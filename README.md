# CodeAgent

CodeAgent is a native Windows agent-harness control surface written entirely in Rust. It talks to the locally installed `codex app-server`, so it reuses the Codex CLI's existing ChatGPT subscription or API authentication instead of asking for another key.

There is no Electron shell, browser UI, webview, JavaScript frontend, or hosted middleman. The interface is rendered in a native desktop window with `eframe`/`egui`, while native Windows file and folder pickers are provided by `rfd`.

## Features

- App-owned project and thread history, persisted locally by CodeAgent
- Project folders used as the working directory and workspace-access boundary
- Ephemeral Codex transport threads, keeping CodeAgent conversations out of Codex history
- Live agent-message, reasoning, plan, command, tool, and file-change streams
- Live current-thread context-window usage beside the composer send control
- Native command/file approval dialogs with once/session/deny decisions
- Structured `request_user_input` dialogs
- Model and reasoning-effort discovery from the installed Codex version
- Codex plan usage, remaining rolling-window allowance, and reset times
- Usage-reset inventory with expiry details and confirmed reset redemption
- Read-only, workspace-write, and full-access sandbox choices
- Configurable approval policy
- Image attachments using local-file inputs
- Stop/interrupt support
- Per-project thread search and local thread removal
- Workspace file inspector, activity timeline, and Git change summary
- Persisted workspace and agent preferences
- Existing ChatGPT subscription/account status in the title bar

## Requirements

- Windows 10 or later
- Rust 1.85 or later
- Codex CLI installed, configured, and available as `codex` on `PATH`
- An existing Codex login (`codex login`) or other supported Codex authentication

## Run

```powershell
cargo run
```

Build the optimized application:

```powershell
cargo build --release
```

The executable is written to `target\release\codeagent.exe`.

## Test

The default test suite is local and does not contact a model:

```powershell
cargo test --all-targets
```

Two ignored end-to-end checks exercise the configured local Codex installation. The turn test creates an ephemeral, read-only thread and consumes one short model response:

```powershell
cargo test real_codex_app_server_handshake -- --ignored
cargo test real_codex_ephemeral_turn_streams -- --ignored
```

## Architecture

`src/backend.rs` owns a hidden `codex app-server --stdio` child process. Dedicated reader, writer, and diagnostics threads carry newline-delimited JSON-RPC without blocking the UI. `src/app.rs` maps app-server requests, responses, and streaming notifications into the native control surface. Projects, threads, and their messages are stored in CodeAgent's own persisted application data. Codex threads are started with `ephemeral: true`; reopening a local thread reconstructs model context from the saved CodeAgent transcript. `src/model.rs` contains durable preferences and UI data models, and `src/theme.rs` defines the desktop visual system.

The app-server protocol is versioned with the installed Codex CLI. CodeAgent deliberately discovers models and account state at runtime and handles unknown nonessential notifications defensively.
