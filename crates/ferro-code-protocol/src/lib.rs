//! A small, UI-free Codex transport.
//!
//! The native Rust app-server client is preferred. The installed
//! `codex app-server --stdio` transport remains available as a compatibility
//! fallback when the embedded client cannot be initialized or does not expose
//! every protocol method Ferro Code requires.

use codex_app_server_client::{
    DEFAULT_IN_PROCESS_CHANNEL_CAPACITY, EnvironmentManager, InProcessAppServerClient,
    InProcessClientStartArgs, InProcessServerEvent,
};
use codex_app_server_protocol::{ClientNotification, ClientRequest, JSONRPCErrorError, RequestId};
use codex_arg0::Arg0DispatchPaths;
use codex_config::{CloudConfigBundleLoader, LoaderOverrides};
use codex_core::config::Config;
use codex_feedback::CodexFeedback;
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
    let command = Command::new(program);
    #[cfg(windows)]
    let command = {
        let mut command = command;
        command.creation_flags(CREATE_NO_WINDOW);
        command
    };
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
    uses_cli_fallback: bool,
}

impl CodexBackend {
    pub fn spawn() -> Result<Self, String> {
        match Self::spawn_sdk() {
            Ok(backend) => Ok(backend),
            Err(sdk_error) => Self::spawn_program("codex").map_err(|cli_error| {
                format!(
                    "Could not start the Codex Rust SDK ({sdk_error}); CLI fallback also failed ({cli_error})"
                )
            }),
        }
    }

