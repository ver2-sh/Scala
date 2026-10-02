# Changelog

## 0.1.6 - Model capabilities and update restart reliability

Adds read-only request-capability discovery for model profiles while improving
application update recovery and restart behavior.

- Expose optional model thinking capabilities through /v1/models without
  persisting runtime observations or changing inference defaults.
- Qualify NInfer thinking switches and Low/Medium/XHigh effort support from the
  exact runtime and embedded template semantics; keep q27 capability reporting
  bounded to its reviewed request contract.
- Keep capability discovery origin-neutral so equivalent artifacts receive the
  same serving behavior regardless of provenance.
- Restart Scala automatically after interactive application updates and improve
  Windows cleanup/error handling during replacement.

## 0.1.5 - Windows child-process isolation

Prevents Scala-owned noninteractive child processes from mutating the parent
Windows console while preserving existing runtime ownership and captured output.

- Launch managed runtimes, probes and helper processes with `CREATE_NO_WINDOW`
  on Windows so console-global changes in child executables cannot alter Scala's
  TUI console state.
- Preserve piped stdout/stderr, runtime lifecycle ownership and existing
  termination behavior while keeping non-Windows process behavior unchanged.
- Apply the same isolation policy to runtime discovery, installer/toolchain
  probes, NInfer source inspection, startup helpers and doctor probes.
- Avoid unsupported parent-directory sync after benchmark checkpoint
  finalization on non-Unix platforms while retaining the Unix durability step.

## 0.1.4 — Windows startup and terminal reliability

Improves Windows desktop behavior while keeping Scala's serving, runtime and
settings ownership unchanged.

- Run login-started Scala as a background process without leaving a persistent
  console window, while normal CLI and TUI invocations keep their console.
- Harden Windows login startup with SID-based task ownership, an explicit
  delayed AtLogOn trigger, missed-start catch-up and bounded early-login retries.
- Validate the full owned Task Scheduler definition, including principal,
  action/arguments, trigger state/delay, restart policy, execution limit and
  stored battery settings.
- Use a Windows-safe glyph set in the TUI to avoid fallback-width rendering
  problems on common console fonts.
- Keep Windows NInfer startup-log cleanup dependency-free and compatible with
  native package execution.

## 0.1.3 — Native Windows NInfer

Adds a managed native-Windows NInfer route while preserving Scala's existing
engine/runtime separation and origin-neutral artifact admission.

- Add a separate `natpate/ninfer-windows` provider for the exact reviewed
  portable Windows x86_64 CUDA package; no WSL is required.
- Pin the reviewed v0.7.1 release to its exact source tag/tree, GitHub asset
  identity, size and SHA-256 so moved/replaced packages fail closed.
- Keep canonical `Neroued/ninfer` managed source builds Linux-only and keep the
  Windows/Linux runtime update lines independent.
- Bound managed Windows support to the reviewed NInfer v2 package; v0.8+ / v3
  releases remain unavailable until Scala has a separately reviewed v3 contract.
- Preserve the portable ZIP layout and adjacent DLLs while safely normalizing
  Windows archive separators before traversal/containment checks.

## 0.1.2 — Login startup control

Adds first-class, per-user OS login startup management without changing Scala's
inference-settings ownership or current-process lifecycle.

- Add **Settings → Start automatically on login** backed directly by the native
  OS registration rather than settings.json.
- Add scriptable scala startup status|enable|disable; Scala starts scala serve
  at the next login when enabled.
- Use per-user systemd on Linux, LaunchAgent on macOS, and Task Scheduler on
  Windows without administrator elevation.
- Keep enable/disable registration-only: toggling startup never starts, stops,
  or restarts the currently running Scala server.
- Isolate Windows tasks per user with a SID-derived identity and validate the
  exact owned task definition, including enabled state, current-user
  interactive/limited principal, exact executable/arguments, one action, and
  one AtLogOn trigger.

## 0.1.1 — Scala Link compatibility

Maintenance release restoring zero-configuration Scala Link compatibility with
current Wayfinder Sync Chain installations.

- Discover Wayfinder through deterministic OS-native local IPC rather than any
  executable/install path: native Windows named pipe and safe Linux runtime
  socket candidates.
- Reconnect and re-register automatically when Wayfinder is started or restarted.
- Report absent/unavailable Wayfinder local transport cleanly instead of exposing
  raw filesystem errors.
- Preserve existing Scala Link protocol, profile/runtime ownership, local serving
  behavior and explicit Link enablement policy.

## 0.1.0 — Scala

First public Scala developer release. The application and command are
`scala`, with Scala-native crates, storage, service, API identity and release assets.

- Apache-2.0 project licensing, third-party attribution and bundled MPL sources.
- rustls 0.23.45 resolves RUSTSEC-2026-0285; smartstring's upstream maintenance
  warning remains tracked without suppression.
- Generated PowerShell installation verifies the selected archive's embedded
  SHA-256 before extraction, preserving cargo-dist installation semantics.
- Terminal-first local inference server with an authenticated OpenAI-compatible API.
- Independently installed llama.cpp, NInfer and q27 runtimes, with existing
  capability admission and Model Profile settings precedence.
- Managed model discovery/download/import and optional Wayfinder-backed Scala Link.
- Server-only archives for Linux x64/ARM64, macOS Intel/Apple Silicon and Windows
  x64, shell/PowerShell installers, and generated SHA-256 checksums.
- Native `scala update --check` and explicitly approved `scala update` / `--yes`,
  with cargo-dist receipt ownership and active-session replacement protection.
- Daily nonblocking TUI update checks and a `/update` command, with cached failures
  and machine-readable CLI output. Application updates preserve model/runtime data.

Models, inference runtimes, GPU drivers and credentials are not included. A server
binary does not promise every inference runtime supports the same platform.
Norted-built and equivalent ordinary artifacts retain identical serving semantics.

Public installers and versioned assets are available through `https://ver2.sh/scala/`.
Native macOS/Windows validation and signing readiness remain outside this developer
release's validation scope. See
[release maintenance](docs/releases.md) for verification and publication gates.
