# Changelog

## 0.1.27 - Independent Model Profile runtime binding

Add persistent per-profile runtime selection independent of model artifacts.
Multiple profiles can now share one model file while selecting different
installed runtimes.

Preserve explicit load overrides, per-profile settings, runtime provenance,
compatibility validation, runtime removal protection, and inheritance.
Expose runtime selection through the CLI and TUI.

Existing unpinned profiles retain their previous behavior. No models,
runtimes, or user settings are automatically migrated.


## Unreleased

Add exact runtime pins to Model Profiles without duplicating model artifacts. Profile runtime
selection now reaches load/JIT, settings inspection, capability discovery, and provenance; load
overrides remain ephemeral. CLI `set-runtime`/`clear-runtime` and the profile-scoped TUI picker
share validation and atomic persistence. Inherit preserves artifact/format defaults. Missing or
incompatible pins fail explicitly. Runtime inventory, removal, and normal prune account for
profile references. Existing unpinned profile records and hashes remain unchanged.

## 0.1.26 - Native v3 draft admission and Windows runtime launch

Accept valid native DFlash/DFlash2 v3 component bindings using the target model's
hidden width and nested draft configuration, while preserving strict component,
tensor, shape and resource validation. No model data or templates are rewritten.

Launch long Windows runtime paths through a process-scoped directory junction to
the same verified executable and adjacent DLLs. Preserve installed paths, package
integrity, runtime identity, engine behavior and provenance. No runtime files are
copied; the alias is removed after exit. Ordinary paths and non-Windows launches
are unchanged. Heap-box CLI command dispatch and Model Profile operations to avoid
Windows debug-stack exhaustion.

Install verified portable Windows runtime packages with nested executable
paths using portable manifest-relative names, keeping archive containment,
checksums and runtime identity checks intact. Compare the observed NVIDIA
driver against the CUDA 13.1 R590 branch requirement instead of treating
driver compatibility as always unknown; previously installed reviewed packages are requalified at read time without rewriting their manifests. Unavailable and outdated drivers
remain distinct from a positively verified prerequisite.

Local validation includes native v3 artifact admission, Windows alias identity and
cleanup regressions, plus successful 500,000-token NInfer-all startup and coding,
function-calling and image requests through Scala on an RTX 5090. The tested host
profile is user-owned; this release creates no inference presets, converts no
models and makes no claim of benchmark-equivalent quality across a full context.

## 0.1.25 - NInfer-all managed installation and serving

Runtime acquisition remains in Scala: use qualified upstream or alternative
release assets where available, otherwise build the selected pinned source
locally on Install. No Scala-prepackaged inference runtime is introduced.

Heap-box native operation futures to prevent Windows stack exhaustion after
dependency provisioning. Share verified downloads with a 2 GiB archive limit,
reuse compatible toolchains across runtime/provisioner revisions, retain one
merged CUDA tree including NVTX headers, and dispose temporary compilation
objects and dependency build trees on completion or ordinary failure.

Live Windows install testing found and fixed a stack overflow in the CLI, unsafe
cache-control file assumptions, a missing isolated PATHEXT setting, an obsolete
Visual Studio manifest hash/size, required Microsoft channel arguments and an
anonymous GitHub REST rate-limit dependency. For this exact source recipe,
Scala verifies the pinned Git commit/tree locally instead of relying on a
separate rate-limited REST metadata call; ordinary runtime providers retain
their existing release admission policy.

Add independently pinned NInfer-all Windows and Linux source candidates under
the NInfer engine, with separate source, build and executable provenance.

The Windows Install action now provisions its complete software build environment:
private pinned Git, CMake, Ninja and CUDA components; pinned vcpkg FFmpeg/curl and
transitive dependencies; and compatible MSVC/Windows SDK reuse or the verified
Microsoft installer. It captures the developer environment automatically, binds
exact tools, reports Windows approval/restart requirements, reuses verified
cache data and packages runtime DLLs before atomic activation. Dependency recipe
bytes are part of the Windows runtime identity. No manual tool setup or global
PATH/CUDA edits are required. Catalog browsing remains read-only. The Linux
recipe, Python installers and existing portable runtime behavior are unchanged.

Qualify schema-28 startup and Chat/streaming contracts with independent native
source evidence and synthetic HTTP/SSE fixtures. Expose YaRN, fixed YaRN factor,
FP16 GDN state and rotated KV formats for this runtime only. Verify context,
page-rounded KV capacity, GPU/artifact identity, speculation, vision and explicit
settings. Preserve conditional output/sampler defaults without persisting runtime
observations. Do not borrow schema-25 strict-output guarantees.