    fn spawn_sdk() -> Result<Self, String> {
        ensure_sdk_supports_required_methods()?;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("codex-sdk-runtime")
            .thread_stack_size(16 * 1024 * 1024)
            .build()
            .map_err(|error| format!("Could not start the Codex SDK runtime: {error}"))?;
        let client = runtime.block_on(start_sdk_client())?;
        let (out_tx, out_rx) = unbounded::<Value>();
        let (in_tx, in_rx) = unbounded::<Value>();

        thread::Builder::new()
            .name("codex-sdk-client".into())
            .stack_size(16 * 1024 * 1024)
            .spawn(move || runtime.block_on(run_sdk_client(client, out_rx, in_tx)))
            .map_err(|error| format!("Could not start the Codex SDK worker: {error}"))?;

        Ok(Self {
            outgoing: out_tx,
            incoming: in_rx,
            child: Arc::new(Mutex::new(None)),
            uses_cli_fallback: false,
        })
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
            uses_cli_fallback: true,
        })
    }

    pub fn uses_cli_fallback(&self) -> bool {
        self.uses_cli_fallback
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

async fn start_sdk_client() -> Result<InProcessAppServerClient, String> {
    let config = Config::load_with_cli_overrides(Vec::new())
        .await
        .map_err(|error| format!("Could not load Codex configuration: {error}"))?;
    let config_warnings = config
        .startup_warnings
        .iter()
        .map(
            |summary| codex_app_server_protocol::ConfigWarningNotification {
                summary: summary.clone(),
                details: None,
                path: None,
                range: None,
            },
        )
        .collect();

    InProcessAppServerClient::start(InProcessClientStartArgs {
        arg0_paths: Arg0DispatchPaths::default(),
        config: Arc::new(config),
        cli_overrides: Vec::new(),
        loader_overrides: LoaderOverrides::default(),
        strict_config: false,
        cloud_config_bundle: CloudConfigBundleLoader::default(),
        feedback: CodexFeedback::new(),
        log_db: None,
        state_db: None,
        environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
        config_warnings,
        session_source: serde_json::from_value(json!("cli"))
            .map_err(|error| format!("Could not select the Codex SDK session source: {error}"))?,
        enable_codex_api_key_env: false,
        client_name: "ferro-code".to_owned(),
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        experimental_api: true,
        mcp_server_openai_form_elicitation: false,
        opt_out_notification_methods: Vec::new(),
        channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
    })
    .await
    .map_err(|error| format!("Could not initialize the Codex SDK: {error}"))
}

async fn run_sdk_client(
    mut client: InProcessAppServerClient,
    outgoing: Receiver<Value>,
    incoming: Sender<Value>,
) {
    let (async_outgoing_tx, mut async_outgoing_rx) = tokio::sync::mpsc::unbounded_channel();
    if let Err(error) = thread::Builder::new()
        .name("codex-sdk-writer".into())
        .spawn(move || {
            for message in outgoing {
                if async_outgoing_tx.send(message).is_err() {
                    break;
                }
            }
        })
    {
        let _ = incoming.send(json!({"method":"backend/exited","params":{"message":format!("Could not start the Codex SDK writer: {error}")}}));
        let _ = client.shutdown().await;
        return;
    }

    loop {
        tokio::select! {
            message = async_outgoing_rx.recv() => {
                let Some(message) = message else { break };
                if let Err(error) = handle_sdk_message(&client, message, &incoming).await {
                    let _ = incoming.send(json!({"method":"backend/protocolError","params":{"message":error}}));
                }
            }
            event = client.next_event() => {
                let Some(event) = event else {
                    let _ = incoming.send(json!({"method":"backend/exited","params":{"message":"Codex SDK stopped"}}));
                    break;
                };
                let result = match event {
                    InProcessServerEvent::ServerNotification(notification) => serde_json::to_value(notification),
                    InProcessServerEvent::ServerRequest(request) => serde_json::to_value(request),
                    InProcessServerEvent::Lagged { skipped } => {
                        Ok(json!({"method":"backend/protocolError","params":{"message":format!("Codex SDK event queue dropped {skipped} messages")}}))
                    }
                };
                match result {
                    Ok(message) => if incoming.send(message).is_err() { break },
                    Err(error) => {
                        let _ = incoming.send(json!({"method":"backend/protocolError","params":{"message":format!("Could not encode Codex SDK event: {error}")}}));
                    }
                }
            }
        }
    }
    let _ = client.shutdown().await;
}

async fn handle_sdk_message(
    client: &InProcessAppServerClient,
    message: Value,
    incoming: &Sender<Value>,
) -> Result<(), String> {
    let method = message.get("method").and_then(Value::as_str);
    if method == Some("initialize") {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        return incoming
            .send(json!({"id":id,"result":{"userAgent":"Codex Rust SDK","codexHome":"","platformFamily":std::env::consts::FAMILY,"platformOs":std::env::consts::OS}}))
            .map_err(|_| "Codex SDK receiver is closed".to_owned());
    }
    if method == Some("initialized") {
        return Ok(());
    }

    if method.is_some() && message.get("id").is_some() {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let request = serde_json::from_value::<ClientRequest>(message)
            .map_err(|error| format!("Codex SDK does not support this request: {error}"))?;
        let request_handle = client.request_handle();
        let incoming = incoming.clone();
        tokio::spawn(async move {
            let response = match request_handle.request(request).await {
                Ok(Ok(result)) => json!({"id":id,"result":result}),
                Ok(Err(error)) => json!({"id":id,"error":error}),
                Err(error) => {
                    json!({"id":id,"error":{"code":-32000,"message":format!("Codex SDK request failed: {error}")}})
                }
            };
            let _ = incoming.send(response);
        });
        return Ok(());
    }

    if method.is_some() {
        let notification = serde_json::from_value::<ClientNotification>(message)
            .map_err(|error| format!("Codex SDK does not support this notification: {error}"))?;
        return client
            .notify(notification)
            .await
            .map_err(|error| format!("Codex SDK notification failed: {error}"));
    }

    let request_id =
        serde_json::from_value::<RequestId>(message.get("id").cloned().unwrap_or(Value::Null))
            .map_err(|error| format!("Invalid Codex SDK response id: {error}"))?;
    if let Some(result) = message.get("result") {
        client
            .resolve_server_request(request_id, result.clone())
            .await
            .map_err(|error| format!("Could not answer Codex SDK request: {error}"))
    } else {
        let error = serde_json::from_value::<JSONRPCErrorError>(
            message.get("error").cloned().unwrap_or(Value::Null),
        )
        .map_err(|error| format!("Invalid Codex SDK error response: {error}"))?;
        client
            .reject_server_request(request_id, error)
            .await
            .map_err(|error| format!("Could not reject Codex SDK request: {error}"))
    }
}

fn ensure_sdk_supports_required_methods() -> Result<(), String> {
    let requests = [
        json!({"method":"account/read","id":1,"params":{"refreshToken":false}}),
        json!({"method":"account/rateLimits/read","id":1,"params":null}),
        json!({"method":"account/rateLimitResetCredit/consume","id":1,"params":{"creditId":null,"idempotencyKey":"preflight"}}),
        json!({"method":"model/list","id":1,"params":{"limit":100}}),
        json!({"method":"thread/start","id":1,"params":{"cwd":".","ephemeral":true}}),
        json!({"method":"turn/start","id":1,"params":{"threadId":"preflight","input":[{"type":"text","text":"preflight","text_elements":[]}]}}),
        json!({"method":"turn/interrupt","id":1,"params":{"threadId":"preflight","turnId":"preflight"}}),
    ];
    for request in requests {
        let method = request["method"].as_str().unwrap_or("unknown").to_owned();
        serde_json::from_value::<ClientRequest>(request)
            .map_err(|error| format!("required method {method} is unavailable: {error}"))?;
    }
    Ok(())
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
    fn sdk_supports_every_request_used_by_ferro_code() {
        ensure_sdk_supports_required_methods().unwrap();
    }

    #[test]
    #[ignore = "requires configured Codex authentication"]
    fn real_codex_app_server_handshake() {
        let backend = CodexBackend::spawn().expect("start configured Codex backend");
        backend.send(json!({"method":"initialize","id":1,"params":{"clientInfo":{"name":"ferro_code_test","title":"Ferro Code Test","version":"0.2.0"},"capabilities":{"experimentalApi":true,"requestAttestation":false}}})).unwrap();
        assert!(wait_for_id(&backend, 1).get("result").is_some());
        backend.send(json!({"method":"initialized"})).unwrap();
        backend
            .send(json!({"method":"account/read","id":2,"params":{"refreshToken":false}}))
            .unwrap();
        assert!(wait_for_id(&backend, 2).get("result").is_some());
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
