use crate::child_process::hidden_command;
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, BufWriter, Write},
    process::{Child, ChildStdin, Stdio},
    sync::{Arc, Mutex},
    thread,
};

pub struct CodexBackend {
    outgoing: Sender<Value>,
    incoming: Receiver<Value>,
    child: Arc<Mutex<Option<Child>>>,
}

impl CodexBackend {
    pub fn spawn() -> Result<Self, String> {
        let mut command = hidden_command("codex");
        command
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| format!("Could not start Codex CLI: {e}"))?;
        let stdin = child.stdin.take().ok_or("Codex stdin was unavailable")?;
        let stdout = child.stdout.take().ok_or("Codex stdout was unavailable")?;
        let stderr = child.stderr.take().ok_or("Codex stderr was unavailable")?;
        let child = Arc::new(Mutex::new(Some(child)));
        let (out_tx, out_rx) = unbounded::<Value>();
        let (in_tx, in_rx) = unbounded::<Value>();

        spawn_writer(stdin, out_rx, in_tx.clone());
        let reader_tx = in_tx.clone();
        thread::Builder::new()
            .name("codex-app-server-reader".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    match line {
                        Ok(line) if !line.trim().is_empty() => match serde_json::from_str(&line) {
                            Ok(message) => {
                                if reader_tx.send(message).is_err() {
                                    break;
                                }
                            }
                            Err(err) => {
                                let _ = reader_tx.send(json!({
                                    "method":"backend/protocolError",
                                    "params":{"message":format!("{err}: {line}")}
                                }));
                            }
                        },
                        Ok(_) => {}
                        Err(err) => {
                            let _ = reader_tx.send(json!({
                                "method":"backend/exited",
                                "params":{"message":format!("Codex output closed: {err}")}
                            }));
                            break;
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        let stderr_tx = in_tx;
        thread::Builder::new()
            .name("codex-app-server-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let _ = stderr_tx.send(json!({
                        "method":"backend/stderr",
                        "params":{"message":line}
                    }));
                }
            })
            .map_err(|e| e.to_string())?;

        Ok(Self {
            outgoing: out_tx,
            incoming: in_rx,
            child,
        })
    }

    pub fn send(&self, message: Value) -> Result<(), String> {
        self.outgoing
            .send(message)
            .map_err(|_| "Codex app-server is not running".to_owned())
    }

    pub fn try_recv(&self) -> Option<Value> {
        self.incoming.try_recv().ok()
    }
}

fn spawn_writer(stdin: ChildStdin, outgoing: Receiver<Value>, incoming: Sender<Value>) {
    thread::Builder::new()
        .name("codex-app-server-writer".into())
        .spawn(move || {
            let mut writer = BufWriter::new(stdin);
            for message in outgoing {
                let result: Result<(), String> = (|| {
                    serde_json::to_writer(&mut writer, &message).map_err(|e| e.to_string())?;
                    writer.write_all(b"\n").map_err(|e| e.to_string())?;
                    writer.flush().map_err(|e| e.to_string())?;
                    Ok(())
                })();
                if let Err(err) = result {
                    let _ = incoming.send(json!({
                        "method":"backend/exited",
                        "params":{"message":format!("Could not write to Codex: {err}")}
                    }));
                    break;
                }
            }
        })
        .expect("spawn Codex writer thread");
}

impl Drop for CodexBackend {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.child.lock()
            && let Some(child) = guard.as_mut()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CodexBackend;
    use serde_json::json;
    use std::time::{Duration, Instant};

    #[test]
    fn protocol_messages_are_jsonl_safe() {
        let msg = json!({"method":"turn/start","id":7,"params":{"input":[{"type":"text","text":"hello\nworld","text_elements":[]}]}});
        let encoded = serde_json::to_string(&msg).unwrap();
        assert_eq!(encoded.lines().count(), 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
            msg
        );
    }

