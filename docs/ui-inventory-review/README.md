# Inventory and operational-screen review

These captures are historical validation evidence and retain their original output; they are not current Scala branding.
Implemented on `fix/runtime-settings-inheritance`, starting from `2b8698ed276bca8ac7d6da4afee9ba89925bbdbc` with a clean checkout. Changes are confined to the TUI and directly related documentation. No serving adapter, persistence resolver, artifact, manifest, binary or lineage was changed.

Models, Discover, installed runtimes and runtime dialogs now use shared column geometry and compact rows. Selected-item readers retain full identity, provenance and diagnostics. Installed filtering is local; Discover format filtering no longer initiates an upstream search. Downloads have a bounded summary and a focused, navigable job view. Overview retains request activity and load progress; Server associates operational fields per backend. Logs explicitly distinguish follow mode from paused history and expose complete multiline messages. Help uses aligned sections and documents the implemented keys.

`D` opens a scrollable reader; Escape returns without changing selection. Settings and Model Profiles retain their layouts and typed editors, including the existing profile `D` duplicate shortcut. Selection and destructive confirmation targets survive list reordering by identity. Failed refreshes preserve previously available inventory. At 46×13 the inactive command region uses one line; the Settings/Profile shell geometry is unchanged.

## Render and interaction review

A temporary offline Rust driver called the production `ui::render` using Ratatui's in-memory terminal backend, then exported the actual cell buffers. These are rendered widgets, not hand-written mockups. Text captures omit colours and style attributes. The driver and its temporary dependency were removed; no test code or screenshot framework was added.

Every changed tab, runtime search and model-runtime selection was rendered at **46×13, 100×30 and 160×45**. Populated fixtures included nine models with Unicode/long names and paths, three runtime versions, a format default different from the running runtime, compatibility/attention/incompatibility states, three download phases, and three resident backends including generation activity and a measurable load. Missing/loading/empty/failure states and ASCII/no-colour rendering were also inspected. Both reference screens and their numeric/boolean editors were rendered at all three sizes.

Input was delivered through the existing key/mouse handlers, without executing their queued operations:

- Local name/path filtering, paste, clear, row selection, resize and scrollable details/back.
- Model and runtime reordering preserved IDs. A confirmation armed before reordering still queued removal of exactly `artifact-8` / the originally selected concrete runtime.
- Row clicks queued no model-library operation. Discover format filtering queued no upstream operation.
- Mouse entry into Downloads, navigation to paused/failed jobs, complete failure text, and retention of terminal-job selection across refresh.
- Escape from a runtime reader restored its selection dialog. Profile `D` still opened duplication, not the new reader.
- A new log entry left the paused history end at entry 31; End restored follow mode.

These are **isolated render/input checks, not live backend validation**. No runtime was launched, downloaded, installed, updated or removed; no live profile or runtime selection was changed. Provider availability and real installation/build progress remain unverified in this environment.

## Representative captures

- [Models, minimum size](models-46x13.txt), [Discover](discover-100x30.txt), [Runtimes](runtimes-100x30.txt).
- [Overview with request activity](overview-100x30.txt), [Server with three associated backend rows](server-160x45.txt).
- [Focused downloads](downloads-46x13.txt), [complete failure diagnostic after scrolling](diagnostic-46x13.txt), [runtime search](runtime-search-100x30.txt).
- [Help](help-100x30.txt), [reference numeric/boolean editors at minimum size](editors-46x13.txt).

## Repository validation

All final commands passed:

```text
cargo fmt --all -- --check
cargo check --workspace --offline
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo test --workspace --offline
git diff --check
```

Existing tests: **214 passed, 0 failed, 1 ignored**. The ignored test is the existing manual local-file hash-throughput benchmark. The temporary render command was `cargo run -p scala-tui --example visual_review --offline`; it completed successfully before the driver was removed.