Preserve native v2/v3 isolation and existing model/runtime/profile state. No
model conversion, inference, training, benchmark, driver replacement, production
activation or release is performed by this source change. A complete fresh
Windows dependency installation/build and 500K memory fit remain distinct from
synthetic validation. Preserve exact source checkout bytes, provider-owned CMake
digests and cross-platform source compiler-path validation.

## 0.1.24 - Native D3 image decisions in llama.cpp

Qualify native D3 image decisions through upstream llama.cpp's `/v1/systemone`
for the exact reviewed b11606 and b11617 runtime revisions. Models with native
`vllm-d3` decision identity can now expose image input when their bound projector,
verified executable, native Decision route, loaded model and observed vision
support all qualify together. Fixes Scala rejecting valid D3 multimodal requests
that work in the underlying llama-server.

Preserve decision-candidate versus execution-qualified capability boundaries,
projector integrity, text-only and unsupported-model rejection, bounded media
transport, Scala Link owner-local serving, and existing OpenAI Decisions usage
requirements. Add synthetic regressions for positive and negative qualification,
missing or mismatched projectors, unsupported revisions, and rejected media.

Application-only source change: does not acquire/replace models or inference
runtimes, migrate profiles, or activate production inference.

## 0.1.23 - Correct managed vLLM label runtime attestation

Fix Linux managed `vllm-labels` installation failing after Python dependency
provisioning with `Invalid declared wheel RECORD mismatch`. The two
previously-qualified PyNvVideoCodec wheel exceptions now bind the complete
`PyNvVideoCodec/`-prefixed member paths found in the original wheel and
installed RECORD, without changing original artifact bytes, package versions,
wheel hashes, other runtime checks, or model source identity.

Add synthetic regression coverage for exact exception consumption and rejection
of modified, missing or unexpected wheel members. Local checks also verified
both paths and SHA-256 values against the preserved original upstream wheel.

This source release does not install/activate inference runtimes, change model
profiles or modify the production Scala service. Live model/GPU execution remains
separately unqualified.

## 0.1.22 - Managed vLLM and PyTorch runtimes

Integrates the reviewed managed Python inference-runtime installation and
cancellation workflow into Scala's existing multi-engine runtime control plane.

- Add independently qualified vLLM and PyTorch runtime catalog candidates,
  model-first and engine-first Runtime Search, and installation through the same
  CLI/TUI operations as llama.cpp, q27 and NInfer.
- Acquire exact reviewed isolated CPython, private uv and hashed dependency
  wheels; embed required recipes, engine runners, H2O source bindings and
  licensing in standalone Scala releases without a source checkout.
- Preserve separate Linux and native Windows runtime contracts, isolated CUDA
  dependencies, immutable runtime identity, failed-install recovery, guarded
  removal, explicit side-by-side updates and Scala-owned cache cleanup.
- Add an explicit TUI Cancel installation action (F6), independent of Close,
  with supervised subprocess-tree cancellation, lease lifetime protection,
  cleanup of incomplete staging and retry safety; fix native Windows H2O
  provisioning to use the private verified uv executable without global PATH.
- Offer explicit, reviewed Model Profile engine changes for obsolete
  native_decision references without silently changing profiles, runtime
  selections, settings, weights or model provenance.

Development validation uses local synthetic/packaging checks. Actual dependency
installation, native Windows execution and live model/GPU inference remain
operational qualifications and are not claimed by this source release.

## 0.1.21 - First-class vLLM and PyTorch engines

Replaces the specialised `native_decision` engine with independently qualified
`vllm` and `pytorch` engines. Decision remains an engine-neutral capability.

- Add native vLLM OpenAI Chat Completions and Responses serving for a bounded,
  qualified subset of original Hugging Face safetensors checkpoints, including
  streaming, cancellable requests, and independently scoped inference settings.
- Preserve H2O-Lightning's upstream trained label scoring and calibration through
  the separate vLLM engine. Preserve Imajev's original PyTorch/PEFT float32 trained
  readout, image decisions, calibration and abstention under a model-specific runner.
  Make upstream Imajev fast mode an explicit qualified option; merged LoRA remains
  unsupported pending independent qualification.
- Keep Linux and native Windows runtime identities, verified dependency closures,
  supervised process ownership and exact capability gating. Explicitly preserve
  existing model/profile/runtime state: obsolete `native_decision` profiles require
  an intentional switch to the new engine and separate runtime preparation.
- Preserve System One/OpenAI Decisions APIs, Scala Link, llama.cpp/q27/NInfer
  behaviour, runtime-origin neutrality and Settings/Profile precedence.
  No automatic production activation, runtime installation or model mutation.

