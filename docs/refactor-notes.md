# Slint refactor notes

Measurements were taken on the same Windows machine on 2026-08-05. Process samples were read four seconds after launch with the local Codex backend connected. Build timings are wall-clock observations and are sensitive to cache state; binary and process-memory measurements are the more stable comparisons.

## Before: egui/eframe

- One package with a 4,443-line `app.rs`; state transitions, persistence, protocol handling, rendering, native dialogs, and formatting were coupled together.
- Dark three-column desktop layout: project/thread navigation on the left, conversation and composer in the center, and an optional workspace inspector on the right.
- UI used an OpenGL renderer. Windows Graphics Capture returned a black client surface in this environment, though the process was responsive.
- 18 local tests passed and two live-Codex tests were ignored.
- Warm `cargo test --all-targets`: 17.96 seconds.
- Optimized executable: 9,772,544 bytes.
- Four-second process sample: 78,716,928-byte working set, 105,267,200-byte private memory, 2.375 CPU seconds, 357 handles, 16 threads.

## After: Slint workspace

- Four crates isolate durable models, Codex transport, the application state machine, and the desktop view.
- The Slint UI retains the three-column visual hierarchy and dark palette while using virtualized lists for threads, messages, files, and activity.
- The software renderer avoids requiring a GPU/OpenGL context. During verification, the optional accessibility build exposed the complete live control tree, including connected account status, navigation, welcome state, composer, and all selectors. The environment's capture helper still returned a black raster surface for native windows, so semantic inspection was used for layout verification.
- 16 local tests pass across the new crates, with one ignored live-Codex handshake. Tests cover persistence, history validation, restored conversation context, Unicode formatting, plan parsing, project/thread lifecycle, streamed deltas, completed activities, approval routing, and JSONL framing.
- Incremental full-workspace test rebuild after the split: 13.18 seconds (26.6% faster). Crate-scoped domain/state tests avoid compiling or linking the Slint desktop.
- Incremental optimized desktop rebuild: 82.02 seconds (11.6% faster than the measured egui release rebuild).
- Optimized executable: 9,283,072 bytes, 489,472 bytes (5.0%) smaller.
- Four-second process sample: 31,354,880-byte working set (60.2% lower), 8,081,408-byte private memory (92.3% lower), 0.109375 CPU seconds, 250 handles, 15 threads.
- A from-scratch/full-profile release build remains dominated by GUI dependencies and full LTO. The compilation improvement is primarily in normal development: isolated crates, narrower invalidation, faster workspace tests, and UI-independent crate checks.

## Design choices

- Slint is pinned to `1.17.1` and built with only `std`, `backend-winit`, `renderer-software`, and compatibility features.
- Release builds use `opt-level = "z"`, one codegen unit, symbol stripping, aborting panics, and full LTO to keep the distributed executable smaller.
- Development builds retain line information for workspace crates but omit debug symbols from third-party dependencies. This keeps application debugging useful while reducing dependency compile work and target-directory growth.
- UI models use `ListView` so large histories do not instantiate every row.
- Persistence writes a temporary JSON document before replacing the state file, keeping partial serialization away from the durable path.
- The controller polls the non-blocking transport every 75 ms; no renderer work occurs when state has not changed.
