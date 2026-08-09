# Ferro Code

Ferro Code is a lightweight native Windows control surface for the locally installed `codex app-server`. It reuses the Codex CLI's existing ChatGPT subscription or API authentication and stores projects and conversation history locally.

The desktop UI is implemented in [Slint](https://slint.dev/) with its software renderer. There is no Electron shell, browser UI, webview, JavaScript frontend, or hosted middleman.

## Features

- Local project and thread history with staged JSON persistence
- Ephemeral Codex transport threads reconstructed from saved local conversations
- Live assistant messages, reasoning, plans, commands, tools, and file changes
- Model, reasoning-effort, sandbox, and approval controls
- Native command and file-change approvals
- Structured `request_user_input` questions
- Image paste previews and arbitrary file attachments through native Windows pickers
- Stop/interrupt support
- Thread search and removal
- Workspace file list, activity timeline, and Git status inspector
- Codex account, plan, model, and context-window information
- In-app Codex update notifications with one-click installation

## Requirements

- Windows 10 or later
- Rust 1.92 or later
- Codex CLI installed and available as `codex` on `PATH`
- An existing Codex login (`codex login`) or another supported Codex authentication method

## Run

```powershell
cargo run -p ferro-code
```

Build the size-optimized application:

```powershell
cargo build --release -p ferro-code
```

The executable is written to `target\release\ferro-code.exe`. Local state is stored at `%LOCALAPPDATA%\Ferro Code\state.json`.

## Test and lint

The default suite is local and does not contact a model:

```powershell
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The ignored protocol check exercises the configured local Codex installation:

```powershell
cargo test -p ferro-code-protocol real_codex_app_server_handshake -- --ignored
```

For fast iteration on UI-independent behavior, test only the affected crate:

```powershell
cargo test -p ferro-code-core
cargo test -p ferro-code-app
cargo test -p ferro-code-protocol
```

## Workspace architecture

| Crate | Responsibility | UI dependency |
|---|---|---|
| `ferro-code-core` | Domain models, formatting, preferences, and staged persistence | None |
| `ferro-code-protocol` | Hidden Codex child process and JSONL transport | None |
| `ferro-code-app` | Testable application state machine and workspace inspection | None |
| `ferro-code` | Slint components, native dialogs, and state-to-view projection | Slint |

The split keeps protocol and domain changes out of the Slint build graph, gives each layer a narrow API, and permits fast crate-scoped tests. See [docs/refactor-notes.md](docs/refactor-notes.md) for the measured baseline and results.
