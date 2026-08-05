//! A small, UI-free transport around `codex app-server --stdio`.

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, BufWriter, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn hidden_command(program: &str) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

pub fn encode_message(message: &Value) -> Result<Vec<u8>, serde_json::Error> {
    let mut bytes = serde_json::to_vec(message)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub struct CodexBackend {
    outgoing: Sender<Value>,
    incoming: Receiver<Value>,
    child: Arc<Mutex<Option<Child>>>,
}

impl CodexBackend {
    pub fn spawn() -> Result<Self, String> {
        Self::spawn_program("codex")
    }

    pub fn spawn_program(program: &str) -> Result<Self, String> {
        let mut command = hidden_command(program);
        command
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("Could not start Codex CLI: {error}"))?;
        let stdin = child.stdin.take().ok_or("Codex stdin was unavailable")?;
        let stdout = child.stdout.take().ok_or("Codex stdout was unavailable")?;
        let stderr = child.stderr.take().ok_or("Codex stderr was unavailable")?;
        let child = Arc::new(Mutex::new(Some(child)));
        let (out_tx, out_rx) = unbounded::<Value>();
        let (in_tx, in_rx) = unbounded::<Value>();

        spawn_writer(stdin, out_rx, in_tx.clone())?;
        let reader_tx = in_tx.clone();
        thread::Builder::new().name("codex-app-server-reader".into()).spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) if !line.trim().is_empty() => match serde_json::from_str(&line) {
                        Ok(message) => if reader_tx.send(message).is_err() { break; },
                        Err(error) => { let _ = reader_tx.send(json!({"method":"backend/protocolError","params":{"message":format!("{error}: {line}")}})); }
                    },
                    Ok(_) => {},
                    Err(error) => {
                        let _ = reader_tx.send(json!({"method":"backend/exited","params":{"message":format!("Codex output closed: {error}")}}));
                        break;
                    }
                }
            }
        }).map_err(|error| error.to_string())?;

        thread::Builder::new()
            .name("codex-app-server-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let _ =
                        in_tx.send(json!({"method":"backend/stderr","params":{"message":line}}));
                }
            })
            .map_err(|error| error.to_string())?;

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

fn spawn_writer(
    stdin: ChildStdin,
    outgoing: Receiver<Value>,
    incoming: Sender<Value>,
) -> Result<(), String> {
    thread::Builder::new().name("codex-app-server-writer".into()).spawn(move || {
        let mut writer = BufWriter::new(stdin);
        for message in outgoing {
            let result = encode_message(&message).map_err(|error| error.to_string()).and_then(|bytes| {
                writer.write_all(&bytes).and_then(|_| writer.flush()).map_err(|error| error.to_string())
            });
            if let Err(error) = result {
                let _ = incoming.send(json!({"method":"backend/exited","params":{"message":format!("Could not write to Codex: {error}")}}));
                break;
            }
        }
    }).map(|_| ()).map_err(|error| error.to_string())
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
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn protocol_messages_are_single_jsonl_records() {
        let message = json!({"method":"turn/start","id":7,"params":{"text":"hello\nworld"}});
        let encoded = encode_message(&message).unwrap();
        assert_eq!(encoded.iter().filter(|byte| **byte == b'\n').count(), 1);
        assert_eq!(
            serde_json::from_slice::<Value>(&encoded[..encoded.len() - 1]).unwrap(),
            message
        );
    }

    #[test]
    #[ignore = "requires an installed and configured Codex CLI"]
    fn real_codex_app_server_handshake() {
        let backend = CodexBackend::spawn().expect("start configured Codex CLI");
        backend.send(json!({"method":"initialize","id":1,"params":{"clientInfo":{"name":"codeagent_test","title":"CodeAgent Test","version":"0.2.0"},"capabilities":{"experimentalApi":true,"requestAttestation":false}}})).unwrap();
        assert!(wait_for_id(&backend, 1).get("result").is_some());
    }

    fn wait_for_id(backend: &CodexBackend, expected: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(message) = backend.try_recv()
                && message.get("id").and_then(Value::as_u64) == Some(expected)
            {
                return message;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for Codex response {expected}");
    }
}