Local synthetic/static tests qualify source integration. Live Linux/Windows GPU
execution and numerical equivalence of accelerated modes remain separately
unverified operational checks.

## 0.1.20 - Native Decision, updated inference runtimes and multimodal embeddings

Integrates the reviewed October 2026 upstream inference changes into Scala's
existing multi-runtime control plane, preserving normal artifact/engine/runtime/
Settings/Profile separation and independent application/runtime updates.

- Complete native llama.cpp System One decision serving, model candidate versus
  execution-qualified discovery, request activity and cancellation, and the
  OpenAI-style /v1/decisions route with strict response-contract gating.
  Qualified upstream llama.cpp runtimes cannot advertise OpenAI Decisions while
  their native usage observations are insufficient; System One remains available.
- Qualify llama.cpp v0.6.0 and reviewed b11565 runtime support, exact projector
  identities and native multimodal pooled embeddings (text, bounded images,
  PCM WAV audio and safe uncompressed AVI video) without changing text embeddings
  or adding a separate inference engine.
- Admit q27 v0.14.5 native executable/source variants with checked provenance,
  Bonsai 2 12G identity, reduced-draft-vocabulary controls and explicit
  unsupported-request handling.
- Add independently qualified NInfer v3 artifact inspection, canonical Linux
  source and native Windows portable runtime routes alongside preserved v2.
  Expose verified Linux NInfer native JSON/schema, grammar, choice, regex and
  strict-tool output constraints without implementing a separate grammar sampler.
- Preserve Model Profile authority, per-engine Settings precedence, Scala Link
  ownership, runtime/package integrity, and fail-closed capability reporting.
  No source/runtime update activates or migrates existing models, profiles or
  inference settings automatically.

The integration is locally validated with synthetic contracts; live GPU/model
inference remains independently qualified on the installed host/runtime pair.

## 0.1.19 - Reliable cold Decision runtime selection

Fixes Windows H2O-Lightning first-load failures when a pinned external
native_decision runtime needs longer than Scala's bounded public discovery
window to complete authoritative integrity verification.

- Keep public model and runtime discovery bounded and responsive while explicit
  Model Profile loads can finish independent, verified external runtime probes.
- Reuse one in-flight native runtime probe after cold-discovery timeout or
  load cancellation rather than repeatedly discarding valid work.
- Preserve original runtime/model/Settings compatibility, exact execution
  identity, native qualification and failure diagnostics, without persisting
  synthetic selections or switching to an unrelated runtime.
- Preserve independent IMAJEV and H2O variants, and allow load verification
  cancellation without blocking public inventory requests.

This is an application update only. It does not install or replace model
weights, Python/vLLM/CUDA runtimes, Model Profiles or inference settings.
Actual Windows H2O GPU execution remains separately qualified.

## 0.1.18 - Native image decisions

Adds genuine image inputs to Scala's existing typed Decision API, including
the original IMAJEV-4B PyTorch/PEFT readout, without translating decisions into
generative chat or changing other inference engines.

- Accept optional PNG, JPEG or WebP image data URLs on `POST /v1/systemone`,
  with at most two images of 8 MiB each and unchanged overall request limits.
- Preserve original image data and order through Model Profile resolution,
  Scala Link, native runtime dispatch and upstream image preprocessing.
- Admit image requests only for qualified source-backed `torch-readout` pairs;
  reject image inputs on native label runtimes, llama.cpp and incompatible
  models instead of silently dropping images or using a generative fallback.
- Keep text-only decisions, upstream probability and abstention readouts,
  runtime integrity/identity and request-local validation unchanged.
- Require a separately verified, newly qualified IMAJEV source runtime with
  Pillow and torchvision. This application update never installs or restarts
  runtimes or changes existing model profiles.

## 0.1.17 - Windows vLLM Decision runtime discovery

Fixes missing native Windows H2O Decision candidates when the pinned vLLM
runtime contains a large installed Python dependency closure.

- Track the bounded Windows vLLM launcher, implementation, original wheel and
  SDK control paths for catalogue refresh instead of enumerating tens of
  thousands of NTFS members on every model-listing request. Keep existing full
  runtime fingerprint verification on cold admission and again before load.
- Increase the bounded Windows runtime verification allowance for slow cold
  filesystems. The public discovery deadline remains unchanged, and only the
  original runtime/model/native validator proof grants execution capability.
- Leave the native Windows Imajev/PyTorch readout and existing Linux runtime
  observation behavior unchanged.

This is an application update; model weights and Python/CUDA serving runtimes
remain separately installed and owned by their local Scala node.

## 0.1.16 - Native Windows Decision runtimes

