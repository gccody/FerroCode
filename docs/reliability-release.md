# Reliability and continuity release

Implements audit roadmap items 1, 2, 4, and 5 from `audit-2026-09-12.md`. The broader Git review-screen roadmap remains separate; the two commit correctness bugs and filename parsing bug are fixed here.

## Audit fixes

| Findings | Implementation |
| --- | --- |
| 1 | Turn overrides are converted into the pinned protocol's typed `SandboxPolicy`, including summary turns. |
| 2 | Approvals retain their owning local thread and identify its project, including background requests. |
| 3–4 | Invalid history cannot be replaced by a default autosave. Synced atomic replacement, validated recovery generations, preservation of damaged files, and a lifetime writer lock protect history. |
| 5 | Project changes synchronize the active conversation first. |
| 6, 17 | Request generations, terminal turn IDs, and queued cancellation handle reordered responses and Stop during startup. Stale interrupts cannot finish the next turn. |
| 7 | CI and the checked-in toolchain use Rust 1.97.1, above the workspace minimum. |
| 8–9 | CLI EOF and disconnected channels terminate the backend generation. String-valued server request IDs are retained and answered. |
| 10 | Polling compares against the previous rendered revision, so question edits reach the UI. |
| 11–12 | Imported and pasted files live in managed storage; restored conversations resend available attachments. Local identifiers use UUIDs. |
| 13–14 | Subfolder commits are rejected before staging. Summaries use the captured staged tree, including new files. Changes to the index or HEAD during summary generation abort the commit. |
| 15 | Project identity is case-sensitive outside Windows. |
| 16 | Command failures, exit codes, and tool results/errors are retained and failure statuses are displayed. |
| 18 | Git status and file lists use NUL-delimited records, including rename sources and Unicode names. |
| 19 | Rate-limit errors and disconnects clear loading flags; Settings displays loading and error states. |

## Requested improvements

- **Reliability:** injectable transport and local event-replay tests; startup/request deadlines; summary timeout; cleanup of runtime mappings, pending approvals/questions, and interrupted items on disconnect.
- **Durable storage:** a bounded background writer serializes atomic snapshot transactions. A schema envelope accepts older unversioned state and rejects unsupported future versions. Backups include relative attachment references and a companion directory. Restore preserves previous data. Archived conversations remain available through Undo archive.
- **Recovery and diagnostics:** Settings exposes Reconnect, Restore last prompt, backup export/restore, and diagnostic export. The connection label identifies the embedded SDK revision or CLI backend. Diagnostics omit credentials, account identifiers, conversation content, and command output.
- **Continuity and UI:** draft text and attachments are scoped to each thread or project. File copying, pasted-image encoding, thumbnail decoding, and Git summary preparation run on workers. Markdown retention is bounded; preview memory is capped at 128 entries per conversation. Wrapped user messages, prose, headings, quotes, code, and expanded activity use renderer measurements at their available widths. Save failures remain visible outside Settings.

## Validation and limits

Final Windows verification: `cargo test --workspace --all-targets --locked` passed 115 tests (31 desktop, 56 application, 24 core, 4 protocol); one authenticated handshake test was intentionally ignored. `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed. Cargo still reports the existing future-compatibility notice for the transitive `proc-macro-error2 2.0.1` dependency.

The local suite covers turn reordering, stale interrupts, startup cancellation, disconnect cleanup, approval ownership, loading-state errors, history corruption and recovery, multiple writers, failed replacement, moved backup attachments, draft/archive persistence, Git scope, staged snapshots, index changes, and unusual filenames. These checks require no model calls. Run the workspace tests, strict Clippy, and formatting check from README to reproduce validation.

Native macOS/Linux execution and authenticated model behavior require their respective environments; the authenticated handshake remains an explicit ignored test. Git hooks retain their normal behavior and can modify the index during `git commit`; external Git operations should not be run concurrently with a commit. Backup attachment directories are retained rather than automatically garbage-collected, avoiding deletion of files referenced by recovery generations. Very large conversations still use a bounded textual transcript when reconstructing an ephemeral backend thread.
