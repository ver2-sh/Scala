# Norted Server

Norted Server is a terminal-first local inference control plane. It discovers local model artifacts, manages separately versioned inference runtimes, owns backend processes, and exposes an authenticated OpenAI-compatible text gateway. Responses is the primary API; Chat Completions is a compatibility surface.

The central distinction is:

```text
model artifact = one discovered physical GGUF, q27, or NInfer file
engine         = adapter, compatibility rules, launch semantics, health, protocol translation
runtime        = one concrete executable package and version
settings       = Global and per-engine defaults
Model Profile  = a user-owned serving target binding one artifact, one engine, and overrides
```

A GGUF model is not permanently tied to llama.cpp, a q27 model is not permanently tied to q27,
and an NInfer container is not the NInfer executable itself. Model Profile creation uses registered
engine compatibility, and exact runtime resolution remains separate from the profile. Today Norted
ships adapters for [llama.cpp](https://github.com/ggml-org/llama.cpp),
[q27](https://github.com/signalnine/q27), and [NInfer](https://github.com/Neroued/ninfer).

## Models, Settings, Model Profiles, and Runtimes

- Models is artifact inventory: format, path, size, technical capability, and provenance.
- Settings contains defaults at exactly Global, llama.cpp, NInfer, and q27 scopes.
- Model Profiles are user-created mutable serving targets and the normal load unit.
- Runtimes are installed executable implementations selected for the profile's bound engine.

Two Model Profiles may bind the same artifact and carry different overrides. The active status and
runtime provenance retain both the Model Profile ID and the underlying artifact Model ID. The
OpenAI-compatible model alias is the loaded Model Profile ID, including `GET /v1/models` entries and
the `model` value accepted by inference requests.

Effective settings use one typed system with this complete precedence:

```text
Global defaults
  → selected-engine defaults
  → Model Profile overrides
  → ephemeral invocation overrides
```

Request generation fields may override configured generation defaults when the exact adapter and
runtime support them. There is no model-default layer and no post-resolution hidden policy. An
omitted value emits no corresponding control when omission is the upstream/runtime default.
For llama.cpp semantic alternatives, a higher layer also suppresses inherited siblings: built-in
versus bound-file chat templates, all-versus-exact CPU MoE placement, and draft artifacts made
inapplicable by `off`, n-gram, or `draft-mtp` speculation. Contradictory alternatives in the same
winning layer remain invalid.

Persistent state is deliberately split:

```text
<data>/settings.json         schema 1: Global and engine defaults
<data>/model-profiles.json   schema 1: user-owned Model Profiles
```

Each has its own inter-process lock and atomic replacement. Older combined state is not consumed or
deleted; recreate current settings and profiles explicitly.

## Norted Builder packages

Model paths are served in place. Current q27 and NInfer Builder packages require manifest schema 6;
schema 5 fails closed with rebuild guidance. GGUF package format is unchanged. Discovery binds
genuine artifact members and provenance—primary hashes, tokenizer, projector, Sharp companion,
lineage, and build facts—and performs the existing complete pre-prepare and final pre-spawn
integrity verification.

Builder packages contain no serving preset. Sharp is an artifact companion and is never selected
automatically. A user may explicitly point a q27 Model Profile's external-template setting at the
package Sharp file or any other compatible local template; both use identical path, size, and
SHA-256 safety. Norted-built artifacts receive no special serving configuration or capability
credit. A package artifact and an equivalent raw artifact expose the same controls when their
observed technical capabilities match.

## Typed serving settings

Common and engine-specific settings share `SettingId`, `SettingValue`, `SettingDefinition`,
`SettingsPatch`, `ResolvedSettings`, and setting-source attribution. Presentation categories are
General, Load, Generation, Reasoning, Prompt, KV / Memory, Speculation, Cache, and Advanced. An
engine or Model Profile editor shows common controls plus only that engine's namespace.

llama.cpp exposes load/runtime controls for context and slots; CPU threads and logical/physical
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
benchmark, kernel-selection, and development-only variables remain intentionally native/internal.
On the runtime chat route, audited v0.10 supports ordinary function tools, auto/none/required/named
choice, parallel-call policy, assistant/tool history, and streaming call deltas. External-template
raw completion mode deliberately reports no tool capability. `q27.constrain_tools` strengthens
eligible greedy automatic calls but does not pretend to constrain sampled or forced calls.

NInfer exposes direct settings for context/KV, exact-help-advertised KV formats, CUDA Graph, prefix
reuse, thinking, speculation/backend/draft/proposal head, samplers and greedy mode, queue limits,
request/statistics limits, Vision residency and media budgets/workers, Responses-store bounds, and
advanced cache controls. There is no nested named speculation abstraction. The exact source
commit/tree, executable help, native container v2 identity, revision-specific startup schema,
DFlash/Vision restriction, and observed startup state remain authoritative. The audited current
source advertises `bf16`, `int8`, `fp8`, `nvfp4`, and `k8v4`; older executable help narrows the
choice list instead of inheriting newer formats.
`--context-cost-presets` remains a narrowly allowlisted native file escape hatch until Norted can
bind its canonical path and content identity as strongly as other file-backed settings. NInfer
host/port/key/model alias/device/CORS/request-log controls remain Norted-owned, and diagnostic,
benchmark, tracing, and kernel-development controls are not promoted into ordinary settings.

Structured path values have stable semantics. Absolute paths are used directly. Relative paths
resolve lexically beneath Norted's `<data>` directory and cannot escape it with `..`. Resolution
happens before inspection, validation, adapter translation, and provenance.
Saving `q27.template_path`, `llama.cpp.chat_template_file`, or
`llama.cpp.speculative_draft_model` through the CLI or TUI automatically records its SHA-256;
every load rereads the bound file and rejects a content mismatch. Draft GGUFs must additionally
prove identical bounded tokenizer metadata with the target; the exact llama-server load remains
authoritative for draft architecture/tensor compatibility and is reported as needs-attention until
that definitive load succeeds. Current upstream `draft-mtp` uses MTP heads from the main model and
does not consume an external draft GGUF. Because Norted's bounded GGUF identity does not currently
prove usable MTP heads architecture-neutrally, selecting `draft-mtp` remains needs-attention until
the exact llama-server load proves it.

All commands honor global `--json`. The scriptable management surface includes:

```console
norted-server model-profiles list
norted-server model-profiles show <PROFILE>
norted-server model-profiles create <PROFILE> --model <MODEL_ID> --engine <ENGINE_ID>
norted-server model-profiles duplicate <SOURCE> <PROFILE>
norted-server model-profiles delete <PROFILE>
norted-server model-profiles set-model <PROFILE> <MODEL_ID>
norted-server model-profiles set-engine <PROFILE> <ENGINE_ID>
norted-server model-profiles set <PROFILE> <SETTING=VALUE>...
norted-server model-profiles unset <PROFILE> <SETTING>...
norted-server model-profiles load <PROFILE> [--runtime <RUNTIME_ID>] [--set <SETTING=VALUE>...]
norted-server model-profiles compatibility <PROFILE> [--runtime <RUNTIME_ID>]
norted-server load <PROFILE> [--runtime <RUNTIME_ID>] [--set <SETTING=VALUE>...]
norted-server settings show --global
norted-server settings set --global <SETTING=VALUE>...
norted-server settings unset --global <SETTING>...
norted-server settings show --engine q27
norted-server settings set --engine q27 <SETTING=VALUE>...
norted-server settings unset --engine q27 <SETTING>...
```

## Runtime packs

Norted can search authoritative upstream runtime catalogs, verify and install either release packs or fixed source-build packs, retain multiple versions side by side, select an exact runtime for an artifact format or model, and launch that exact executable. Ordinary upstream releases and compatible NInfer commits are discovered independently of Norted Server releases; a Norted update is needed only when upstream breaks a known packaging, container, build, CLI, or protocol contract.

Installed packs are local truth. Starting the CLI, TUI, `status`, or `serve` never waits for GitHub. Network access occurs only for explicit search/update operations or after the user opens TUI search.

Managed installs live under the platform-native Norted data directory printed by `norted-server config show`:

```text
<data>/runtimes/
  <engine>/<platform-architecture-accelerator-variant>/<version>/<runtime-id>/
    runtime.json
    <upstream package files>

<data>/runtime-selections.json
```

Each managed `runtime.json` is an immutable schema-version-2 record; existing schema-version-1 release manifests remain valid and are not rewritten. Release manifests retain their repository/release/assets and verified archive digest semantics. Source-build manifests instead retain the canonical repository, source ref, exact commit and Git tree, commit timestamp, fixed Norted recipe and build system/arguments/target, observed toolchain/system-package versions, build target platform/architecture/accelerator, build time, entrypoint, and resulting executable SHA-256. They contain no fabricated archive digest or upstream-binary claim.

Selections are mutable preferences and are therefore stored separately. Resolution precedence is:

```text
explicit load --runtime
  → model-specific override
  → artifact-format default
  → best compatible installed fallback
```

Every candidate is checked through the selected engine's model+runtime+host compatibility contract as well as the artifact format, adapter, host, and exact installed manifest. The same decision is used for explicit selection, stored overrides/defaults, candidate listing, automatic fallback, and final load admission. When a persisted selection is missing or invalid, fallback is reported rather than hidden. Selecting an older runtime is the rollback mechanism.

## Official providers and compatibility

Release catalog entries come from the current GitHub Releases API, not a compiled version list. Only uploaded assets whose names and URLs match the authoritative repository contract and which carry a valid GitHub SHA-256 digest are installable. Source catalogs resolve immutable commits through the canonical GitHub repository. Search shows compatible, recommended, needs-attention, or incompatible state without downloading an archive or cloning source.

The llama.cpp provider recognizes these official priority families where an actual release asset exists:

- Windows x86_64 CPU, CUDA, and Vulkan;
- Linux x86_64 CPU and Vulkan.

Windows CUDA releases are composite packages: Norted verifies and extracts both the main llama.cpp archive and the matching official CUDA-runtime archive. The provider searches real assets across releases, so a temporarily incomplete newest build matrix does not create a broken candidate. `Latest` is the newest installable build. `Stable` follows llama.cpp's authoritative stable/nightly pointer when it resolves to a matching published build.

The separate llama.cpp managed-source provider covers Linux x86_64 CUDA because upstream does not publish a comparable Ubuntu CUDA archive. One admitted exact nightly commit/tree exposes two intentional parallel source variants, neither labeled as an upstream CUDA binary: immutable CUDA-12 `managed-portable-v4` requires Toolkit `>=12.8,<13.0` and Linux driver `>=525.60.13`, while `managed-portable-cuda13-v2` requires Toolkit `>=13.0,<14.0` and driver `>=580.65.06`. Both require the stable active-toolkit compiler `/usr/local/cuda/bin/nvcc`, bind CMake to that same compiler, build only `llama-server` with fixed real-code targets `sm_75`, `sm_80`, `sm_86`, `sm_89`, `sm_90`, and `sm_120a`, origin-relative build RPATHs, and exact source/recipe/toolchain/executable provenance. Source manifests distinguish provider-owned recipe arguments from the effective CMake arguments actually executed; the latter include the generated typed compiler binding, while observed nvcc version remains toolchain identity. Historical schema-2 manifests that predate the effective list remain readable without fabricating or rewriting information they did not record. Historical CUDA-12 V1/V2/V3 and CUDA-13 V1 identities remain immutable; CUDA-12 V1 is retained for update discovery but is not an automatic serving candidate because its pre-relocatability recipe can retain stale build-tree library paths. Norted validates but never installs CUDA, the NVIDIA driver, or other toolchain packages.

The q27 provider exposes official Linux x86_64 CUDA releases from v0.2.0 onward. Earlier v0.1.x servers omit the terminal streaming finish reason needed for truthful Responses completion state, so they are deliberately excluded. A release is visible when it has a verified upstream archive or when its exact tagged source proves a supported q27 Makefile recipe; source-only current releases therefore remain searchable without being mislabeled as binaries. When both routes cover the same release and variant, the verified upstream binary wins. Norted creates only the W8, W12, and W16 variants whose upstream targets are present at that revision: `build/q27-server-w8`, default `build/q27-server`, and `build/q27-server-w16`. W8 is automatically preferred for a positively observed 24 GiB-class device and is also the conservative fallback when VRAM is unresolved. W12 is q27's normal/default build and is preferred only when the selected device is positively confirmed as 32 GiB-class or larger. W16 remains an explicit specialist choice and stays last automatically. Because upstream publishes no exact W16 floor, Norted records only the known fact that 24 GiB-class hardware is insufficient; a larger device remains needs-attention rather than being assigned an invented minimum. Stable/Latest follow the newest admitted semantic release, including a supported source-only release. Upstream currently provides no managed Windows q27 route, so Windows reports these entries as incompatible rather than inventing one.

For a q27 source build, discovery resolves the release tag to a full Git commit/tree and inspects bounded exact-revision build and runtime-contract files. Search does not clone or compile. The current v0.10.0 contract fingerprints its provider-reviewed `Makefile`, `README.md`, `src/server.cu`, and `src/engine.cuh`; that evidence proves generic q27 raw-completions, thinking/sampling, MTP/suffix/fast-head, KV-mode, startup-banner, and target-specific W_MAX capabilities. Installation revalidates the tag contract and immutable commit/tree, checks Linux x86_64, Git, Make, the source-declared CUDA floor and `/usr/local/cuda/bin/nvcc`, and the declared host C++ compiler/standard before staging. It checks out that exact commit separately from model-artifact state, requires the checked-out Makefile to match the provider-audited dependency/command closure digest, removes build-control environment overrides, and runs only `make <selected-target>`. The recipe version, Makefile digest, source commit/tree, exact target, toolchain, and built executable digest persist in the installed provenance, so launch admission does not consult mutable upstream state. The resulting executable must be regular, executable, hash-stable, and pass the q27 adapter probe before atomic activation.

NInfer publishes source rather than an installable release binary. Its provider resolves the canonical `Neroued/ninfer` default-branch HEAD into one `Latest` source snapshot containing the full commit and Git-tree SHAs. It deliberately exposes no `Stable` channel and no fake release asset. Revalidation targets the selected commit itself, so a normal later HEAD does not substitute new source; bounded historical source descriptors retained from explicit searches keep that exact selection addressable after a refresh. Update checks use Git ancestry: identical is current, a descendant is an available update, and backward or diverged history is a provider warning rather than an implicit downgrade. Installed snapshots and selections remain side by side and unchanged until explicitly updated/selected.

The currently reviewed NInfer request/startup authority is commit
`21a0e85f8819edc644a3bc036fca6d05cf52ac6e`, tree
`09eda8f77d17d140f57baac89a0259772e51a5f7`, with request-log schema 19.
Executable help can prove an exact launch option without changing request semantics; an unknown
future source snapshot does not inherit reviewed tools/media/request capabilities and remains
NeedsAttention until its protocol/startup changes are audited.

The current official NInfer build contract is Linux x86_64, NVIDIA GeForce RTX 5090, numeric compute capability 12.0 with `sm_120a`, CUDA Toolkit 13.1 or newer, CMake 3.28 or newer, Ninja, a C++20 compiler, pkg-config, FFmpeg development modules, and libcurl. Product name and compute capability are independent observed checks; missing facts are needs-attention and contradictory facts are incompatible. Norted never installs host packages automatically.

Host detection uses the OS and architecture plus a bounded `nvidia-smi` query for every NVIDIA GPU's UUID, name, VRAM, driver, and numeric compute capability when supported; older tools fall back to the otherwise complete query and leave compute capability unknown. An unavailable hardware signal does not stop Norted itself and becomes a needs-attention result, while a successful probe that positively reports no NVIDIA GPU makes a CUDA requirement incompatible. Upstream phrases such as "24 GiB-class" are represented separately from exact byte floors: up to 2 GiB of reporting/ECC/reservation shortfall still counts as the nominal class (matching q27's measured 22.6 GiB A10 case), the next 1 GiB is needs-attention, and a larger shortfall is incompatible. Informational runtime notes remain visible but do not change compatibility; only explicitly unverified conditions produce needs-attention.

Q27 compatibility is evaluated against one concrete observed GPU, and q27-server is launched with `CUDA_VISIBLE_DEVICES` set to that device's full NVIDIA GPU UUID. Official managed q27 fatbin targets are enforced per runtime version before VRAM ranking, so a supported lower-VRAM device beats a larger unsupported device when it meets the runtime and model floors; an unknown compute capability is needs-attention. Norted never treats an `nvidia-smi` number as a CUDA identity. A parent `CUDA_VISIBLE_DEVICES` containing one uniquely resolvable GPU UUID is honored and normalized to the full UUID; empty, numeric, multiple, unknown, or ambiguous constraints make q27 incompatible rather than allowing assessment and launch to diverge. The q27 configuration cannot set this variable because Norted owns the binding. This is single-device binding for q27, not a general GPU scheduler.

NInfer reuses the same exact UUID visibility rules. A managed runtime selects a proven RTX 5090/sm_120a device even when another unsupported GPU is larger, launches with `CUDA_VISIBLE_DEVICES=<full UUID>`, and passes `--device 0`; inside the isolated child, CUDA-local device zero is therefore the selected physical UUID. That same `AcceleratorDevice` is retained in launch provenance. This remains exact single-device binding, not scheduling.

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
cargo run -p norted-server -- runtimes list
cargo run -p norted-server -- runtimes search [QUERY]
cargo run -p norted-server -- runtimes search --refresh
cargo run -p norted-server -- runtimes info <RUNTIME_ID>
cargo run -p norted-server -- runtimes install <RUNTIME_ID>
cargo run -p norted-server -- runtimes remove <RUNTIME_ID>
cargo run -p norted-server -- runtimes check-updates
cargo run -p norted-server -- runtimes update <RUNTIME_ID>
cargo run -p norted-server -- runtimes select --format gguf <RUNTIME_ID>
cargo run -p norted-server -- runtimes select --format gguf --track stable <RUNTIME_ID>
cargo run -p norted-server -- runtimes select --format gguf --track latest <RUNTIME_ID>
cargo run -p norted-server -- runtimes select --format q27 <RUNTIME_ID>
cargo run -p norted-server -- runtimes select --format ninfer <RUNTIME_ID>
cargo run -p norted-server -- runtimes select --model <MODEL_ID> <RUNTIME_ID>
cargo run -p norted-server -- runtimes clear-selection --format gguf
cargo run -p norted-server -- runtimes clear-selection --model <MODEL_ID>
```

Search terms match engines, formats, versions, platforms, architectures, backends, variants, and source acquisition, so queries such as `llama`, `q27`, `ninfer`, `.ninfer`, `CUDA`, `source`, `GGUF`, and `Q27` work. A runtime reference is its stable exact runtime ID, never an internal path. Search opened from the generic Runtimes page remains model-independent. Search opened from a selected model passes that model to every engine adapter and combines catalog, platform, selected-device, runtime-variant, model metadata, and model hardware compatibility before showing recommendations; another engine advertising the same format remains eligible.

Selections default to `--track pinned`. `--track stable` and `--track latest` keep the exact selected runtime in place but constrain future update checks to the provider's truthful channel candidate. Updates remain explicit and side by side; selecting the newly installed runtime is a separate action.

The rest of the command tree remains:

```text
norted-server
├── tui
├── serve
├── status
├── auth status
├── auth keys list|create|revoke
├── load <MODEL_PROFILE_ID> [--runtime <RUNTIME_ID>] [--set <ID=VALUE>]...
├── unload
├── models list|info <MODEL_ID>
├── runtimes ...
├── model-profiles list|show|create|duplicate|delete|set-model|set-engine|set|unset|load|compatibility
├── settings show|set|unset
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
norted-server doctor
norted-server doctor --verbose
norted-server --json doctor
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
norted-server runtimes check-updates
```

For normal interactive use, start the TUI directly:

```console
cargo run -p norted-server
# or explicitly:
cargo run -p norted-server -- tui
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
Norted Server builds should still use the release profile:

```console
cargo build --release -p norted-server
./target/release/norted-server tui
```

The TUI attaches to an existing healthy Norted Server when one is already running and both its public identity and authenticated private control status can be verified. Otherwise it owns the real public gateway, private control API, runtime manager, and backend lifecycle in the same process for as long as the TUI is open. No second terminal running `serve` is normally required. Load and Unload from the TUI still cross the authenticated private loopback control API. A public-healthy descriptor whose private control status cannot be verified is treated as uncertain ownership: startup fails instead of attaching or starting a competing owner.

For headless/server-only use, run:

```console
cargo run -p norted-server -- serve
```

Scriptable management commands remain clients of a running instance. With headless `serve` running, another terminal can use:

```console
cargo run -p norted-server -- model-profiles create coding-large-context --model <MODEL_ID> --engine llama.cpp
cargo run -p norted-server -- model-profiles set coding-large-context context_length=131072 llama.cpp.kv_cache_k=q8_0
cargo run -p norted-server -- model-profiles compatibility coding-large-context
cargo run -p norted-server -- load coding-large-context
cargo run -p norted-server -- load coding-large-context --set parallel_requests=2
cargo run -p norted-server -- settings show --engine llama.cpp
cargo run -p norted-server -- status
cargo run -p norted-server -- unload
```

`load` contacts the already-running serving process; it does not launch a hidden second server. One backend may be active at a time.

Private `POST /control/v1/load` is a short authenticated admission request. A successful request reserves a manager generation as Loading, starts a server-owned background operation, and returns `202 Accepted`; the TUI then observes progress and the final state through private status. The scriptable `norted-server load` command preserves blocking semantics by polling that exact generation until Running or Failed. Disconnecting a CLI or attached TUI does not cancel an accepted load; unloading, server shutdown, or exiting a TUI that owns its server still uses the normal manager cancellation path.

## TUI

Run the TUI with no subcommand or with `tui`:

```console
cargo run -p norted-server
cargo run -p norted-server -- tui
```

At startup the TUI safely chooses one of two modes: it attaches to a healthy instance discovered through the existing runtime descriptor, public identity probe, and authenticated private control `status`, or it starts and owns the same serving composition used by headless `serve`. In owned mode the configured OpenAI-compatible endpoint is live while the TUI runs. Exiting shuts down the owned listeners and active backend and removes the owned descriptor; exiting an attached TUI leaves the external server running.

Its top-level pages are Overview, Models, Model Profiles, Runtimes, Server, Logs, Settings, and Help. Models is artifact inventory and creates profiles rather than loading raw artifacts. Model Profiles lists, creates, duplicates, deletes, rebinds, edits, validates, and loads user serving targets; it displays missing/incompatible state, active identity, and inherited versus overridden values. Settings shows only Global and engine defaults. The Runtimes page shows exact format selections and installed packs, then opens an interactive available-runtime search with keyboard filtering, arrow or `j`/`k` movement, mouse hover/click, details, and install actions. Incompatible candidates are hidden by default and can be revealed with the keyboard- and mouse-accessible `Show incompatible` checkbox; Recommended, Compatible, and Needs Attention results remain visible. Result rows and details distinguish upstream binaries from source builds. Release downloads retain real byte progress. Source installs instead expose Checking prerequisites, Fetching source, Verifying source, Configuring, Building, Probing, Installed, or Failed without inventing byte totals. Installed source-runtime details include short commit/tree, recipe, Make or CMake, and CUDA provenance.

Owned startup establishes the serving and authenticated control stack without waiting for complete local model discovery. The TUI promptly draws its pending first frame, then starts model discovery asynchronously; the existing NotScanned, Scanning, Ready/Ready with warnings, and Failed registry states report real progress. A Model Profile cannot load until its exact bound artifact is discovered. Headless `serve` continues to complete discovery before announcing that it is listening. Neither interactive path performs catalog network I/O merely to start. The editor shows only common settings plus the bound engine namespace, and clearing an override restores inheritance. Runtime help/usage probing runs in the background. Keyboard, mouse/wheel navigation, narrow layout, `NO_COLOR`, and configured ASCII mode remain supported. Edits never hot-mutate a running backend and apply on its next load.

When a model is loading, the TUI shows a polished model-loading progress bar on the Models, Server, and Overview screens. Progress is engine-neutral and flows through the same private control status used by both owned and attached TUI modes. Percentages are shown only when the exact runtime exposes trustworthy measurable progress; otherwise the TUI shows an animated indeterminate bar with meaningful phase text (for example, selecting runtime, revalidating a package, spawning the backend, or verifying startup). Engine-specific log parsing stays inside each adapter; the generic manager owns the progress state. While loading, the TUI polls control status at approximately 200 ms and runs a lightweight render tick for animation; its local admission intent remains fast until the accepted generation is authoritatively observed, then Loading itself keeps the fast cadence. Once loading finishes it returns to the existing slower, event-driven cadence.

## External runtimes and configuration

Managed packs are the normal path, but the existing external llama.cpp configuration remains valid:

```toml
version = 1

[server]
host = "127.0.0.1"
port = 8742
auth = "auto"

[models]
paths = ["D:/models"]

[engine."llama.cpp"]
enabled = true

[engine."llama.cpp".settings]
binary_path = "C:/tools/llama.cpp/llama-server.exe"
```

An external q27 executable can use the same setting under `[engine.q27]`. An advanced external NInfer server uses:

```toml
[engine.ninfer]
enabled = true

[engine.ninfer.settings]
binary_path = "/path/to/ninfer-serve"
```

Relative paths resolve against the directory containing `config.toml`. Observed paths, digests, and usage/help contracts remain exact, but unproved source revisions, build flags, target registries, and hardware targets are never invented; otherwise-usable external runtimes therefore remain needs-attention. Explicit and persisted external selections are still honored when known model/device checks do not prove incompatibility. If an engine section is absent, its built-in adapter remains enabled for managed discovery; setting `enabled = false` disables it.

External binaries participate in the same resolver as managed packs. Norted canonicalizes and probes the executable, records its SHA-256 and observed facts, labels acquisition as `ExternalBinary`, leaves the repository unverified, and treats updates as unmanaged. External runtime manifests are synthesized in memory and are never mistaken for Norted-owned installations.

All adapters keep native configuration engine-namespaced while rejecting flags or variables that can replace Norted-owned model inputs, identity, loopback host/port, authentication, API behavior, observation files, GPU binding, structured settings, or provable generation settings. NInfer additionally uses a strict allowlist of non-semantic operational options; sampler, greedy, vision, CORS, Responses-state, cache, structured-load, and request-log controls remain Norted-owned. A native tuning option remains usable until a resolved structured setting owns the same option; that load then fails with the stable setting ID and conflicting native option. Because q27 uses a hand-written positional parser, its custom arguments are limited to the known, non-conflicting q27 options with their exact separate-token arity. Raw environment values are never placed in provenance.

## Model artifacts and native identity

Discovery recognizes `.gguf`, `.q27`, and `.ninfer` primary artifacts. GGUF discovery reads a
bounded metadata header, not tensor payloads, and records `general.architecture`, model context,
expert/expert-used counts, and a tokenizer-metadata digest when present. This is what permits
architecture-specific expert override and conservative draft-tokenizer proof without filename or
model-family guesses. Q27 admission reads only its fixed 16-byte `Q27F` v1 header and a metadata JSON blob capped at 1 MiB; it never maps or hashes the tensor payload. The current q27 runtime family requires the metadata-declared `qwen35` 65-block/MTP architecture and the upstream shape constants. Published Qwen3.6 tiers are proven from the exact `quant_policy`/`q4_head`/`q8_extra` tuple: default/q4s/q5f require 24 GiB-class, q6/q6f/q6k require 32 GiB-class, and q8 requires 48 GiB-class. Qwen3.8's distinct v2 tuples map q4s/default/q6 to 24 GiB-class and q6k to 32 GiB-class. A valid architecture with an unknown recipe remains explicit needs-attention rather than being guessed from its filename.

Q27 serving also requires one unambiguous `.tok` companion. An exact same-stem tokenizer is preferred; otherwise discovery may associate a unique boundary-safe prefix match for quantized filenames. The tokenizer must have the current `Q27T` magic/version header. It is recorded as an auxiliary artifact, never listed as an independent model, and does not change the stable primary model ID. During load the adapter canonicalizes and hashes the declared tokenizer into a prepared engine-neutral input. q27-server receives that exact path, and size/SHA-256 are revalidated immediately before launch; the adapter no longer performs a second companion search.

A `.ninfer` file is one self-contained primary artifact; embedded tokenizer, template, frontend, and other resources are not discovered as companions. Admission reads the 16-byte `NINFER\0\x02`/little-endian directory framing and at most 16 MiB of JSON directory metadata, validates object ranges against the actual file, and recovers typed `container_version`, `model_id`, and `weights_id` identity from the container—not its filename. Version 1, bad magic, truncated/absurd directories, malformed closed metadata shapes, and invalid payload ranges fail closed. Discovery never maps or hashes the multi-gigabyte payload and never invokes NInfer or a network. The adapter re-inspects the same bounded identity during preparation and immediately before launch. Managed runtime capabilities are generated from the exact checked-out target registry; an unrecognized registry degrades to needs-attention rather than a hard-coded filename/model allowlist.

## Secure serving and OpenAI-compatible text APIs

The public gateway defaults to `127.0.0.1:8742`:

```text
GET  /health
GET  /v1/models
POST /v1/responses
POST /v1/chat/completions
```

Public authentication is configured under `[server]` with `auth = "auto"`, `"required"`, or `"disabled"`:

- `auto` disables public bearer authentication only for a loopback bind and requires it for every non-loopback bind.
- `required` requires a key even on loopback.
- `disabled` is an explicit insecure override. A non-loopback bind is allowed but produces prominent CLI, log, status, and TUI warnings.

When authentication is effective, it applies to `/v1/models`, `/v1/responses`, and `/v1/chat/completions`. `/health` remains a minimal unauthenticated liveness endpoint. `serve` validates that at least one active key exists before binding a required-auth public listener.

Create and manage keys with the scriptable CLI:

```console
norted-server auth status
norted-server auth keys list
norted-server auth keys create --name vscode
norted-server auth keys revoke <KEY_ID>
```

The create command prints a `norted_sk_...` secret exactly once. The versioned store lives at `<data>/api-keys.json`, uses `<data>/.api-keys.lock` plus atomic replacement, and persists only a key ID, label, display prefix, SHA-256 digest, creation time, and revocation time. It never stores the full key. Every authenticated request reads this small mutable state through a blocking boundary, so revocation takes effect without a restart. `--json` is supported; list output never contains secrets or digests.

Useful server configurations are:

```toml
# Default local: no public key required.
[server]
host = "127.0.0.1"
port = 8742
auth = "auto"
```

```toml
# Tailscale/VPN-style bind: at least one active Norted key is required.
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

Bearer authentication does not encrypt transport. Loopback needs no network transport layer; Tailscale or another trusted VPN can provide encrypted transport, and a reverse proxy can provide TLS. Plain HTTP on an untrusted LAN exposes bearer credentials, prompts, and outputs. Norted does not manage certificates and does not enable permissive browser CORS.

Each backend and the separately authenticated private control listener use OS-assigned loopback ports. Public API keys cannot authorize control operations, and clients are never redirected to or given the private upstream server.

The Responses surface accepts `model`; string or message/`function_call`/`function_call_output`
input Items; canonical roles; optional `instructions`; output and sampler controls; reasoning;
function tools; supported image/video content; `text.format`; and `stream`. JSON Schema
name/description/schema/strictness are retained only for a compatible structured-output backend.
Function-call Items preserve call IDs, names, and JSON argument strings. Stateful Responses,
storage, automatic OpenAI truncation, audio, generated-image output, arbitrary local media paths,
and other unimplemented modalities remain explicit errors.

Chat Completions accepts the same canonical messages and generation controls, plus non-null
32-bit `seed`, local `top_k`/`min_p` extensions, string-or-array `stop`, presence/frequency
penalties in `-2..=2`, reasoning effort and local thinking toggle/budget extensions,
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
`q27.request_thinking`. NInfer's
configured reasoning budget is a launch default because its private Chat route has no matching
per-request budget field. System prompt, output limit, sampler, stop, penalty, thinking, and effort
values otherwise act as request defaults where the exact private contract supports them, and an
explicit request wins.

`context_overflow=truncate_middle` is Norted request management, not llama.cpp context shift. It
requires a finite request/profile output allowance, reads the exact effective slot context from the
selected backend, counts the fully rendered chat with that model's tokenizer, preserves every
system/developer instruction and the newest conversational tail, and removes older middle messages
until prompt plus generation allowance fits. If either exact capacity/tokenization is unavailable,
or required content alone cannot fit, the request fails instead of estimating characters.

Every public success, error, and stream carries a fresh opaque `x-request-id`. A valid ASCII `X-Client-Request-Id` of at most 512 characters is retained only as correlation metadata and never replaces the server ID. Public inference JSON is bounded to 32 MiB; oversized bodies receive a clean 413. Errors use one sanitized OpenAI-style envelope and never expose local paths, private endpoints, control tokens, key digests, or Rust debug output.

Example:

```console
curl http://127.0.0.1:8742/v1/responses \
  -H "Authorization: Bearer $NORTED_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"<MODEL_PROFILE_ID>","input":"Reply with exactly: Norted works."}'
```

Chat uses the same key:

```console
curl http://127.0.0.1:8742/v1/chat/completions \
  -H "Authorization: Bearer $NORTED_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"<MODEL_ID>","messages":[{"role":"user","content":"Say hello."}]}'
```

Responses streaming emits the current ordered Norted-generated Responses SSE events and ends in completed, incomplete, or failed state. Chat streaming emits stable `chat.completion.chunk` IDs, assistant/text deltas, a truthful stop/length finish reason, optional known usage, and `[DONE]`. Dropping a client stream drops its owned backend stream rather than leaving detached generation.

For NInfer, both public endpoints pass through the canonical Norted `InferenceRequest` and private
`/v1/chat/completions` translation; they are never raw-proxied to NInfer's richer APIs. The reviewed
source contract carries seed, top-k/min-p, penalties, stops, thinking/effort, ordinary function
tools, tool history/results, and streaming tool deltas. NInfer supports only `auto`/`none` tool
choice and cannot guarantee single-call mode, so required/named choices, strict functions, and
`parallel_tool_calls=false` are rejected. With exact registered-artifact support and
`ninfer.vision=on`, user images/videos and tool-result images may use bounded HTTP(S) or data URLs;
audio, file paths, assistant/system media, and arbitrary modalities remain unsupported. Structured
output stays false because the audited NInfer source rejects constrained non-text response formats.

Public `/v1/models` remains the minimal OpenAI list (`id`, `object`, `created`, `owned_by`). Use the private local `norted-server models info <MODEL_ID>` command, with optional `--json`, to inspect compatible engines/runtimes, current resolution and active state, and model/engine serving capabilities without exposing them publicly.

## Lifecycle and provenance

The common runtime manager resolves a concrete runtime before asking its engine adapter for a launch specification. The common supervisor owns process creation, stdout/stderr draining, crash observation, cancellation, bounded shutdown, and cleanup. Load still transitions through Stopped, Loading, Running, Stopping, and Failed; unloading during startup cancels and cleans up the child.

Private control status identifies the model, engine, exact runtime ID/version/variant, executable SHA-256, process, and private endpoint. Launch provenance additionally snapshots the selected profile and every explicitly resolved structured load value with its global/engine/model/profile/invocation source. Absent upstream-default settings are not recorded as configured. This is separate from existing adapter/generation `normalized_settings` and redacted native argument provenance. Provenance also retains the immutable runtime manifest, selection source, accelerator UUID and observations, typed model native identity, auxiliary facts, release digests or source commit/tree/recipe/toolchain facts as appropriate, environment names/value hashes, process identity, endpoint, and launch time. External, official-binary, and managed-source acquisition are never blurred. Public `/v1/models` and Responses objects receive no profile, runtime, source, hardware, or NInfer native-identity metadata; `models info` exposes the latter locally.

## Development

Rust 1.88 or newer is required; `rust-toolchain.toml` pins 1.88.0. Normal validation is:

```console
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Focused tests cover bounded NInfer admission, typed identity, source manifests and current schema validation, exact commit/tree verification, CMake dependency auditing, source history, GPU selection/isolation, settings cross-validation, startup observation, bounded protocol translation, stream cancellation, archive traversal, digest verification, catalog parsing, selection, and existing adapter contracts. Runtime archives, source checkouts/build trees, extracted binaries, model/tokenizer files, caches, generated manifests/selections, logs, control credentials, and build output must not be committed.

## Current limitations

- Only one model/backend can run at a time; there is no scheduler or implicit swap.
- Managed q27 is Linux x86_64 CUDA only because those are the currently supported upstream binary/source contracts; Windows can use only a separately supplied compatible external binary.
- Managed source builds are provider-specific: llama.cpp supports the newest exact tagged Linux x86_64 portable CUDA source recipe, q27 supports exact tagged Linux x86_64 CUDA releases whose upstream Makefile contract is recognized, and NInfer supports its exact Linux x86_64/RTX 5090/sm_120a contract. Other providers do not gain source support automatically.
- The llama.cpp Linux CUDA V4 build requires Git, CMake 3.18 or newer, Ninja, a C++17 compiler, CUDA Toolkit `>=12.8,<13.0` at `/usr/local/cuda/bin/nvcc`, and compiler support for every fixed target. Driver `525.60.13` or newer is required; absent driver evidence remains needs-attention. Norted validates these tools but does not install system, CUDA, driver, or toolchain packages.
- Norted does not download, convert, or migrate `.ninfer` model artifacts. The user places supported version-2 containers in configured model directories.
- The public surface is Responses and Chat Completions plus health/model listing. Ordinary function tools are exact-engine gated; NInfer image/video input is exact-artifact/runtime/residency gated. Embeddings, audio, generated images, arbitrary modalities, and stateful Responses are not implemented.
- There is no built-in TLS/certificate management, permissive CORS, rate-limit infrastructure, service installer, model downloader, or web UI.

See [docs/architecture.md](docs/architecture.md) for component boundaries and the exact runtime acquisition, resolution, and launch flow.