Adds native Windows support for Scala's source-backed Decision engine without
WSL, while preserving the existing Linux implementation and runtime admission.
Application updates remain separate from model/runtime installation.

- Launch pinned Windows Python runtimes directly and supervise their process trees
  with Windows Job Objects, including startup, cancellation, crash and unload
  cleanup. Retain Windows-safe runtime integrity, source closure and discovery
  invalidation without granting capabilities from metadata or origin.
- Provide native Windows CUDA provisioning and isolated model qualification for
  Imajev's original unmerged PyTorch/PEFT readout, float32 trained head and
  calibration. Preserve decision-only auxiliary profiles and Settings inheritance.
- Add reproducible, SHA-pinned Windows H2O Lightning provisioning for the
  reviewed vLLM 0.29.0 Windows compatibility variant, original label-scoring
  shim and private CUDA SDK, with fail-closed wheel/package verification and
  independently qualified runtime identities.
- Retain `decision_candidate` and `architecture.output_modalities` discovery
  with execution-qualified `decision` granted only after the native proof.
  Add offline Windows provisioning, native-lifecycle, typed Decision and
  cross-platform regression coverage.

Windows model runtimes and model weights are **not** included in this application
release. Imajev has separate development-host smoke evidence; H2O's native
Windows GPU inference remains unverified and is not advertised as qualified
until its own runtime/model checks succeed.

## 0.1.15 - Fast native Decision discovery and client interoperability

Improves Scala's model catalogue and Model Profiles responsiveness for native
Decision runtimes, including H2O-Lightning and Imajev, without changing inference
engines, model weights, or running profiles.

- Share process-local runtime inspections among profiles and requests instead of
  repeating heavy external Python wheel-closure checks for every model listing.
  Reuse dependency inventories with metadata and RECORD-based invalidation,
  bounded freshness, and explicit refresh; execution still independently verifies
  immutable runtime identities and artifact closure.
- Probe external runtimes concurrently and bound cold discovery latency. Preserve
  fail-closed runtime compatibility, Settings precedence and profile ownership.
- Advertise native Decision candidates through both
  `capabilities.decision_candidate` and
  `architecture.output_modalities: ["decisions"]` for Unsloth/Coded, including
  `GET /v1/models?output_modalities=decisions`, without declaring unqualified
  execution capability.
- Add synthetic discovery and dependency-topology regression coverage. Preserve
  existing llama.cpp, NInfer and q27 discovery and inference contracts.

## 0.1.14 - Native source Decision runtimes

Introduces narrowly qualified local source-backed Decision runtimes for
structured choice, ordinal score and yes/no answers, without changing
existing generative serving or requiring Norted-produced artifacts.

- Add the independent `native_decision` engine with explicit
  `.decisionbundle` sources, external runtime discovery and profile-bound
  native System One JIT serving. Source provenance, artifact metadata, names and
  runtime identity alone never grant Decision capability.
- Support original vLLM-based label scoring and trained PyTorch/PEFT readouts,
  preserving native confidence, calibration and abstention observations.
  Keep llama.cpp, q27 and NInfer inference controls and eligibility separate.
- Verify complete pinned source and installed wheel closures before launch,
  attest runtime/model identity through health and an exact native validator,
  supervise native worker process trees, and fail closed on mismatches.
- Allow explicitly SHA-pinned original wheel archives to attest narrowly
  enumerated defective vendor RECORD members without editing their installed
  bytes, RECORD files or changing the required package version. All other
  wheel hashes remain mandatory.
- Keep source bundles, engine runtimes, and user-owned Model Profiles separate:
  this application release neither installs a particular model nor starts
  inference or changes existing GPU residency.

## 0.1.13 - Unsloth named-message interoperability

Improves Chat Completions tool-history compatibility with Unsloth Studio while
preserving native engine/template boundaries and safe message semantics.

- Accept validated optional participant names in Chat message history and
  normalize named tool results only when an earlier assistant tool call with
  the same ID establishes the matching function name. Preserve tool arguments,
  content, IDs and ordering without granting capabilities from message names.
- Retain participant identities in the engine-neutral message representation
  and forward them to qualified llama.cpp templates or explicit q27 external
  templates. Return a typed unsupported-name error when NInfer or q27 native
  Chat cannot represent meaningful participant identities.
- Add synthetic Unsloth Web Search, edit_file and Code Mode continuation
  fixtures, cross-layer Scala API/profile-routing tests, and an optional
  CPU-only native contract harness verified against reviewed NInfer sources.
- Document the remaining NInfer limitation for meaningful named participants
  and the installed Unsloth external-provider builder's current omission of
  ordinary participant names. Do not claim unrestricted Code Mode support.

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
