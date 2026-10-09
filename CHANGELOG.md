# Changelog

## 0.1.12 - Model-agnostic reasoning controls

Improves OpenAI-compatible Chat reasoning controls across Scala engines while
preserving runtime-owned capabilities, Model Profiles and explicit request precedence.

- Accept the validated `thinking.type=enabled|disabled` request shape used by
  Unsloth Studio; ON inherits the selected model's supported effort and OFF
  suppresses inherited reasoning without changing persistent settings.
- Reject conflicting reasoning controls and unsupported per-request thinking
  rather than silently changing a model's effort or process launch settings.
- Preserve qualified llama.cpp and q27 native `reasoning_effort` requests
  independently of optional model-capability discovery; keep q27's exact native
  per-request thinking gate and reject unsupported aliases.
- Track genuine NInfer reasoning stream phases and expose native reasoning text
  only with an explicit streaming opt-in; default Chat and Responses behavior
  keeps reasoning content private.
- Document the remaining Unsloth Custom Connection limitation: its normal
  relay does not yet support a privacy-preserving thinking-duration indicator.

## 0.1.11 - Runtime acquisition rate-limit hardening

Reduces GitHub REST consumption during managed runtime discovery and installation
without weakening Scala's runtime identity, compatibility or provenance checks.

- Download official runtime archives through their validated GitHub release URLs
  instead of the REST release-asset endpoint, while preserving repository/tag/
  asset binding, advertised size checks and mandatory SHA-256 verification.
- Carry a live provider-verified binary candidate into installation so the same
  release is not immediately revalidated a second time; source-build validation
  remains independently exact.
- Coalesce repeated release-list metadata reads, extend ordinary catalogue
  freshness to one hour and honor GitHub rate-limit cooldown/reset information
  while retaining stale provider caches and optional token support.
- Limit q27 source inspection to the explicitly reviewed v0.10.0 and v0.14.3
  source contracts while preserving qualified official binary discovery.

## 0.1.10 - Native llama.cpp Decision serving

Completes the native Decision path against official llama.cpp System One while
preserving Scala's exact runtime/model/settings qualification boundary.

- Qualify native llama.cpp /v1/systemone support for exact loaded Decision
  runtime/model/settings tuples without granting capabilities from GGUF format,
  filenames, provenance or version strings alone.
- Expose a separate decision_candidate discovery fact so unloaded Decision
  profiles remain selectable and can JIT-load after Scala restart or eviction,
  while decision remains execution-qualified only.
- Preserve Decision-only serving semantics so classifier GGUFs do not acquire
  ordinary chat, Responses, streaming or embedding capability.
- Translate omitted generic instructions only at the private llama.cpp boundary
  and enforce llama.cpp's native 2-10 score-level limit before backend execution.
- Update the managed llama.cpp source catalogue to admit the current official
  System One-capable nightly source contract without changing existing runtime
  identities or automatically selecting/installing a runtime.

## 0.1.9 - Runtime qualification updates

Updates q27 and NInfer runtime admission while preserving Scala's strict
artifact/engine/runtime/settings/profile boundaries.

- Qualify q27 v0.14.3 from exact immutable source/release evidence and add the
  distinct sm86-only `q27-server-12g` Bonsai 2 route with current host
  requirements.
- Keep q27 reduced-vocabulary MTP controls unsupported by default where their
  native cross-constraints cannot be represented safely as ordinary settings.
- Re-audit canonical NInfer master, recognize its v3-only container transition,
  and keep Scala's managed Linux route pinned to the reviewed v2 source contract.
- Withhold NInfer capability domains whose successor ownership is not proven;
  provenance or repository identity alone never grants serving capabilities.

## 0.1.8 - Native decision models

Adds the native decision-model path while preserving Scala's origin-neutral
runtime, engine and Model Profile boundaries.

- Add bounded native decision capability admission derived from the qualified
  engine/runtime/model combination rather than artifact origin or naming.
- Add the /v1/systemone API route for native structured decision requests and
  preserve request state/question shapes without generation emulation.
- Keep llama.cpp, q27 and NInfer decision support independently qualified:
  unsupported runtime/model pairs fail closed and never fall back to chat or
  another engine.
- Preserve ordinary serving behavior and existing runtime/model update ownership;
  decision capability adds no hidden defaults or Norted-specific privilege.

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