    #[test]
    #[ignore = "requires an installed and configured Codex CLI"]
    fn real_codex_app_server_handshake() {
        let backend = CodexBackend::spawn().expect("start configured Codex CLI");
        backend
            .send(json!({
                "method":"initialize",
                "id":1,
                "params":{
                    "clientInfo":{"name":"codeagent_test","title":"CodeAgent Test","version":"0.1.0"},
                    "capabilities":{"experimentalApi":true,"requestAttestation":false}
                }
            }))
            .unwrap();
        let initialized = wait_for_id(&backend, 1);
        assert!(initialized.get("result").is_some(), "{initialized}");

        backend.send(json!({"method":"initialized"})).unwrap();
        backend
            .send(json!({
                "method":"account/read","id":2,"params":{"refreshToken":false}
            }))
            .unwrap();
        let account = wait_for_id(&backend, 2);
        backend
            .send(json!({"method":"model/list","id":3,"params":{"limit":10}}))
            .unwrap();
        let models = wait_for_id(&backend, 3);
        backend
            .send(json!({"method":"account/rateLimits/read","id":4,"params":null}))
            .unwrap();
        let rate_limits = wait_for_id(&backend, 4);
        assert!(account.pointer("/result/account").is_some(), "{account}");
        assert!(
            models
                .pointer("/result/data")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|models| !models.is_empty()),
            "{models}"
        );
        assert!(
            rate_limits.pointer("/result/rateLimits").is_some(),
            "{rate_limits}"
        );
    }

    #[test]
    #[ignore = "uses the configured Codex subscription for one ephemeral turn"]
    fn real_codex_ephemeral_turn_streams() {
        let backend = CodexBackend::spawn().expect("start configured Codex CLI");
        backend
            .send(json!({
                "method":"initialize",
                "id":1,
                "params":{
                    "clientInfo":{"name":"codeagent_test","title":"CodeAgent Test","version":"0.1.0"},
                    "capabilities":{"experimentalApi":true,"requestAttestation":false}
                }
            }))
            .unwrap();
        assert!(wait_for_id(&backend, 1).get("result").is_some());
        backend.send(json!({"method":"initialized"})).unwrap();
        backend
            .send(json!({
                "method":"thread/start",
                "id":2,
                "params":{
                    "cwd":std::env::current_dir().unwrap().to_string_lossy(),
                    "ephemeral":true,
                    "approvalPolicy":"never",
                    "sandbox":"read-only"
                }
            }))
            .unwrap();
        let thread = wait_for_id(&backend, 2);
        let thread_id = thread
            .pointer("/result/thread/id")
            .and_then(serde_json::Value::as_str)
            .expect("thread id")
            .to_owned();
        backend
            .send(json!({
                "method":"turn/start",
                "id":3,
                "params":{
                    "threadId":thread_id,
                    "input":[{"type":"text","text":"Reply with exactly: CODEAGENT_SMOKE_OK","text_elements":[]}],
                    "effort":"low"
                }
            }))
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(90);
        let mut streamed = String::new();
        let mut completed = false;
        while Instant::now() < deadline {
            if let Some(message) = backend.try_recv() {
                if message.get("id").and_then(serde_json::Value::as_u64) == Some(3)
                    && message.get("error").is_some()
                {
                    panic!("turn/start failed: {message}");
                }
                match message.get("method").and_then(serde_json::Value::as_str) {
                    Some("item/agentMessage/delta") => streamed.push_str(
                        message
                            .pointer("/params/delta")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default(),
                    ),
                    Some("item/completed") => {
                        if message
                            .pointer("/params/item/type")
                            .and_then(serde_json::Value::as_str)
                            == Some("agentMessage")
                            && let Some(text) = message
                                .pointer("/params/item/text")
                                .and_then(serde_json::Value::as_str)
                        {
                            streamed = text.to_owned();
                        }
                    }
                    Some("turn/completed") => {
                        let status = message
                            .pointer("/params/turn/status")
                            .and_then(serde_json::Value::as_str);
                        assert_eq!(status, Some("completed"), "{message}");
                        completed = true;
                        break;
                    }
                    _ => {}
                }
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(
            completed,
            "Codex turn did not complete; streamed: {streamed}"
        );
        assert!(streamed.contains("CODEAGENT_SMOKE_OK"), "{streamed}");
    }

    fn wait_for_id(backend: &CodexBackend, expected: u64) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(message) = backend.try_recv()
                && message.get("id").and_then(serde_json::Value::as_u64) == Some(expected)
            {
                return message;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for Codex response {expected}");
    }
}
