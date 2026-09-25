# Scala

Scala is a terminal-first local inference control plane. It discovers local model artifacts, manages separately versioned inference runtimes, owns backend processes, and exposes an authenticated OpenAI-compatible text gateway. Responses is the primary API; Chat Completions is a compatibility surface.

The central distinction is:

```text
model artifact = one discovered physical GGUF, q27, or NInfer file
engine         = adapter, compatibility rules, launch semantics, health, protocol translation
runtime        = one concrete executable package and version
settings       = Server Settings plus independent per-runtime inference defaults
Model Profile  = a user-owned serving target binding one artifact, one engine, role, and overrides
```

A GGUF model is not permanently tied to llama.cpp, a q27 model is not permanently tied to q27,
and an NInfer container is not the NInfer executable itself. Model Profile creation uses registered
engine compatibility, and exact runtime resolution remains separate from the profile. Today Scala
ships adapters for [llama.cpp](https://github.com/ggml-org/llama.cpp),
[q27](https://github.com/signalnine/q27), and [NInfer](https://github.com/Neroued/ninfer).

## License and contributions

Original Scala code is [Apache-2.0 licensed](LICENSE); see [NOTICE](NOTICE) for
project attribution. Release archives include reproducible third-party notices
and corresponding MPL dependency sources. Dependency and runtime licenses remain
separate. See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).

## Scala releases

**Scala** is the application, command (`scala`), and repository (`ver2-sh/Scala`).
Its workspace crates use `scala-*` names, the systemd unit is `scala.service`, and
machine-to-machine serving uses **Scala Link** (`scala.link.v1`). Release archives
and installers are named `scala-*`.

For source installation, `./scala-service.sh install` builds the application,
installs the Linux service, and links the actual binary as `/usr/local/bin/scala`.
Use `./scala-service.sh` for source-build/service maintenance. It does not replace
the application CLI: `scala --help`, `scala status`, and `scala models list` run
the same executable shipped in release archives.

Application storage uses the platform-native Scala namespace. On Linux this is
`~/.config/scala`, `~/.local/share/scala`, `~/.local/state/scala`, and `~/.cache/scala`;
`scala config show` reports the resolved paths on every supported platform.
API keys use `scala_sk_`; optional orchestration headers are `X-Scala-Session` and
`X-Scala-Role`. The application does not provide alternate product-name commands,
old storage-path discovery, or dual Link service registration.

Norted remains the separate model builder. Its artifact schemas, immutable model
identity domains, source lineage, and historical validation records are not product
branding and are preserved. Equivalent standard artifacts retain equal behavior.

The release pipeline targets Linux x64/ARM64, macOS Intel/Apple Silicon and
Windows x64, with server-only archives, shell/PowerShell installers and SHA-256
checksums. Models, GPU drivers and independently versioned inference runtimes are
installed separately. These are packaging targets, not five verified native platforms. Linux x64
installation/update is validated locally; ARM64 Linux, macOS and Windows native
execution remain unverified. A packaged server does not imply every runtime
supports every platform.

Scala is public, with v0.1.0 as the first developer release. See
[installation and release maintenance](docs/releases.md) for public downloads,
installation/update commands, platform limits and publication gates.

## Install and update Scala

`ver2.sh` is the stable distribution front door: its dedicated Cloudflare Worker
serves installer bodies
and versioned release assets under `/scala/`, while the generated cargo-dist shell
and PowerShell installers remain authoritative.

Latest stable, Linux/macOS:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://ver2.sh/scala/install.sh | sh
```

Latest stable, Windows PowerShell:

```powershell
irm https://ver2.sh/scala/install.ps1 | iex
```

To inspect first, download the script and read it before executing it. On Windows,
this alternative applies execution policy only to the child PowerShell process:

```powershell
Invoke-WebRequest -Uri 'https://ver2.sh/scala/install.ps1' -OutFile 'scala-installer.ps1'
Get-Content .\scala-installer.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scala-installer.ps1
```

Organizational Group Policy still wins. For pinned versions and the Linux/macOS
inspect-first flow, see [release installation](docs/releases.md#install-and-update).
Both generated installers verify embedded SHA-256 archive checksums before
extraction. A guarded release-generation step adds this missing check to upstream
cargo-dist 0.33.0 PowerShell while preserving its installation, PATH and receipt
behavior. Windows/macOS native execution, signing and notarization remain
[platform gates](docs/releases.md#trust-and-platform-gates).

```sh
scala update --check       # fresh stable-version discovery
scala update               # report versions, then explicitly confirm replacement
scala update --yes         # explicit unattended approval
scala --json update --check
scala --json update --yes
```

Interactive TUI startup checks in the background at most daily, caching errors too.
The footer shows availability or failure; `/update` requests a fresh check. Serving
continues if the channel is unavailable. Headless serve/status/doctor do not check,
and no telemetry or private GitHub tokens are sent. Checks never install updates.
Explicit checks bypass cached results; inaccessible channels fail, without stale success.
JSON/noninteractive replacement requires `--yes`; `--check --yes` is invalid.

Replacement requires a matching cargo-dist receipt and executable ownership, and
only installs a strictly newer stable version. Stop existing serving/TUI instances
first, then use the original invocation or service startup method afterward. Updates
never start, stop, or restart services; the opt-in login-startup registration below
is the only user-service definition Scala manages, and an in-place update at the same
executable path leaves that registration valid. Package-manager, source and manual
copies must use their owning installation method.
Models, inference runtimes, settings, credentials, state and evidence are preserved.
Application updates are separate from `scala runtimes update` and the source-checkout
helper `./scala-service.sh update`.

### Start automatically on login

Installers never enable background startup. To have `scala serve` launch when you
log in, use the TUI: **Settings → Server → APPLICATION → Start automatically on
login — Enabled/Disabled**. The row is a live read of the operating system's
registration, not a saved preference: deleting or altering the registration
outside Scala is reported truthfully on the next refresh, and moving the
executable marks the registration as moved (re-enable to rebind it). Enabling or
disabling only affects the next login and never starts, stops, or restarts the
running server. Mechanisms are per-user and need no administrator rights:

- **Windows:** Task Scheduler logon task `dev.scala.serve.<SID digest>` — the
  name is derived from the current user's SID because Task Scheduler names are
  machine-global, so each Windows user gets an independent Scala-owned task.
  The task runs as the current user with an `AtLogOn` trigger, Interactive
  logon, Limited run level, launching the exact current executable with
  `serve`, with restart-on-failure and no execution time limit. Startup is
  reported `Enabled` only while the registration is enabled and matches that
  whole contract; a disabled or altered task is reported truthfully and
  re-enabling re-registers it.
- **Linux:** per-user systemd unit `dev.scala.serve.service` under
  `~/.config/systemd/user`, enabled without `--now` so nothing launches at
  enable time. It is independent of the privileged `scala.service`
  unit managed by `./scala-service.sh` for source checkouts.
- **macOS:** per-user LaunchAgent `~/Library/LaunchAgents/dev.scala.serve.plist`
  running the exact executable with `serve` (`RunAtLoad`, keep-alive on
  unsuccessful exit).

The same capability is scriptable:

```text
scala startup status     # live OS registration state
scala startup enable     # register `scala serve` for the next login only
scala startup disable    # remove the registration only
```

All three honor `--json`; `enable`/`disable` never touch a running server.

## Models, Settings, Model Profiles, and Runtimes

- Models is artifact inventory: format, path, size, technical capability, and provenance.
- Settings separates Server operations from independent llama.cpp, NInfer, and q27 overrides.
- Model Profiles are user-created mutable serving targets and the normal load unit.
- Runtimes are installed executable implementations selected for the profile's bound engine.

Two Model Profiles may bind the same artifact and carry different roles or overrides. Multi-backend status and
runtime provenance retain both the Model Profile ID and the underlying artifact Model ID. The
OpenAI-compatible model alias is the loaded Model Profile ID, including `GET /v1/models` entries and
the `model` value accepted by inference requests.

Effective settings use one typed system with this complete precedence:

```text
runtime defaults → Settings runtime overrides
  → Model Profile overrides
  → ephemeral invocation overrides
```

Only the selected runtime participates; one runtime never inherits another runtime's settings.
Request generation fields may override configured generation defaults when the exact adapter and
runtime support them. There is no Global inference layer, model-default layer, or hidden policy.
For llama.cpp semantic alternatives, a higher layer also suppresses lower-layer siblings: built-in
versus bound-file chat templates, all-versus-exact CPU MoE placement, and draft artifacts made
inapplicable by `off`, n-gram, or `draft-mtp` speculation. Contradictory alternatives in the same
winning layer remain invalid.

Persistent state is deliberately split:

```text
<data>/settings.json         schema 3: Server Settings and strictly namespaced Settings overrides
<data>/model-profiles.json   schema 2: user-owned Model Profiles
```

Each has its own inter-process lock and atomic replacement. Older combined state is not consumed or
deleted; settings schemas 1 and 2, model-profiles schema 1, and obsolete Global/unqualified fields
are rejected rather than migrated. Current inference overrides use fully qualified engine setting
IDs (for example `llama.cpp.context_length`); unqualified names are not accepted. Because the
application is unreleased, stale development state should be recreated or manually updated by the
developer/user.

### Model Library

Models includes a managed, runtime-agnostic Model Library as well as configured external search
paths. The Installed view browses all locally discovered GGUF, q27, and NInfer artifacts. Discover
searches Hugging Face directly, exposes each concrete file/quantization with repository and exact
revision context, filters by format, and labels remote results as format candidates whose runtime
compatibility is unverified. Download, import, and removal are explicit operations; only
receipt-backed acquisitions below the managed root can be removed by the library.

Discover includes an in-process model download manager. Separate acquisitions run concurrently
(four by default), overflow requests wait in FIFO order, and a completed or failed job immediately
releases its slot to the next queued request. Settings > Server > Downloads exposes the typed
`server.max_parallel_model_downloads` value with a minimum of 1; raising it starts more queued jobs
immediately, while lowering it lets current transfers finish and limits subsequent starts. Exact
duplicate references are suppressed while queued, resolving or downloading, paused, or completing
cancellation cleanup. Active downloads can be paused; pausing retains resumable partial data and
releases the scheduler slot so queued work can run. Resume continues through the existing HTTP
Range partial-file mechanism.

Queued, paused, resolving, and downloading jobs can be cancelled where safe. Cancellation removes
partial transfer data before the same model reference becomes eligible again; verifying,
validating, and installing are intentionally atomic and non-cancellable. References that resolve
to the same package acquisition wait for its owner and settle as already installed. The TUI
download cards retain a bounded recent history and expose progress, bytes/total, transfer rate,
ETA, queue state, and Pause, Resume, or Cancel controls as applicable. Search and TUI navigation
remain independent of downloads.

Managed models use the platform-native Scala data directory shown by `config show`:

```text
<data>/models/
  huggingface/<publisher>/<repository>/<revision>/<artifact-key>/
    <repository-relative artifact or package tree>
    .scala-library.json
  imports/<stable-import-key>/
    <copied artifact or exact package tree>
    .scala-library.json
```

The package-level receipt records one acquisition identity plus every exposed primary member. Each
member retains provider, repository, exact revision, remote filename, size, acquisition time,
source URL, and the authoritative acquisition SHA-256 when one was verified. A rescan accepts that
managed provenance only while the current member size still matches; the acquisition digest is not
misrepresented as a freshly computed current-file hash. Model payloads are not modified.

Norted `BUILD-MANIFEST.json`, `Q27-MANIFEST.json`, and `NINFER-MANIFEST.json` packages are resolved
relative to the directory containing that exact manifest. Downloads and imports reproduce only the
manifest-required closure, preserving safe nested relative paths, all primary outputs, the q27
tokenizer and Sharp companion, embedded NInfer resources, and a declared GGUF projector. Builder
build keys, master IDs, quant-recipe identity, native identity, canonical source lineage, projector
binding, and manifest identity remain authoritative and unmodified. Multiple package primaries are
activated and owned as one acquisition while remaining separate models in the registry and Model
Profiles.

Large files stream to resumable partial files in the application cache. They are checked for the
published or manifest size and SHA-256, inspected through the normal bounded format handling, and
activated by directory rename only after every required file validates. Thus every member of a
package becomes visible together, while failed/partial transfers never enter local discovery.
Each canonical acquisition also holds its own inter-process file lock in the model-download cache
through partial-cache mutation and final activation. TUI and CLI processes therefore share safe
acquisition state without serializing unrelated downloads; managed removal takes the same lock.

Raw q27 pairing uses one shared rule for discovery and acquisition: exact stems first, then a unique
longest boundary-safe prefix, then the sole `.tok` file (including `MODEL.q27` plus
`TOKENIZER.tok`). A Norted q27 package instead uses its exact manifest-bound tokenizer, regardless
of unrelated nearby `.tok` files. The tokenizer header and adapter-owned Q27 architecture metadata
must validate.
NInfer stays a self-contained `.ninfer` v2 artifact with embedded resources and exposes its native
`container_version`, `model_id`, and `weights_id`. GGUF and NInfer use their existing bounded
inspectors after acquisition.

Artifact format and verified compatibility are deliberately different facts. GGUF is only a
llama.cpp candidate, q27 is only a q27 candidate, and NInfer is only an NInfer candidate until the
registered adapter checks the exact artifact, installed runtime, and observed host. Existing GPU,
architecture, tier, native-identity, and runtime restrictions are never inferred from a filename.

## Norted Builder packages

Model paths are served in place. Current q27 Builder packages use schema 6; NInfer uses schema 7
with selected targets, actual embedded frontend hashes and optional draft provenance. Discovery binds
genuine artifact members and provenance—primary hashes, tokenizer, projector, Sharp companion,
lineage, and build facts—and performs the existing complete pre-prepare and final pre-spawn
integrity verification.

Managed package installation is atomic and package-owned. Removing any member plans removal of the
single containing acquisition and all of its primary `ModelId`s. Both the CLI and TUI refuse that
removal while any affected member is active. Configured external packages remain read-only.

Builder packages contain no serving preset. Sharp is an artifact companion and is never selected
automatically. A user may explicitly point a q27 Model Profile's external-template setting at the
package Sharp file or any other compatible local template; both use identical path, size, and
SHA-256 safety. Norted-built artifacts receive no special serving configuration or capability
credit. A package artifact and an equivalent raw artifact expose the same controls when their
observed technical capabilities match.

## Typed serving settings

Common semantic definitions and runtime-specific settings share `SettingId`, `SettingValue`, `SettingDefinition`,
`SettingsPatch`, `ResolvedSettings`, and setting-source attribution. Presentation categories are
General, Downloads, Load, Generation, Reasoning, Prompt, KV / Memory, Speculation, Cache, and
Advanced. Server-operational definitions appear only in Server Settings. Common inference semantics
are defined once in code, then bound independently into each runtime's categories; a Runtime or
Model Profile editor shows the selected runtime's schema, including unsupported fields for diagnosis and removal.

Known effective settings show their value and winning source; unknown pre-start defaults remain explicitly unknown,
for example `1.0 (runtime default)`, `0.7 (Settings override)`, `0.5 (model profile)`, or `200000 (boot inference)`.
Source text supplements the value and never replaces it. Dynamic runtime/model/host derivations
remain part of the runtime-default layer and carry optional secondary detail. A genuine `auto`
policy remains automatic before startup; presenting it never causes Scala to materialize an
invented launch value. Authoritative startup results replace that policy in the running effective
view without changing its winning source. See
[Settings and defaults](docs/settings.md).

llama.cpp exposes load/runtime controls for context and slots; ordered exact-UUID CUDA devices and
multi-GPU split/main-device/fit policy; CPU threads and logical/physical
batches; weight and KV offload; unified KV, checkpointing, Flash Attention, and K/V cache types;
RoPE frequency base/scale; current `load_mode`; MoE CPU placement; architecture-aware active
expert override; reasoning mode/effort/budget/message; built-in or bound-file chat templates; and
exact-runtime-advertised speculative modes with an optional bound GGUF draft. `load_mode` is the
only first-class model-loading policy: `mmap`, `mlock`, `mmap+mlock`, and `dio` choices cover the
old “Try mmap” and “Keep Model in Memory” behavior without reviving deprecated standalone flags.
Generation/profile defaults include engine-supported seed behavior, response limit, stop strings,
repeat/presence penalties, system prompt, and an optional JSON Schema. The common setting retains
`random` for engines that implement runtime randomness; q27 exact schemas accept numeric seeds only.
The JSON Schema is a request default, not a process-wide constraint: an omitted request format uses
it, while explicit text, JSON object, or request JSON Schema wins.

q27 exposes ordinary settings for context/slots (including the background-slot context), generation
samplers, thinking and budget, opt-in per-request thinking, fast-head, KV mode, MTP
depth/probability, suffix drafting and compiled-width behavior, continuous batching, sampled-graph
residency, optional greedy tool-call constraints, external template path/hash and delivery,
generation-prompt rendering, template thinking, the generic `strip_initial_reasoning` response
filter, and prefix cache controls. Automatic q27 KV selection is the same server-owned quality
sequence for every compatible q27 artifact, filtered by exact runtime proof. `Q27_BATCH` and
`Q27_SAMPLED` are typed because v0.10 documents them as serving controls; checksum, trace,
benchmark, instrumentation, and development-only variables remain unsupported and are scrubbed from
the child environment.
On the runtime chat route, audited v0.10 supports ordinary function tools, auto/none/required/named
choice, parallel-call policy, assistant/tool history, and streaming call deltas. External-template
raw completion mode deliberately reports no tool capability. `q27.constrain_tools` strengthens
eligible greedy automatic calls but does not pretend to constrain sampled or forced calls.

NInfer exposes direct settings for context/KV, exact-help-advertised KV formats, CUDA Graph, prefix
reuse, thinking, speculation/backend/draft/proposal head, samplers and greedy mode, queue limits,
request/statistics limits, Vision residency and media budgets/workers, Responses-store bounds, and
advanced cache controls. There is no nested named speculation abstraction. The exact source
commit/tree, executable help, native container v2 identity, revision-specific startup schema,
draft/Vision capability, and observed startup state remain authoritative. DFlash remains limited
to exact `qwen3.6-35b-a3b`/`groupwise-int`. DFlash2 requires the complete native 66-tensor companion
on supported 27B targets. MTP 1–5 and DFlash/DFlash2 1–15 are exclusive backend choices;
the qualified runtime supports drafting with Vision. A plain MTP artifact cannot enable DFlash2. The audited current source advertises `bf16`,
`int8`, `fp8`, `nvfp4`, and `k8v4`; older executable help narrows the choice list instead of
inheriting newer formats. `ninfer.context_cost_presets` is first-class and binds its canonical path
and SHA-256 immediately before launch; raw `--context-cost-presets` is rejected. NInfer
host/port/key/model alias/device/CORS/request-log controls remain Scala-owned, and diagnostic,
benchmark, tracing, and kernel-development controls are not promoted into ordinary settings.

Structured path values have stable semantics. Absolute paths are used directly. Relative paths
resolve lexically beneath Scala's `<data>` directory and cannot escape it with `..`. Resolution
happens before inspection, validation, adapter translation, and provenance.
Saving `q27.template_path`, `llama.cpp.chat_template_file`, or
`llama.cpp.speculative_draft_model` through the CLI or TUI automatically records its SHA-256;
every load rereads the bound file and rejects a content mismatch. Draft GGUFs must additionally
prove identical bounded tokenizer metadata with the target; the exact llama-server load remains
authoritative for draft architecture/tensor compatibility and is reported as needs-attention until
that definitive load succeeds. Current upstream `draft-mtp` uses MTP heads from the main model and
does not consume an external draft GGUF. Because Scala's bounded GGUF identity does not currently
prove usable MTP heads architecture-neutrally, selecting `draft-mtp` remains needs-attention until
the exact llama-server load proves it.

All commands honor global `--json`. The scriptable management surface includes:

```console
scala model-profiles list
scala model-profiles show <PROFILE>
scala model-profiles create <PROFILE> --model <MODEL_ID> --engine <ENGINE_ID> [--role primary|auxiliary]
scala model-profiles duplicate <SOURCE> <PROFILE>
scala model-profiles delete <PROFILE>
scala model-profiles set-model <PROFILE> <MODEL_ID>
scala model-profiles set-engine <PROFILE> <ENGINE_ID>
scala model-profiles set-role <PROFILE> <primary|auxiliary>
scala model-profiles set <PROFILE> <SETTING=VALUE>...
scala model-profiles unset <PROFILE> <SETTING>...
scala model-profiles load <PROFILE> [--runtime <RUNTIME_ID>] [--set <SETTING=VALUE>...]
scala model-profiles compatibility <PROFILE> [--runtime <RUNTIME_ID>]
scala load <PROFILE> [--runtime <RUNTIME_ID>] [--set <SETTING=VALUE>...]
scala unload <PROFILE>
scala settings show --server
scala settings set --server <SETTING=VALUE>...
scala settings unset --server <SETTING>...
scala settings show --runtime q27
scala settings set --runtime q27 <SETTING=VALUE>...
scala settings unset --runtime q27 <SETTING>...
```

## Runtime packs

Scala can search authoritative upstream runtime catalogs, verify and install either release packs or fixed source-build packs, retain multiple versions side by side, select an exact runtime for an artifact format or model, and launch that exact executable. Ordinary upstream releases and compatible NInfer commits are discovered independently of Scala releases; a Scala update is needed only when upstream breaks a known packaging, container, build, CLI, or protocol contract.

Installed packs are local truth. Starting the CLI, TUI, `status`, or `serve` never waits for GitHub. Network access occurs only for explicit search/update operations or after the user opens TUI search.

Managed installs live under the platform-native Scala data directory printed by `scala config show`:

```text
<data>/runtimes/
  <engine>/<platform-architecture-accelerator-variant>/<version>/<runtime-id>/
    runtime.json
    <upstream package files>

<data>/runtime-selections.json
```

Each managed `runtime.json` is an immutable schema-version-2 record; existing schema-version-1 release manifests remain valid and are not rewritten. Release manifests retain their repository/release/assets and verified archive digest semantics. Source-build manifests instead retain the canonical repository, source ref, exact commit and Git tree, commit timestamp, fixed Scala recipe and build system/arguments/target, observed toolchain/system-package versions, build target platform/architecture/accelerator, build time, entrypoint, and resulting executable SHA-256. They contain no fabricated archive digest or upstream-binary claim.

Selections are mutable preferences and are therefore stored separately. Resolution precedence is:

```text
explicit load --runtime
  → model-specific override
  → artifact-format default
  → best compatible installed fallback
```

Every candidate is checked through the selected engine's model+runtime+host compatibility contract as well as the artifact format, adapter, host, and exact installed manifest. The same decision is used for explicit selection, stored overrides/defaults, candidate listing, automatic fallback, and final load admission. A missing or incompatible persisted selection is reported as an error; select another runtime explicitly. Selecting an older runtime is the rollback mechanism.

## Official providers and compatibility

Release catalog entries come from the current GitHub Releases API, not a compiled version list. Only uploaded assets whose names and URLs match the authoritative repository contract and which carry a valid GitHub SHA-256 digest are installable. Source catalogs resolve immutable commits through the canonical GitHub repository. Search shows compatible, recommended, needs-attention, or incompatible state without downloading an archive or cloning source.

The llama.cpp provider recognizes these official priority families where an actual release asset exists:

- Windows x86_64 CPU, CUDA, and Vulkan;
- Linux x86_64 CPU and Vulkan.

Windows CUDA releases are composite packages: Scala verifies and extracts both the main llama.cpp archive and the matching official CUDA-runtime archive. The provider searches real assets across releases, so a temporarily incomplete newest build matrix does not create a broken candidate. `Latest` is the newest installable build. `Stable` follows llama.cpp's authoritative stable/nightly pointer when it resolves to a matching published build.

The separate llama.cpp managed-source provider covers Linux x86_64 CUDA because upstream does not publish a comparable Ubuntu CUDA archive. One admitted exact nightly commit/tree exposes two intentional parallel source variants, neither labeled as an upstream CUDA binary: immutable CUDA-12 `managed-portable-v4` requires Toolkit `>=12.8,<13.0` and Linux driver `>=525.60.13`, while `managed-portable-cuda13-v2` requires Toolkit `>=13.0,<14.0` and driver `>=580.65.06`. Both require the stable active-toolkit compiler `/usr/local/cuda/bin/nvcc`, bind CMake to that same compiler, build only `llama-server` with fixed real-code targets `sm_75`, `sm_80`, `sm_86`, `sm_89`, `sm_90`, and `sm_120a`, origin-relative build RPATHs, and exact source/recipe/toolchain/executable provenance. Source manifests distinguish provider-owned recipe arguments from the effective CMake arguments actually executed; the latter include the generated typed compiler binding, while observed nvcc version remains toolchain identity. Historical schema-2 manifests that predate the effective list remain readable without fabricating or rewriting information they did not record. Historical CUDA-12 V1/V2/V3 and CUDA-13 V1 identities remain immutable; CUDA-12 V1 is retained for update discovery but is not an automatic serving candidate because its pre-relocatability recipe can retain stale build-tree library paths. Scala validates but never installs CUDA, the NVIDIA driver, or other toolchain packages.

The q27 provider exposes official Linux x86_64 CUDA releases from v0.2.0 onward. Earlier v0.1.x servers omit the terminal streaming finish reason needed for truthful Responses completion state, so they are deliberately excluded. A release is visible when it has a verified upstream archive or when its exact tagged source proves a supported q27 Makefile recipe; source-only current releases therefore remain searchable without being mislabeled as binaries. When both routes cover the same release and variant, the verified upstream binary wins. Scala creates only the W8, W12, and W16 variants whose upstream targets are present at that revision: `build/q27-server-w8`, default `build/q27-server`, and `build/q27-server-w16`. W8 is automatically preferred for a positively observed 24 GiB-class device and is also the conservative fallback when VRAM is unresolved. W12 is q27's normal/default build and is preferred only when the selected device is positively confirmed as 32 GiB-class or larger. W16 remains an explicit specialist choice and stays last automatically. Because upstream publishes no exact W16 floor, Scala records only the known fact that 24 GiB-class hardware is insufficient; a larger device remains needs-attention rather than being assigned an invented minimum. Stable/Latest follow the newest admitted semantic release, including a supported source-only release. Upstream currently provides no managed Windows q27 route, so Windows reports these entries as incompatible rather than inventing one.

For a q27 source build, discovery resolves the release tag to a full Git commit/tree and inspects bounded exact-revision build and runtime-contract files. Search does not clone or compile. The current v0.10.0 contract fingerprints its provider-reviewed `Makefile`, `README.md`, `src/server.cu`, and `src/engine.cuh`; that evidence proves generic q27 raw-completions, thinking/sampling, MTP/suffix/fast-head, KV-mode, startup-banner, and target-specific W_MAX capabilities. Installation revalidates the tag contract and immutable commit/tree, checks Linux x86_64, Git, Make, the source-declared CUDA floor and `/usr/local/cuda/bin/nvcc`, and the declared host C++ compiler/standard before staging. It checks out that exact commit separately from model-artifact state, requires the checked-out Makefile to match the provider-audited dependency/command closure digest, removes build-control environment overrides, and runs only `make <selected-target>`. The recipe version, Makefile digest, source commit/tree, exact target, toolchain, and built executable digest persist in the installed provenance, so launch admission does not consult mutable upstream state. The resulting executable must be regular, executable, hash-stable, and pass the q27 adapter probe before atomic activation.

NInfer publishes source rather than an installable release binary. Its provider resolves the canonical `Neroued/ninfer` default-branch HEAD into one `Latest` source snapshot containing the full commit and Git-tree SHAs. It deliberately exposes no `Stable` channel and no fake release asset. Revalidation targets the selected commit itself, so a normal later HEAD does not substitute new source; bounded historical source descriptors retained from explicit searches keep that exact selection addressable after a refresh. Update checks use Git ancestry: identical is current, a descendant is an available update, and backward or diverged history is a provider warning rather than an implicit downgrade. Installed snapshots and selections remain side by side and unchanged until explicitly updated/selected.

The currently reviewed NInfer request/startup authority is commit
`d49296868dcc17bd478ec185f0d3a801bcc0bf56`, tree
`8e2f0275fc533cf11fe05a4ac3ac85f00eb91c72`, with request-log schema 20.
NInfer admission is capability-domain based: exact process/launch and sampler controls use the
observed executable help plus reviewed default-owning source blobs, while request semantics,
request-log/startup proof, thinking, tools, and Vision/media use their own reviewed source-blob
sets. The full commit/tree remains provenance. A later source snapshot retains any domain whose
contract-owning blobs are unchanged; an unreviewed semantic domain becomes NeedsAttention without
erasing unrelated launch controls.

The current official NInfer build contract is Linux x86_64, NVIDIA GeForce RTX 5090, numeric compute capability 12.0 with `sm_120a`, CUDA Toolkit 13.1 or newer, CMake 3.28 or newer, Ninja, a C++20 compiler, pkg-config, FFmpeg development modules, and libcurl. Product name and compute capability are independent observed checks; missing facts are needs-attention and contradictory facts are incompatible. Scala never installs host packages automatically.

Host detection uses the OS and architecture plus a bounded `nvidia-smi` query for every NVIDIA GPU's UUID, name, VRAM, driver, and numeric compute capability when supported; older tools fall back to the otherwise complete query and leave compute capability unknown. An unavailable hardware signal does not stop Scala itself and becomes a needs-attention result, while a successful probe that positively reports no NVIDIA GPU makes a CUDA requirement incompatible. Upstream phrases such as "24 GiB-class" are represented separately from exact byte floors: up to 2 GiB of reporting/ECC/reservation shortfall still counts as the nominal class (matching q27's measured 22.6 GiB A10 case), the next 1 GiB is needs-attention, and a larger shortfall is incompatible. Informational runtime notes remain visible but do not change compatibility; only explicitly unverified conditions produce needs-attention.

llama.cpp uses an ordered accelerator binding. `llama.cpp.devices` contains exact NVIDIA GPU UUIDs in
the intended upstream device order; every entry must resolve uniquely to a visible compatible device and
duplicates fail. Scala writes those UUIDs in the same order to `CUDA_VISIBLE_DEVICES`, then passes
`--device CUDA0,CUDA1,...` using the child-local indexes. If `llama.cpp.devices` is omitted, the existing
automatic policy still chooses one compatible GPU. `main_gpu`, tensor-split, and per-device fit values are
validated against the selected group under current upstream index, broadcast, and arity rules.

Q27 compatibility is evaluated against one concrete observed GPU, and q27-server is launched with `CUDA_VISIBLE_DEVICES` set to that device's full NVIDIA GPU UUID. Official managed q27 fatbin targets are enforced per runtime version before VRAM ranking, so a supported lower-VRAM device beats a larger unsupported device when it meets the runtime and model floors; an unknown compute capability is needs-attention. Scala never treats an `nvidia-smi` number as a CUDA identity. A parent `CUDA_VISIBLE_DEVICES` containing one uniquely resolvable GPU UUID is honored and normalized to the full UUID; empty, numeric, multiple, unknown, or ambiguous constraints make q27 incompatible rather than allowing assessment and launch to diverge. The q27 configuration cannot set this variable because Scala owns the binding. This is single-device binding for q27, not a general GPU scheduler.

NInfer reuses the same exact UUID visibility rules. A managed runtime selects a proven RTX 5090/sm_120a device even when another unsupported GPU is larger, launches with `CUDA_VISIBLE_DEVICES=<full UUID>`, and passes `--device 0`; inside the isolated child, CUDA-local device zero is therefore the selected physical UUID. The common ordered binding retains that one complete `AcceleratorDevice` in launch provenance. This remains exact single-device binding, not scheduling.

## Secure, transactional installation

An install follows one fail-closed transaction:

```text
official catalog result
  → restricted HTTPS download into cache temporary file
  → exact size and SHA-256 verification
  → safe ZIP/tar.gz extraction into a unique staging directory
  → unique expected entrypoint lookup
  → entrypoint SHA-256
  → engine-adapter identity/capability probe
  → immutable manifest write and sync
  → atomic directory activation
```

GitHub redirects are accepted only among the exact GitHub API, raw-source-metadata, and release-asset hosts. API tokens from `GITHUB_TOKEN` or `GH_TOKEN` are optional, are sent only to the GitHub API client, and are neither logged nor persisted. A missing trustworthy digest makes a release-asset route non-installable; a provider-supported source route remains independently eligible. Partial downloads are never valid cache entries.

Extraction rejects absolute paths, parent traversal, Windows drive/backslash tricks, duplicate archive paths, special entries, oversized archives, and links that escape staging. The official Linux llama.cpp archives use relative symlinks; those are accepted only after their resolved targets are proven to remain inside staging. A download, extraction, or probe failure removes staging and cannot create an Installed runtime.

Managed source runtimes use the same transactional acquisition arm with provider-owned recipes. NInfer's recipe is:

```text
canonical source candidate
  → bounded prerequisite checks (no host modification)
  → exact commit revalidation
  → unique staging checkout over GitHub HTTPS
  → HEAD and HEAD^{tree} verification
  → bounded CMake dependency audit
  → fixed Release/Ninja/sm_120a ninfer-serve recipe
  → executable SHA-256 + adapter probe
  → source-build manifest write and sync
  → atomic activation
```

The fixed `ninfer-serve-v2` recipe enables product apps, disables tests and benchmarks, and builds only `ninfer-serve`; the retained source/build tree supplies the runtime-relative layout upstream expects. It declares `/usr/local/cuda/bin/nvcc` as its stable active-toolkit prerequisite, and the generic builder supplies that same path as `CMAKE_CUDA_COMPILER`. Managed build-control variables such as `CC`/`CXX`, CUDA compiler overrides, `CFLAGS`/`CPPFLAGS`/`CXXFLAGS`/`LDFLAGS`, CUDA/NVCC flags, `CMAKE_ARGS`, generator, parallel-level, and toolchain-file overrides are removed while normal non-CUDA tool discovery remains available. The checked CMake tree may use vendored source and system packages but is rejected if it introduces FetchContent, ExternalProject, Git clones, or network downloads. Git/CMake children are direct argument-array processes with drop cancellation; any prerequisite, checkout, configure, build, hash, probe, or activation failure removes staging and creates no installed runtime. Historical `ninfer-serve-v1` identities retain their PATH-selected compiler meaning and remain readable; V2 is the next immutable generation on the same functional update line.

Updates install a new exact runtime beside the old one. They never overwrite or delete the previous version and never silently move a pinned selection. The inactive older pack remains available until explicitly removed. The running runtime cannot be removed, and selected runtimes must be explicitly deselected or remapped first.

## CLI

The runtime command tree is scriptable and supports global `--json`:

```console
cargo run -p scala -- models list
cargo run -p scala -- models info <MODEL_ID>
cargo run -p scala -- models search [QUERY] [--format gguf|q27|ninfer]
cargo run -p scala -- models download <MODEL_REF>
cargo run -p scala -- models import <PATH>
cargo run -p scala -- models remove <MODEL_ID>
cargo run -p scala -- runtimes list
cargo run -p scala -- runtimes search [QUERY]
cargo run -p scala -- runtimes search --refresh
cargo run -p scala -- runtimes info <RUNTIME_ID>
cargo run -p scala -- runtimes install <RUNTIME_ID>
cargo run -p scala -- runtimes remove <RUNTIME_ID>
cargo run -p scala -- runtimes check-updates
cargo run -p scala -- runtimes update <RUNTIME_ID>
cargo run -p scala -- runtimes select --format gguf <RUNTIME_ID>
cargo run -p scala -- runtimes select --format gguf --track stable <RUNTIME_ID>
cargo run -p scala -- runtimes select --format gguf --track latest <RUNTIME_ID>
cargo run -p scala -- runtimes select --format q27 <RUNTIME_ID>
cargo run -p scala -- runtimes select --format ninfer <RUNTIME_ID>
cargo run -p scala -- runtimes select --model <MODEL_ID> <RUNTIME_ID>
cargo run -p scala -- runtimes clear-selection --format gguf
cargo run -p scala -- runtimes clear-selection --model <MODEL_ID>
```

Search terms match engines, formats, versions, platforms, architectures, backends, variants, and source acquisition, so queries such as `llama`, `q27`, `ninfer`, `.ninfer`, `CUDA`, `source`, `GGUF`, and `Q27` work. A runtime reference is its stable exact runtime ID, never an internal path. Search opened from the generic Runtimes page remains model-independent. Search opened from a selected model passes that model to every engine adapter and combines catalog, platform, selected-device, runtime-variant, model metadata, and model hardware compatibility before showing recommendations; another engine advertising the same format remains eligible.

Selections default to `--track pinned`. `--track stable` and `--track latest` keep the exact selected runtime in place but constrain future update checks to the provider's truthful channel candidate. Updates remain explicit and side by side; selecting the newly installed runtime is a separate action.

The rest of the command tree remains:

```text
scala
├── tui
├── serve
├── status
├── auth status
├── auth keys list|create|revoke
├── load <MODEL_PROFILE_ID> [--runtime <RUNTIME_ID>] [--set <ID=VALUE>]...
├── unload
├── models list|info|search|download|import|remove
├── runtimes ...
├── model-profiles list|show|create|duplicate|delete|set-model|set-engine|set|unset|load|compatibility
├── settings show|set|unset
├── startup status|enable|disable   # per-user OS login startup for `serve`
├── engines list       # low-level adapter diagnostics
├── config show
└── doctor
```

### Whole-system diagnostics

`doctor` checks application paths and configuration, public authentication,
local model/package discovery, settings and Model Profiles, engine adapters,
managed runtime integrity and selections, private control state, host/GPU
compatibility, and relevant managed-source toolchain prerequisites.

```console
scala doctor
scala doctor --verbose
scala --json doctor
```

The diagnostic is always offline and read-only. It does not initialize stores,
repair state, start the Server, install packages, invoke Norted-Utils, contact
runtime providers, or check for updates. The default human view prints only
warnings and failures; `--verbose` also prints successful checks. JSON always
contains every check in one document.

Warnings are advisory and exit with status 0. Any failed check exits with
status 1. Doctor never applies repairs; follow its suggested existing commands
explicitly. Runtime update discovery remains a separate, explicit operation:

```console
scala runtimes check-updates
```

For normal interactive use, start the TUI directly:

```console
cargo run -p scala
# or explicitly:
cargo run -p scala -- tui
```

### Root helper scripts

The repository includes small wrappers for common build, run, and validation commands. The normal
production workflow is:

```console
./build-production.sh
./run-tui-production.sh
```

Production helpers use Cargo's release profile; development helpers use the development profile:

```console
./build-development.sh
./run-tui-development.sh
./run-server-production.sh
./run-server-development.sh
./validate.sh
```

The TUI normally owns the serving stack automatically, so it does not require a separate `serve`
process. Use `run-server-production.sh` or `run-server-development.sh` only when intentionally
running headless. Build options can be passed to the build helpers, and run options are forwarded
after the `tui` or `serve` subcommand.

Development builds optimize the SHA-256 dependency used for package verification, but production
Scala builds should still use the release profile:

```console
cargo build --release -p scala
./target/release/scala tui
```

The TUI attaches to an existing healthy Scala when one is already running and both its public identity and authenticated private control status can be verified. Otherwise it owns the real public gateway, private control API, runtime manager, and backend lifecycle in the same process for as long as the TUI is open. No second terminal running `serve` is normally required. Load and Unload from the TUI still cross the authenticated private loopback control API. A public-healthy descriptor whose private control status cannot be verified is treated as uncertain ownership: startup fails instead of attaching or starting a competing owner.

For headless/server-only use, run:

```console
cargo run -p scala -- serve
```

Scriptable management commands remain clients of a running instance. With headless `serve` running, another terminal can use:

```console
cargo run -p scala -- model-profiles create coding-large-context --model <MODEL_ID> --engine llama.cpp
cargo run -p scala -- model-profiles set coding-large-context llama.cpp.context_length=131072 llama.cpp.kv_cache_k=q8_0
cargo run -p scala -- model-profiles compatibility coding-large-context
cargo run -p scala -- load coding-large-context
cargo run -p scala -- load coding-large-context --set llama.cpp.parallel_requests=2
cargo run -p scala -- settings show --runtime llama.cpp
cargo run -p scala -- status
cargo run -p scala -- unload coding-large-context
```

`load` contacts the already-running serving process; it does not launch a hidden second server. Explicit loads are pinned, may coexist, and are never removed by JIT cleanup. `unload <PROFILE>` drains and removes only that profile.

Inference is JIT-loaded by Model Profile. With zero resident models, the first Responses or Chat Completions request resolves the profile's exact artifact, engine, runtime, settings, and provenance, waits for startup, and then runs. Requests without Scala attribution share one deterministic default logical session, so changing its primary model drains and replaces the previous unpinned JIT primary. A profile declared `auxiliary` uses the auxiliary cache policy. A request carrying `X-Scala-Role: auxiliary` routes alongside the session primary without changing the backend's Model Profile role or immutable identity.

Both inference endpoints accept two optional orchestration headers: `X-Scala-Session` is an opaque 1–128 byte visible value retained only in memory, and `X-Scala-Role` is exactly `primary` or `auxiliary`. Role overrides the profile default for that request. Session and role metadata are never forwarded to engines or included in immutable provenance. Distinct explicit sessions may retain different primary profiles, while sessions choosing the same profile share one backend. Every attributed request renews an existing session, but only primary requests create a session or change its primary. Any backend held as a live session primary is protected from JIT eviction regardless of its configured profile role.

Private `POST /control/v1/load` is a short authenticated admission request. A successful request reserves a manager generation as Loading, starts a server-owned background operation, and returns `202 Accepted`; the TUI then observes progress and the final state through private status. The scriptable `scala load` command preserves blocking semantics by polling that exact generation until Running or Failed. Disconnecting a CLI or attached TUI does not cancel an accepted load; unloading, server shutdown, or exiting a TUI that owns its server still uses the normal manager cancellation path.

## TUI

Run the TUI with no subcommand or with `tui`:

```console
cargo run -p scala
cargo run -p scala -- tui
```

At startup the TUI safely chooses one of two modes: it attaches to a healthy instance discovered through the existing runtime descriptor, public identity probe, and authenticated private control `status`, or it starts and owns the same serving composition used by headless `serve`. In owned mode the configured OpenAI-compatible endpoint is live while the TUI runs. Exiting shuts down the owned listeners and all managed backends and removes the owned descriptor; exiting an attached TUI leaves the external server running.

Its top-level pages are Overview, Models, Model Profiles, Runtimes, Server, Logs, Settings, and Help. Models is an integrated Model Library: Installed preserves the local artifact/profile/runtime workflow, while Discover provides Hugging Face query editing, format filtering, repository-qualified artifact details, compact revision and size, known companion/package-manifest status, live download bytes, and explicit managed removal. Repository columns and full selected-artifact details distinguish identical filenames and retain their exact repository-qualified download reference. Remote rows remain format candidates; full details explain that compatibility is unverified, and the existing runtime picker continues to provide proven engine/runtime/host compatibility after acquisition. Model Profiles lists, creates, duplicates, deletes, rebinds, edits, validates, and loads user serving targets; it displays missing/incompatible state, active identity, and concrete effective values with source annotations. Settings shows Server, llama.cpp, q27, and NInfer scopes; there is no Global or Common inference page. The Server scope's APPLICATION category carries **Start automatically on login**, a live OS-registration toggle described under [Install and update Scala](#start-automatically-on-login). The Server view and CLI status expose each backend's ordered physical GPU UUID binding. The Runtimes page shows exact format selections and installed packs, then opens an interactive available-runtime search with keyboard filtering, arrow or `j`/`k` movement, mouse hover/click, details, and install actions. Incompatible candidates are hidden by default and can be revealed with the keyboard- and mouse-accessible `Show incompatible` checkbox; Recommended, Compatible, and Needs Attention results remain visible. Result rows and details distinguish upstream binaries from source builds. Release downloads retain real byte progress. Source installs instead expose Checking prerequisites, Fetching source, Verifying source, Configuring, Building, Probing, Installed, or Failed without inventing byte totals. Full installed-runtime details retain commit/tree, recipe, Make or CMake, and CUDA provenance.

Inventory rows use aligned columns; full paths, concrete runtime IDs, provenance and diagnostics are available with **D** (Shift+d) or **[ D Details ]**. Escape returns to the same view; arrows, Page Up/Down, Home/End and the mouse wheel scroll the details. Details capture the selected identity, so a background refresh cannot replace the item being read. This shortcut applies to Overview, Models, Runtimes, Server, Logs and Help, and to runtime dialog results. Model Profiles keeps its existing **D** duplicate action and Settings/Profile editors keep **i** details.

- **Installed models:** **f** edits a local name/path filter. Enter or Escape finishes editing; Escape outside the editor clears the filter, and Ctrl+U clears its input. No search service is involved.
- **Discover:** **f** and the format buttons filter already fetched results. Only explicit Search/Enter requests another upstream search. Exact repository, revision, filename, companions and provenance remain in details.
- **Downloads:** **J** or **[ J Jobs ]** opens the focused job list; **J/Escape** or **[ Esc Back ]** returns. Arrows/wheel select among all jobs, including recent failures. **p** pauses/resumes and **x** cancels only when supported by the job phase. **D** reads the selected job's complete diagnostic. Unknown totals, rates and ETA remain unknown.
- **Logs:** Up/Down reads history, **End** follows latest, and **D** opens complete multiline messages. New entries preserve the history position while follow mode is off.

The 46×13 layout retains selection and essential actions using fewer columns and focused details. Runtime format defaults, explicit model bindings and observed in-use runtimes remain distinct. Server rows associate each resident profile with its own engine/version, lifecycle, request count and private endpoint; full backend/device details remain grouped by profile. [Render review and validation](docs/ui-inventory-review/README.md).


The Models download panel presents active, queued, completed, and failed acquisitions independently
with phase, progress bar, bytes, percentage, measured rate, ETA, and FIFO position whenever those
values are knowable. It remains available after navigating away and returning. The download queue
lives only for the current TUI process and does not survive an application restart; resumable
`.part` cache files do survive, so requesting the interrupted exact acquisition again resumes
through the normal verified path. The CLI `models download` command remains synchronous and waits
for its requested acquisition.

Owned startup establishes the serving and authenticated control stack without waiting for complete local model discovery. The TUI promptly draws its pending first frame, then starts model discovery asynchronously; the existing NotScanned, Scanning, Ready/Ready with warnings, and Failed registry states report real progress. A Model Profile cannot load until its exact bound artifact is discovered. Headless `serve` continues to complete discovery before announcing that it is listening. Neither interactive path performs catalog network I/O merely to start. The editor shows the bound runtime's complete supported schema, and clearing an override immediately reveals the concrete runtime-default value. Runtime help/usage probing runs in the background. Keyboard, mouse/wheel navigation, narrow layout, `NO_COLOR`, and configured ASCII mode remain supported. Edits never hot-mutate a running backend and apply on its next load.

When a model is loading, the TUI shows a polished model-loading progress bar on the Models, Server, and Overview screens. Progress is engine-neutral and flows through the same private control status used by both owned and attached TUI modes. Percentages are shown only when the exact runtime exposes trustworthy measurable progress; otherwise the TUI shows an animated indeterminate bar with meaningful phase text (for example, selecting runtime, revalidating a package, spawning the backend, or verifying startup). Engine-specific log parsing stays inside each adapter; the generic manager owns the progress state. While loading, the TUI polls control status at approximately 200 ms and runs a lightweight render tick for animation; its local admission intent remains fast until the accepted generation is authoritatively observed, then Loading itself keeps the fast cadence. Once loading finishes it returns to the existing slower, event-driven cadence.

## External runtimes and configuration

Managed packs are the normal path, but the existing external llama.cpp configuration remains valid:

```toml
version = 1

[server]
host = "127.0.0.1"
port = 8742
auth = "auto"

[server.jit]
enabled = true
primary_idle_ttl_seconds = 3600
auxiliary_idle_ttl_seconds = 300
max_idle_auxiliary_backends = 2

[models]
paths = ["D:/models"]

[engine."llama.cpp"]
enabled = true

[engine."llama.cpp".settings]
binary_path = "C:/tools/llama.cpp/llama-server.exe"
```

### Model discovery and download destination

The `[models]` table controls two distinct concerns:

- `models.paths` is the list of additional directories scanned for local model
  artifacts. Use it to expose pre-existing model files that live outside
  Scala's managed library (for example a manually maintained GGUF directory on
  another disk). Relative entries resolve against the directory containing
  `config.toml`.
- `models.model_downloads_path` is the destination root for models that Scala
  Server downloads itself. When omitted, downloads land under
  `<Scala data_dir>/models`, preserving the historical managed library root.
  Relative paths resolve against the config directory, matching `paths`. A
  common reason to set it is to place large models on a separate mounted disk.

The effective download destination is always scanned automatically; do not
repeat it inside `models.paths`. Download staging and the download cache stay
under Scala's own data and cache directories and are not configurable. When
`model_downloads_path` is overridden, the previous `<data_dir>/models` location
is no longer an implicit discovery root, but it can still be scanned by adding
it to `models.paths`.

```toml
[models]
model_downloads_path = "/mnt/ai/models"

paths = [
    "/mnt/other-models",
    "/home/user/manual-models",
]
```

With the above, Scala downloads to `/mnt/ai/models`, automatically discovers
models there, and additionally discovers the two `paths` entries.

An external q27 executable can use the same setting under `[engine.q27]`. An advanced external NInfer server uses:

```toml
[engine.ninfer]
enabled = true

[engine.ninfer.settings]
binary_path = "/path/to/ninfer-serve"
```

Relative paths resolve against the directory containing `config.toml`. Observed paths, digests, and usage/help contracts remain exact, but unproved source revisions, build flags, target registries, and hardware targets are never invented; otherwise-usable external runtimes therefore remain needs-attention. Explicit and persisted external selections are still honored when known model/device checks do not prove incompatibility. If an engine section is absent, its built-in adapter remains enabled for managed discovery; setting `enabled = false` disables it.

External binaries participate in the same resolver as managed packs. Scala canonicalizes and probes the executable, records its SHA-256 and observed facts, labels acquisition as `ExternalBinary`, leaves the repository unverified, and treats updates as unmanaged. External runtime manifests are synthesized in memory and are never mistaken for Scala-owned installations.

All adapters keep native configuration engine-namespaced while rejecting flags or variables that can replace Scala-owned model inputs, identity, loopback host/port, authentication, API behavior, observation files, GPU binding, structured settings, or provable generation settings. llama.cpp and q27 raw native arguments are disabled. llama.cpp additionally requires every exact-help option to be classified and dynamically scrubs inherited llama/MTMD/GGML/LLGuidance/AIP controls; q27 rejects configured and scrubs inherited `Q27_*` controls before adding back typed values. NInfer currently admits no raw native option; its ordinary controls are structured. Unrelated ordinary process environment remains inherited, and raw environment values are never placed in provenance.

## Model artifacts and native identity

Discovery recognizes `.gguf`, `.q27`, and `.ninfer` primary artifacts. GGUF discovery reads a
bounded metadata header, not tensor payloads, and records `general.architecture`, model context,
expert/expert-used counts, and a tokenizer-metadata digest when present. This is what permits
architecture-specific expert override and conservative draft-tokenizer proof without filename or
model-family guesses. Q27 admission reads only its fixed 16-byte `Q27F` v1 header and a metadata JSON blob capped at 1 MiB; it never maps or hashes the tensor payload. The current q27 runtime family requires the metadata-declared `qwen35` 65-block/MTP architecture and the upstream shape constants. Published Qwen3.6 tiers are proven from the exact `quant_policy`/`q4_head`/`q8_extra` tuple: default/q4s/q5f require 24 GiB-class, q6/q6f/q6k require 32 GiB-class, and q8 requires 48 GiB-class. Qwen3.8's distinct v2 tuples map q4s/default/q6 to 24 GiB-class and q6k to 32 GiB-class. Its trained-template reasoning-effort capability additionally requires bounded `general.name` metadata matching q27's own normalized `qwen38` selector; this keeps Qwen3.6 and ambiguous fine-tunes from receiving a false capability. A valid architecture with an unknown recipe remains explicit needs-attention rather than being guessed from its filename.

Q27 serving also requires one unambiguous `.tok` companion. An exact same-stem tokenizer is preferred; otherwise discovery may associate a unique boundary-safe prefix match for quantized filenames. The tokenizer must have the current `Q27T` magic/version header. It is recorded as an auxiliary artifact, never listed as an independent model, and does not change the stable primary model ID. During load the adapter canonicalizes and hashes the declared tokenizer into a prepared engine-neutral input. q27-server receives that exact path, and size/SHA-256 are revalidated immediately before launch; the adapter no longer performs a second companion search.

A `.ninfer` file is one self-contained primary artifact; embedded tokenizer, template, frontend, and other resources are not discovered as companions. Admission reads the 16-byte `NINFER\0\x02`/little-endian directory framing and at most 16 MiB of JSON directory metadata, validates object ranges against the actual file, and recovers typed `container_version`, `model_id`, and `weights_id` identity from the container—not its filename. Version 1, bad magic, truncated/absurd directories, malformed closed metadata shapes, and invalid payload ranges fail closed. Discovery never maps or hashes the multi-gigabyte payload and never invokes NInfer or a network. The adapter re-inspects the same bounded identity during preparation and immediately before launch. Managed runtime capabilities are generated from the exact checked-out target registry; an unrecognized registry degrades to needs-attention rather than a hard-coded filename/model allowlist.

## Secure serving and OpenAI-compatible APIs

The public gateway defaults to `127.0.0.1:8742`:

```text
GET  /health
GET  /v1/models
GET  /v1/models/{model}
POST /v1/responses
POST /v1/chat/completions
POST /v1/completions
POST /v1/embeddings
```

Public authentication is configured under `[server]` with `auth = "auto"`, `"required"`, or `"disabled"`:

- `auto` disables public bearer authentication only for a loopback bind and requires it for every non-loopback bind.
- `required` requires a key even on loopback.
- `disabled` is an explicit insecure override. A non-loopback bind is allowed but produces prominent CLI, log, status, and TUI warnings.

When authentication is effective, it applies to `/v1/models`, `/v1/models/{model}`, `/v1/responses`, `/v1/chat/completions`, `/v1/completions`, and `/v1/embeddings`. `/health` remains a minimal unauthenticated liveness endpoint. `serve` validates that at least one active key exists before binding a required-auth public listener.

Create and manage keys with the scriptable CLI:

```console
scala auth status
scala auth keys list
scala auth keys create --name vscode
scala auth keys revoke <KEY_ID>
```

The create command prints a `scala_sk_...` secret exactly once. The versioned store lives at `<data>/api-keys.json`, uses `<data>/.api-keys.lock` plus atomic replacement, and persists only a key ID, label, display prefix, SHA-256 digest, creation time, and revocation time. It never stores the full key. Every authenticated request reads this small mutable state through a blocking boundary, so revocation takes effect without a restart. `--json` is supported; list output never contains secrets or digests.

Useful server configurations are:

```toml
# Default local: no public key required.
[server]
host = "127.0.0.1"
port = 8742
auth = "auto"
```

```toml
# Tailscale/VPN-style bind: at least one active Scala key is required.
[server]
host = "<TAILSCALE_OR_VPN_IP>"
port = 8742
auth = "auto"
```

```toml
# Explicit insecure remote HTTP. Do not use on an untrusted network.
[server]
host = "0.0.0.0"
port = 8742
auth = "disabled"
```

Bearer authentication does not encrypt transport. Loopback needs no network transport layer; Tailscale or another trusted VPN can provide encrypted transport, and a reverse proxy can provide TLS. Plain HTTP on an untrusted LAN exposes bearer credentials, prompts, and outputs. Scala does not manage certificates and does not enable permissive browser CORS.

Each backend and the separately authenticated private control listener use OS-assigned loopback ports. JIT is enabled by default with a 3600-second session/primary idle TTL, a 300-second auxiliary idle TTL, and at most two idle auxiliary JIT backends. Cleanup is LRU/oldest-idle, ignores pinned or active backends, and never evicts the requesting session's leased primary to launch an auxiliary. Public API keys cannot authorize control operations, and clients are never redirected to or given the private upstream server.

The Responses surface accepts `model`; string or message/`function_call`/`function_call_output`
input Items; canonical roles; optional `instructions`; output and sampler controls; reasoning;
function tools; supported image/video content; `text.format`; and `stream`. JSON Schema
name/description/schema/strictness are retained only for a compatible structured-output backend.
Function-call Items preserve call IDs, names, and JSON argument strings. Stateful Responses,
storage, automatic OpenAI truncation, audio, generated-image output, arbitrary local media paths,
and other unimplemented modalities remain explicit errors.

Chat Completions accepts the same canonical messages and generation controls, plus non-null
32-bit `seed`, local `top_k`/`min_p` and non-negative `repeat_penalty` extensions subject to exact
engine/runtime support, string-or-array `stop`, presence/frequency penalties in `-2..=2`,
reasoning effort and local thinking toggle/budget extensions,
`response_format` text/JSON object/JSON Schema, and `max_completion_tokens` with its deprecated
`max_tokens` alias. Equal token-limit aliases are accepted and conflicting aliases fail.
Ordinary function tools use standard assistant `tool_calls` and tool-result history shapes.
Requests for multiple choices, logprobs, audio, stored completions, or stream obfuscation are
rejected. Stream options require `stream=true`. Both public parsers produce the same
engine-neutral `InferenceRequest`; neither endpoint proxies upstream JSON.

Request-time sampler, stop, reasoning, structured-output, and output-limit values are ephemeral and
never mutate profiles, runtime selection, or launch provenance. Explicit request fields override
configured defaults only where the active exact engine/runtime schema proves support. For
llama.cpp, configured samplers and generation defaults become process defaults through advertised
flags; structured-output schemas instead remain per-request defaults sent to its private Chat API.
Responses reports this effective format after defaulting, including configured schemas used for
omitted formats and explicit text/schema overrides. Unsupported runtimes receive a clear 400
rather than an unknown flag or silently ignored request field. q27 request seed/top-k/min-p require
sampled v0.10 execution; q27 accepts only numeric seeds and rejects the common `random` sentinel
instead of treating it as an omitted seed. Its request thinking fields require
`q27.request_thinking`. On a proven Qwen3.8 v0.10.0 combination, persistent
`q27.reasoning_effort` selects the process default `low`, `medium`, or `xhigh` (runtime default
`xhigh`), while request `minimal`/`low`, `medium`, and `high`/`xhigh`/`max` map to those three q27
profiles. Request `none` disables thinking without changing the persistent default only when
`q27.request_thinking` is enabled, and is rejected otherwise. NInfer's
configured reasoning budget is a launch default because its private Chat route has no matching
per-request budget field. System prompt, output limit, sampler, stop, penalty, thinking, and effort
values otherwise act as request defaults where the exact private contract supports them, and an
explicit request wins.

`context_overflow=truncate_middle` is Scala request management, not llama.cpp context shift. It
requires a finite request/profile output allowance, reads the exact effective slot context from the
selected backend, counts the fully rendered chat with that model's tokenizer, preserves every
system/developer instruction and the newest conversational tail, and removes older middle messages
until prompt plus generation allowance fits. If either exact capacity/tokenization is unavailable,
or required content alone cannot fit, the request fails instead of estimating characters.

Every public success, error, and stream carries a fresh opaque `x-request-id`. A valid ASCII `X-Client-Request-Id` of at most 512 characters is retained only as correlation metadata and never replaces the server ID. Public inference JSON is bounded to 32 MiB; oversized bodies receive a clean 413. Errors use one sanitized OpenAI-style envelope and never expose local paths, private endpoints, control tokens, key digests, or Rust debug output.

Example:

```console
curl http://127.0.0.1:8742/v1/responses \
  -H "Authorization: Bearer $SCALA_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"<MODEL_PROFILE_ID>","input":"Reply with exactly: Scala works."}'
```

Chat uses the same key:

```console
curl http://127.0.0.1:8742/v1/chat/completions \
  -H "Authorization: Bearer $SCALA_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"<MODEL_ID>","messages":[{"role":"user","content":"Say hello."}]}'
```

Responses streaming emits the current ordered Scala-generated Responses SSE events and ends in completed, incomplete, or failed state. Chat streaming emits stable `chat.completion.chunk` IDs, assistant/text deltas, a truthful stop/length finish reason, optional known usage, and `[DONE]`. Dropping a client stream drops its owned backend stream rather than leaving detached generation.

For NInfer, Responses and Chat Completions pass through the canonical Scala `InferenceRequest` and private
`/v1/chat/completions` translation; they are never raw-proxied to NInfer's richer APIs. The reviewed
source contract carries seed, top-k/min-p, penalties, stops, thinking/effort, ordinary function
tools, tool history/results, and streaming tool deltas. NInfer supports only `auto`/`none` tool
choice and cannot guarantee single-call mode, so required/named choices, strict functions, and
`parallel_tool_calls=false` are rejected. With exact registered-artifact support and
`ninfer.vision=on`, user images/videos and tool-result images may use bounded HTTP(S) or data URLs;
audio, file paths, assistant/system media, and arbitrary modalities remain unsupported. Structured
output stays false because the audited NInfer source rejects constrained non-text response formats.

Responses remains the primary generation API; Chat Completions is the modern compatibility generation API. `/v1/completions` provides legacy **raw-prompt** compatibility, with no chat template or system-message injection. llama.cpp and exact reviewed q27 runtimes support it, including streaming; NInfer does not. Requests accept one string `prompt`, `model`, `stream`, `stream_options.include_usage`, `max_tokens`, `temperature`, `top_p`, and adapter-supported `top_k`, `min_p`, `seed`, stop strings and penalties. q27 rejects stop strings and repetition/presence/frequency penalties. Non-default `n`/`best_of`, `echo=true`, non-null `logprobs`/`suffix`, non-empty `logit_bias`, token-ID prompts and multiple prompts fail explicitly. `user` is accepted as client metadata without changing inference.

`/v1/embeddings` requires an embedding-capable Model Profile/runtime. Initially only llama.cpp is supported. Bounded inspection of the actual GGUF reads `<architecture>.pooling_type`: mean (1), CLS (2), or last (3) establishes pooled embedding capability. Missing/unknown, none (0), and rank (4) do not. Raw and package-managed artifacts use the same inspection, independently of Builder provenance and filenames. Such profiles launch normally with `--embedding`, without forcing pooling. Runtime inference supplies numeric vectors; Scala formats float arrays or base64 little-endian float32 bytes, preserving input order and indexes. Token accounting is included only when supplied consistently by the runtime.

Embeddings accepts `input` as a non-empty string or non-empty array of non-empty strings, `encoding_format` of `float` (default) or `base64`, and optional `user`. Any supplied `dimensions`, token-array inputs, or unknown fields are rejected. Unsupported model/runtime capabilities fail explicitly with sanitized OpenAI-style errors. Both new inference routes share authentication, request correlation, the 32 MiB body limit, X-Scala routing, JIT loading, and runtime leases.

```sh
curl http://127.0.0.1:8742/v1/embeddings \
  -H "Authorization: Bearer $SCALA_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"<EMBEDDING_MODEL_PROFILE_ID>","input":["First text","Second text"],"encoding_format":"float"}'
```

`GET /v1/models/{model}` retrieves one Model Profile ID with the same fields as the list, or returns an OpenAI-style 404.

Public `/v1/models` remains the minimal OpenAI list (`id`, `object`, `created`, `owned_by`). Use the private local `scala models info <MODEL_ID>` command, with optional `--json`, to inspect compatible engines/runtimes, current resolution and active state, and model/engine serving capabilities without exposing them publicly.

## Lifecycle and provenance

The common runtime manager resolves a concrete runtime before asking its engine adapter for a launch specification. The common supervisor owns process creation, stdout/stderr draining, crash observation, cancellation, bounded shutdown, and cleanup. Load still transitions through Stopped, Loading, Running, Stopping, and Failed; unloading during startup cancels and cleans up the child.

Private control status identifies the model, engine, exact runtime ID/version/variant, executable SHA-256, process, and private endpoint. Launch provenance additionally snapshots the selected profile and the complete concrete effective settings map with runtime-default, Model Profile, or invocation source attribution and optional derivation detail. Startup-confirmed runtime values replace pre-launch calculations without changing the winning layer; when startup changes a value, structured `requested_value` preserves the pre-start policy/value. This is separate from adapter/generation `normalized_settings` and redacted native argument provenance. Provenance also retains the immutable runtime manifest, selection source, accelerator UUID and observations, typed model native identity, auxiliary facts, release digests or source commit/tree/recipe/toolchain facts as appropriate, environment names/value hashes, process identity, endpoint, and launch time. Inference-affecting LoRA/control-vector/context-cost inputs additionally record canonical bound paths and SHA-256 digests; scaled entries retain their scale. Mutable output destinations such as log and slot-save paths are not content-hashed. External, official-binary, and managed-source acquisition are never blurred. Public `/v1/models` and Responses objects receive no profile, runtime, source, hardware, or NInfer native-identity metadata; `models info` exposes the latter locally.

## Per-profile benchmarks

Open `/benchmarks` and press **b Benchmark** to run **Scala Quick Bench v4**
manually for one Model Profile (600 seconds maximum including loading and
finalization). The scorecard shows Intelligence, Agentic, independent executable
Coding /100, Grep-style Retrieval /100, Output TPS, native Prefill TPS, and Latency,
with no composite score. Retrieval uses four original repository tasks and the
canonical four-round grep/glob/read protocol. It requires native parallel tools
and constrained no-tools JSON finalization; unsupported engines show unavailable.
The exact reviewed q27 runtime only constrains tool-call bodies, so its Retrieval
remains unavailable (see [runtime evidence](docs/q27-grep-contract.md)).
Coding uses six original tasks and deterministic all-or-nothing hidden tests in a
bounded, capability-free in-process Rhai evaluator; it is not a full repository
engineering benchmark. Prefill uses actual native processed prompt tokens/time,
never TTFT; insufficient evidence stays unavailable and partial coverage stays
in Details. Historical results are immutable, and unsuccessful attempts do not
hide the previous finished scorecard.

Scriptable access uses private authenticated control:

```console
scala benchmarks start PROFILE_ID
scala benchmarks status --json
scala benchmarks history PROFILE_ID --json
scala benchmarks result RUN_ID --json
scala benchmarks compare LEFT_RUN_ID RIGHT_RUN_ID --json
scala benchmarks cancel
```

Benchmarking temporarily reserves inference. See [benchmark methodology and
operations](docs/benchmarks.md) for exact scoring, task limits, measurement units,
configuration matching, privacy/storage and calibration limitations. These
scores are specific to the bundled pack and are not AA, MMLU or IQ estimates.

## Development

Rust 1.88 or newer is required; `rust-toolchain.toml` pins 1.88.0. Normal validation is:

```console
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Focused tests cover bounded NInfer admission, typed identity, source manifests and current schema validation, exact commit/tree verification, CMake dependency auditing, source history, GPU selection/isolation, settings cross-validation, startup observation, bounded protocol translation, stream cancellation, archive traversal, digest verification, catalog parsing, selection, and existing adapter contracts. Runtime archives, source checkouts/build trees, extracted binaries, model/tokenizer files, caches, generated manifests/selections, logs, control credentials, and build output must not be committed.

## Scala Link

[Scala Link](docs/scala-link.md) makes linked Scala instances available from
either machine through Wayfinder's authenticated peer services. Link machines normally in Wayfinder, set `[link] enabled = true` in Scala
(or run `scala link enable`), and start Scala. Local registration and
reconnection are automatic; Wayfinder needs no application-specific setup. Native
Windows 11 uses a protected same-account named pipe; Linux retains its protected
Unix socket. Ubuntu ↔ Windows federation uses the same Link protocol without WSL.
See the [account requirements and native launch flow](docs/scala-link.md#discovery-and-freshness). The normal
Models, Model Profiles and Overview views show owner/host labels, inventories,
loaded state and owner-reported runtime identity. Load/unload executes on the
selected owner through its normal manager.

Reachable installed peer profiles (including unloaded profiles) appear in `/v1/models` and work through Chat,
Responses, Completions and Embeddings where the owner runtime supports them,
including streaming and cancellation. Unique profile IDs remain simple aliases;
collisions use `<profile-id>@<full-wayfinder-node-id>`, and ambiguous unqualified
requests fail explicitly. Forwarded inference can trigger the owner's normal JIT
load using its local settings and runtime admission; explicit load/unload is optional.
There is no retry, failover, recursive federation, main node or scheduling.

Every node owns its own model files, profiles, runtime installations, runtime
selection/configuration and execution state. Peer reports are cached observations;
no model files, runtime files or authoritative state stores are synchronized.
Runtime screens remain local. Link is disabled by default, and local serving
continues when Wayfinder or a peer is unavailable. See the [setup and protocol
documentation](docs/scala-link.md) and [validation record](docs/scala-link-validation.md).

## Current limitations

- Residency cleanup is TTL/LRU based and does not attempt speculative GPU-memory accounting; an auxiliary load that still cannot fit fails without sacrificing its parent primary.
- Managed q27 is Linux x86_64 CUDA only because those are the currently supported upstream binary/source contracts; Windows can use only a separately supplied compatible external binary.
- Managed source builds are provider-specific: llama.cpp supports the newest exact tagged Linux x86_64 portable CUDA source recipe, q27 supports exact tagged Linux x86_64 CUDA releases whose upstream Makefile contract is recognized, and NInfer supports its exact Linux x86_64/RTX 5090/sm_120a contract. Other providers do not gain source support automatically.
- The llama.cpp Linux managed-source provider exposes two intentional parallel CUDA variants — CUDA-12 `managed-portable-v4` (`>=12.8,<13.0`, driver `>=525.60.13`) and CUDA-13 `managed-portable-cuda13-v2` (`>=13.0,<14.0`, driver `>=580.65.06`); see the runtime documentation above for their exact toolkit, compiler, and target contracts. Scala validates the prerequisite tools but does not install system, CUDA, driver, or toolchain packages.
- Scala does not convert or migrate `.ninfer` model artifacts. The managed Model Library can acquire validated version-2 `.ninfer` containers through the same explicit download/import/remove operations as GGUF and q27; the user may also place supported containers directly in configured model directories.
- The public surface includes Responses, Chat Completions, Completions, Embeddings and health/model listing. Function tools, retrieval and media support remain model/engine/runtime gated. Generated audio/images, arbitrary modalities and stateful Responses are not implemented.
- There is no built-in TLS/certificate management, permissive CORS, rate-limit infrastructure, service installer, or web UI.

See [docs/architecture.md](docs/architecture.md) for component boundaries and the exact runtime acquisition, resolution, and launch flow.
## Reclaiming managed storage

```sh
scala prune
scala prune --dry-run
scala prune --all
scala prune --all --dry-run
scala prune --all --yes
scala --json prune --dry-run
```

Normal prune removes reproducible runtime download packages/provider catalogs,
abandoned runtime staging/trash, incomplete model downloads and acquisition
staging, stale operational descriptors, and obsolete daily logs (keeping the newest
log). Complete managed model acquisitions are durable user artifacts and survive,
even when nothing is loaded.

An installed runtime is removed only when the validated runtime-store scan finds a
strictly newer proven replacement in the same authoritative logical update line,
with matching platform, architecture, accelerator, provider/repository, requirements
and supported-format/native compatibility. Adapter-defined source recipe generations
may supersede older generations on the same source snapshot; intentional functional
variants remain separate. Established logical families can span release and source
packages. Timestamps and identity tie-breakers alone never prove replacement. Explicit selections and update preferences are protected.
Unordered version labels, malformed selections, invalid installations and any pack
without a proven replacement survive. Runtime identity, entrypoint containment and
entrypoint hashes are checked using the existing store validator.

`prune --all` resets generated/heavy storage: all managed model acquisitions in the
library's `huggingface` and `imports` namespaces, managed runtime packs, download
and provider caches, staging, logs and regeneratable runtime state. Generated
runtime selections are removed before packs so they cannot reference deleted
installations. This does **not** delete the application data directory wholesale.
`config.toml`, Server/engine settings, user Model Profiles, API keys/authentication,
external model/runtime configuration and other authored data survive. Preserved
profiles can remain unresolved until their artifacts are reinstalled. Independently
placed model files outside acquisition namespaces are retained. Stable operation
lock files remain so waiting processes cannot acquire a different lock inode.

Both modes refuse while a Server/control or other application storage session is
active. An exclusive storage lease excludes concurrent startup/download sessions;
existing runtime/model operation locks and recorded process IDs are checked too.
An unreachable control endpoint does not authorize deletion of a running process's
storage. Stop the application before pruning. Unknown process state fails closed.

`--all` displays a destructive warning and requires typing `yes`. Non-interactive
execution requires `--yes`; a dry-run never prompts. `--dry-run` uses the same
planner and reports exact deletion targets, categories, file counts and estimated
logical bytes without initializing configuration, logs, directories or lock files.
`--json` returns the structured plan/result. Actual runs also report removed counts
and a measured filesystem free-space delta where available; hard links, sparse
files and concurrent disk activity can make this differ from logical estimates.

Deletion is bounded to application-owned paths and never follows symlinks or
Windows reparse points out of managed storage. Missing paths are harmless. Global
caches, external files, Cargo `target/`, global Cargo caches, and Scala build
storage are never part of Server pruning.
