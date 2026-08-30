# Norted Server

Norted Server is a terminal-first local inference control plane. It discovers local model artifacts, manages separately versioned inference runtimes, owns backend processes, and exposes an authenticated OpenAI-compatible text gateway. Responses is the primary API; Chat Completions is a compatibility surface.

The central distinction is:

```text
engine   = adapter, compatibility rules, launch semantics, health, protocol translation
runtime  = one concrete executable package: version + platform + architecture + backend
profile  = reusable structured load-setting overrides; it never selects a runtime
load settings = process-start configuration for loading/serving a model
generation settings = request-time sampling/output behavior
```

A GGUF model is not permanently tied to llama.cpp, a Q27 model is not permanently tied to q27, and a NInfer container is not the NInfer executable itself. Selection happens among all installed runtimes whose registered engine can actually use the artifact. Today Norted ships adapters for [llama.cpp](https://github.com/ggml-org/llama.cpp), [q27](https://github.com/signalnine/q27), and [NInfer](https://github.com/Neroued/ninfer).

## Norted Builder packages

Model paths are served in place. When a configured search directory contains a Norted Builder `BUILD-MANIFEST.json`, `q27/Q27-MANIFEST.json`, or `ninfer/NINFER-MANIFEST.json`, discovery binds the declared primary artifacts to their exact tokenizer, projector, Sharp template, runtime policy, hashes, and canonical lineage. It does not copy, move, hardlink, or symlink those files into the Server data directory. `mmproj-F16.gguf` is retained as a projector auxiliary and is not listed as a language model.

Claimed package directories fail closed: an unsupported schema, malformed or oversized JSON, unsafe relative path, symlink escape, missing file, size mismatch, sidecar hash mismatch, duplicate binding, lineage mismatch, or NInfer native-identity mismatch rejects the claimed artifacts instead of reverting to raw serving. Standalone GGUF, q27 plus tokenizer, and NInfer v2 files in directories without the corresponding Norted manifest keep the existing raw-artifact behavior.

Discovery parses at most 16 MiB per manifest/runtime-policy JSON, validates recorded primary sizes, hashes bounded sidecars, and retains the expected primary digest. The complete package primary is SHA-256 verified during explicit preparation and SHA-256 verified again immediately before every actual process launch. Sidecars are revalidated according to the package boundary. Runtime provenance retains the typed package binding and package-derived load-setting source separately from persistent profiles.

Package validity and runtime compatibility are separate. A q27 or NInfer package may be structurally valid even when no installed runtime can satisfy its quality contract. Such a package remains discoverable, but an unproven hard capability is `Incompatible`, not a launchable candidate. `NeedsAttention` is reserved for a runtime that has passed every pre-launch capability gate and needs only bounded startup-observable facts before promotion to Running. Raw artifacts retain their prior launch semantics.

For q27 packages, Server renders the exact manifest-bound Sharp template locally and submits that pre-rendered raw prompt to q27's `/v1/completions` route, avoiding a second chat-template pass. The package response filter suppresses the initial reasoning block and its closing control transition while preserving public answer text, finish reason, and usage. Bounded startup output proves the selected KV mode, served context, numeric compiled `W_MAX`, and thinking/MTP/fast-head profile. Package KV selection follows `fp8`, `turbo5k`, then `turbo3`, restricted to modes proven for the exact executable; only a proven sub-200,000 context result advances to the next supported mode. Official q27 v0.6.2 proves `fp8` and `turbo3` (not `turbo5k`) and numeric W8/W12/W16 `W_MAX`, but remains incompatible because its exact sampler contract lacks the required top-k/min-p capability. Unknown runtimes receive no capability credit from filename or version assumptions.

For NInfer packages, the exact schema-18 `server_start` record can prove model/weights identity, public alias, GPU identity and compute capability, maximum context, physical KV capacity and type, CUDA graph state, prefix reuse, MTP/speculation profile, and sampler defaults. The current exact managed runtime remains incompatible with Dirk-equivalent package execution because applying the required external Sharp template is unsupported/unproven. Startup observation does not compensate for that missing pre-launch mechanism.

Model files continue to be served in place from configured `models.paths`; Norted creates no second model store. Package qualification never copies Builder artifacts or modifies Builder output.

## Load settings and profiles

Load settings are typed, stable Norted IDs. Common settings such as `context_length` and `parallel_requests` are engine-neutral; adapter-owned settings use namespaces such as `llama.cpp.kv_cache_k` and `q27.kv_fp16`. Raw upstream flag spellings remain adapter details. Load profiles never contain a runtime ID or request-time generation controls such as temperature and `top_p`.

Mutable state is schema-versioned at `<data>/load-profiles.json`, outside `config.toml` and immutable runtime manifests. Writes use an inter-process lock and atomic replacement. One model may be assigned one profile. Resolution is:

```text
global defaults
  → selected-engine defaults
  → model defaults
  → assigned or invocation-selected named profile
  → invocation --set overrides
```

Global defaults accept only common IDs. Engine defaults accept common IDs and that engine's namespace. Model defaults and profiles retain settings for multiple engines; only common settings and the selected engine's namespace apply to one launch. `--profile` replaces the persisted model assignment for that load, and repeated `--set` values are ephemeral.

Omission is significant: if no layer sets a value, Norted emits no corresponding argument and the exact upstream runtime uses its own current default. Clearing a value removes it from that layer and falls through. Known upstream defaults are descriptions, not Norted launch values.

Before launch, the adapter validates the resolved values against the exact executable. llama.cpp observations come from that runtime's `--help` and are cached by runtime ID plus entrypoint SHA-256. q27 uses its exact usage signature plus managed-version gates. A structured setting conflicts explicitly with an equivalent native argument (`--flag value` or `--flag=value`) or environment variable rather than relying on ordering. Native arguments remain available when no structured setting owns the same option.

Initial llama.cpp settings are `context_length`, `parallel_requests`, `llama.cpp.threads`, `llama.cpp.batch_size`, `llama.cpp.micro_batch_size`, `llama.cpp.gpu_offload` (`none`, `auto`, `all`, or an exact layer count), `llama.cpp.flash_attention`, separate `llama.cpp.kv_cache_k`/`llama.cpp.kv_cache_v`, and the single semantic `llama.cpp.load_mode`. Its choices are the Norted-understood modes actually advertised by the selected runtime's `--load-mode` help; older runtimes without that contract mark the setting unsupported. An explicit value emits one `--load-mode` option, while omission emits nothing. When configured, it owns the modern option, deprecated mmap/mlock/direct-I/O aliases, and all equivalent llama environment controls, so another input cannot change the effective mode.

Initial q27 settings are `context_length`, `parallel_requests`, one-way `q27.kv_fp16`, two-sided `q27.fast_head`, and the current persistent prefix-cache path/disk/token-step/RAM controls. Explicit context length accepts any positive integer; automatic VRAM-sizing floors are not applied to an explicit value. For managed runtimes, `parallel_requests` is bounded by the selected version's proven conductor limit: 4 through v0.3.0 and 8 from v0.3.1 while that known contract remains applicable. An external q27 usage signature does not prove a ceiling, so Norted enforces only the positive minimum and passes the requested value unchanged. Unsupported exact-runtime contracts remain visible but fail validation rather than being ignored.

Initial NInfer settings include common `context_length` and `parallel_requests` (1–8), plus `ninfer.kv_dtype`, typed integer-or-`auto` `ninfer.kv_capacity`, `ninfer.prefill_chunk`, startup cache capacities, thinking/cache/CUDA-graph flags, and explicit speculative controls. Speculation remains off/upstream-owned when omitted. Selecting MTP requires 1–5 draft tokens; selecting DFlash requires 1–15 and is accepted only for the exact `qwen3.6-35b-a3b/groupwise-int` text target. `ninfer.lm_head_draft` requires an explicit speculative backend. Every mapped option must be advertised by the selected executable's exact `--help` observation.

Structured path values have stable engine-neutral semantics. Absolute paths are used directly. Relative paths resolve lexically beneath Norted's `<data>` directory, do not require the target to exist, and cannot escape that base with `..`; use an absolute path for another location. Resolution happens before effective inspection and adapter translation, so `settings show`, the TUI, the launch argument, and load-settings provenance all contain the same effective path regardless of the server process's working directory.

All commands honor global `--json`. The scriptable management surface is:

```console
norted-server profiles list
norted-server profiles show|create|delete <NAME>
norted-server profiles set <NAME> <ID=VALUE>...
norted-server profiles unset <NAME> <ID>...
norted-server profiles assign --model <MODEL_ID> <NAME>
norted-server profiles clear-assignment --model <MODEL_ID>
norted-server settings set --global <ID=VALUE>...
norted-server settings unset --global <ID>...
norted-server settings set --engine llama.cpp <ID=VALUE>...
norted-server settings unset --engine llama.cpp <ID>...
norted-server settings set --model <MODEL_ID> <ID=VALUE>...
norted-server settings unset --model <MODEL_ID> <ID>...
norted-server settings schema --model <MODEL_ID> [--runtime <RUNTIME_ID>]
norted-server settings show --model <MODEL_ID> [--runtime <RUNTIME_ID>] [--profile <NAME>]
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

The q27 provider exposes official Linux x86_64 CUDA releases from v0.2.0 onward. Earlier v0.1.x servers omit the terminal streaming finish reason needed for truthful Responses completion state, so they are deliberately excluded. A release is visible when it has a verified upstream archive or when its exact tagged source proves a supported q27 Makefile recipe; source-only current releases therefore remain searchable without being mislabeled as binaries. When both routes cover the same release and variant, the verified upstream binary wins. Norted creates only the W8, W12, and W16 variants whose upstream targets are present at that revision: `build/q27-server-w8`, default `build/q27-server`, and `build/q27-server-w16`. W8 is automatically preferred for a positively observed 24 GiB-class device and is also the conservative fallback when VRAM is unresolved. W12 is q27's normal/default build and is preferred only when the selected device is positively confirmed as 32 GiB-class or larger. W16 remains an explicit specialist choice and stays last automatically. Because upstream publishes no exact W16 floor, Norted records only the known fact that 24 GiB-class hardware is insufficient; a larger device remains needs-attention rather than being assigned an invented minimum. Stable/Latest follow the newest admitted semantic release, including a supported source-only release. Upstream currently provides no managed Windows q27 route, so Windows reports these entries as incompatible rather than inventing one.

For a q27 source build, discovery resolves the release tag to a full Git commit/tree and inspects bounded exact-revision build and runtime-contract files. Search does not clone or compile. The current v0.10.0 contract fingerprints its provider-reviewed `Makefile`, `README.md`, `src/server.cu`, and `src/engine.cuh`; that evidence proves the raw-completions, thinking/sampling, MTP/suffix/fast-head, KV-mode, startup-banner, and target-specific W_MAX capabilities needed by Norted packages. Installation revalidates the tag contract and immutable commit/tree, checks Linux x86_64, Git, Make, the source-declared CUDA floor and `/usr/local/cuda/bin/nvcc`, and the declared host C++ compiler/standard before staging. It checks out that exact commit separately from model-artifact state, requires the checked-out Makefile to match the provider-audited dependency/command closure digest, removes build-control environment overrides, and runs only `make <selected-target>`. The recipe version, Makefile digest, source commit/tree, exact target, toolchain, and built executable digest persist in the installed provenance, so launch admission does not consult mutable upstream state. The resulting executable must be regular, executable, hash-stable, and pass the q27 adapter probe before atomic activation.

NInfer publishes source rather than an installable release binary. Its provider resolves the canonical `Neroued/ninfer` default-branch HEAD into one `Latest` source snapshot containing the full commit and Git-tree SHAs. It deliberately exposes no `Stable` channel and no fake release asset. Revalidation targets the selected commit itself, so a normal later HEAD does not substitute new source; bounded historical source descriptors retained from explicit searches keep that exact selection addressable after a refresh. Update checks use Git ancestry: identical is current, a descendant is an available update, and backward or diverged history is a provider warning rather than an implicit downgrade. Installed snapshots and selections remain side by side and unchanged until explicitly updated/selected.

The current official NInfer build contract is Linux x86_64, NVIDIA GeForce RTX 5090, numeric compute capability 12.0 with `sm_120a`, CUDA Toolkit 13.1 or newer, CMake 3.28 or newer, Ninja, a C++20 compiler, pkg-config, FFmpeg development modules, and libcurl. Product name and compute capability are independent observed checks; missing facts are needs-attention and contradictory facts are incompatible. Norted never installs host packages automatically.

Host detection uses the OS and architecture plus a bounded `nvidia-smi` query for every NVIDIA GPU's UUID, name, VRAM, driver, and numeric compute capability when supported; older tools fall back to the otherwise complete query and leave compute capability unknown. An unavailable hardware signal does not stop Norted itself; it becomes a needs-attention result unless a known upstream requirement proves incompatibility. Upstream phrases such as "24 GiB-class" are represented separately from exact byte floors: up to 2 GiB of reporting/ECC/reservation shortfall still counts as the nominal class (matching q27's measured 22.6 GiB A10 case), the next 1 GiB is needs-attention, and a larger shortfall is incompatible. Informational runtime notes remain visible but do not change compatibility; only explicitly unverified conditions produce needs-attention.

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

The fixed recipe enables product apps, disables tests and benchmarks, and builds only `ninfer-serve`; the retained source/build tree supplies the runtime-relative layout upstream expects. Managed build-control variables such as `CC`/`CXX`, CUDA compiler overrides, `CFLAGS`/`CPPFLAGS`/`CXXFLAGS`/`LDFLAGS`, CUDA/NVCC flags, `CMAKE_ARGS`, generator, parallel-level, and toolchain-file overrides are removed while normal tool discovery remains available. The checked CMake tree may use vendored source and system packages but is rejected if it introduces FetchContent, ExternalProject, Git clones, or network downloads. Git/CMake children are direct argument-array processes with drop cancellation; any prerequisite, checkout, configure, build, hash, probe, or activation failure removes staging and creates no installed runtime.

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
├── load <MODEL_ID> [--runtime <RUNTIME_ID>] [--profile <NAME>] [--set <ID=VALUE>]...
├── unload
├── models list|info <MODEL_ID>
├── runtimes ...
├── profiles list|show|create|delete|set|unset|assign|clear-assignment
├── settings show|schema|set|unset
├── engines list       # low-level adapter diagnostics
├── config show
└── doctor
```

For normal interactive use, start the TUI directly:

```console
cargo run -p norted-server
# or explicitly:
cargo run -p norted-server -- tui
```

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
cargo run -p norted-server -- load <MODEL_ID>
cargo run -p norted-server -- profiles create coding-large-context
cargo run -p norted-server -- profiles set coding-large-context context_length=131072 llama.cpp.kv_cache_k=q8_0
cargo run -p norted-server -- profiles assign --model <MODEL_ID> coding-large-context
cargo run -p norted-server -- settings show --model <MODEL_ID>
cargo run -p norted-server -- load <MODEL_ID> --set parallel_requests=2
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

Its top-level pages are Overview, Models, Runtimes, Server, Logs, Settings, and Help. The Runtimes page shows exact format selections and installed packs, then opens an interactive available-runtime search with keyboard filtering, arrow or `j`/`k` movement, mouse hover/click, details, and install actions. Result rows and details distinguish upstream binaries from source builds. Release downloads retain real byte progress. Source installs instead expose Checking prerequisites, Fetching source, Verifying source, Configuring, Building, Probing, Installed, or Failed without inventing byte totals. Installed source-runtime details include short commit/tree, recipe, Make or CMake, and CUDA provenance.

Owned startup establishes the serving and authenticated control stack without waiting for complete local model discovery. The TUI promptly draws its pending first frame, then starts model discovery asynchronously; the existing NotScanned, Scanning, Ready/Ready with warnings, and Failed registry states report real progress. While discovery is pending, the public model list contains only artifacts actually registered so far (normally none), and Load cannot admit a model until it exists in the discovered registry. Headless `serve` continues to complete discovery before announcing that it is listening. Neither interactive path performs catalog network I/O merely to start. Settings is a generic schema-driven editor for global/engine defaults and named profiles; Enter edits or cycles, Delete clears the current layer, and profile creation/deletion is explicit. From Models, `p` opens model defaults, assignment selection, exact-runtime support, and effective value/source inspection. Runtime help/usage probing runs in the background. Keyboard, mouse/wheel navigation, narrow layout, `NO_COLOR`, and configured ASCII mode remain supported. Edits never hot-mutate a running backend and apply on its next load.

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

Discovery recognizes `.gguf`, `.q27`, and `.ninfer` primary artifacts. Q27 admission reads only its fixed 16-byte `Q27F` v1 header and a metadata JSON blob capped at 1 MiB; it never maps or hashes the tensor payload. The current q27 runtime family requires the metadata-declared `qwen35` 65-block/MTP architecture and the upstream shape constants. Published Qwen3.6 tiers are proven from the exact `quant_policy`/`q4_head`/`q8_extra` tuple: default/q4s/q5f require 24 GiB-class, q6/q6f/q6k require 32 GiB-class, and q8 requires 48 GiB-class. Qwen3.8's distinct v2 tuples map q4s/default/q6 to 24 GiB-class and q6k to 32 GiB-class. A valid architecture with an unknown recipe remains explicit needs-attention rather than being guessed from its filename.

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

The Responses text subset accepts `model`; string or text-message `input`; `developer`, `system`, `user`, and `assistant` roles; optional `instructions`; `max_output_tokens`; `temperature`; `top_p`; and `stream`. Identity values such as `store=false`, `background=false`, `tools=[]`, `tool_choice="none"`, plain-text format, `truncation="disabled"`, empty metadata, and null optional fields are accepted where they request no extra behavior. Stateful Responses, storage, non-empty tools, reasoning controls, automatic truncation, structured output, and non-text content are rejected explicitly.

Chat Completions accepts the same canonical text messages and generation controls, plus `max_completion_tokens` and its deprecated `max_tokens` alias. Equal aliases are accepted and conflicting aliases fail. Compatibility identity values include `n=1`, `store=false`, `tools=[]`, `tool_choice="none"`, text-only modality, `stream_options.include_usage`, and explicit `include_obfuscation=false`; requests for multiple choices, tools, logprobs, audio, vision, stored completions, structured output, penalties, stop-sequence behavior, or stream obfuscation are rejected. Stream options require `stream=true`. Both public parsers produce the same engine-neutral `InferenceRequest`; neither endpoint proxies upstream JSON.

Request-time `temperature`, `top_p`, and output-token limits are generation settings, not load settings. They never modify profiles, runtime selection, launch provenance, or backend defaults. llama.cpp receives only explicitly supplied sampler fields and otherwise retains the values observed from `/props`. q27 retains Norted's existing omitted defaults of `temperature=0` and `top_p=1`; because q27 is greedy at temperature zero, an explicit `top_p < 1` requires a positive effective temperature. NInfer likewise sends only explicit sampler fields. Its effective omitted defaults come from the exact schema-18 `server_start` record written by the launched runtime, including thinking mode and server overrides; Norted validates artifact identity, public alias, GPU identity, compute capability, and non-greedy state before readiness. The private restrictive JSONL path is Norted-owned and unlinked after startup observation so later prompts/outputs do not persist as request history. Responses stream options likewise require `stream=true` and accept explicit `include_obfuscation=false`; Norted does not implement OpenAI stream padding/obfuscation. Responses usage remains omitted unless its required shape is known truthfully; Chat emits basic prompt/completion/total counts when the backend reports them.

Every public success, error, and stream carries a fresh opaque `x-request-id`. A valid ASCII `X-Client-Request-Id` of at most 512 characters is retained only as correlation metadata and never replaces the server ID. Public inference JSON is bounded to 32 MiB; oversized bodies receive a clean 413. Errors use one sanitized OpenAI-style envelope and never expose local paths, private endpoints, control tokens, key digests, or Rust debug output.

Example:

```console
curl http://127.0.0.1:8742/v1/responses \
  -H "Authorization: Bearer $NORTED_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"<MODEL_ID>","input":"Reply with exactly: Norted works."}'
```

Chat uses the same key:

```console
curl http://127.0.0.1:8742/v1/chat/completions \
  -H "Authorization: Bearer $NORTED_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"<MODEL_ID>","messages":[{"role":"user","content":"Say hello."}]}'
```

Responses streaming emits the current ordered Norted-generated Responses SSE events and ends in completed, incomplete, or failed state. Chat streaming emits stable `chat.completion.chunk` IDs, assistant/text deltas, a truthful stop/length finish reason, optional known usage, and `[DONE]`. Dropping a client stream drops its owned backend stream rather than leaving detached generation.

For NInfer, both public endpoints still pass through the canonical Norted `InferenceRequest` and private `/v1/chat/completions` translation; they are never raw-proxied to NInfer's richer APIs. Developer/System/User/Assistant order is preserved. Private reasoning text is discarded rather than mixed into the answer, only known usage details are mapped, and bounded SSE requires a supported terminal reason plus `[DONE]`. Norted continues to report tools, vision, structured output, stateful Responses, and Anthropic Messages as unsupported even where upstream NInfer implements them.

Public `/v1/models` remains the minimal OpenAI list (`id`, `object`, `created`, `owned_by`). Use the private local `norted-server models info <MODEL_ID>` command, with optional `--json`, to inspect compatible engines/runtimes, current resolution and active state, and Norted serving capabilities without exposing them publicly.

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

Focused tests cover bounded NInfer admission, typed identity, source manifests and legacy compatibility, exact commit/tree verification, CMake dependency auditing, source history, GPU selection/isolation, settings cross-validation, startup observation, bounded protocol translation, stream cancellation, archive traversal, digest verification, catalog parsing, selection, and existing adapter contracts. Runtime archives, source checkouts/build trees, extracted binaries, model/tokenizer files, caches, generated manifests/selections, logs, control credentials, and build output must not be committed.

## Current limitations

- Only one model/backend can run at a time; there is no scheduler or implicit swap.
- Managed q27 is Linux x86_64 CUDA only because those are the currently supported upstream binary/source contracts; Windows can use only a separately supplied compatible external binary.
- Managed source builds are provider-specific: q27 supports exact tagged Linux x86_64 CUDA releases whose upstream Makefile contract is recognized, while NInfer supports its exact Linux x86_64/RTX 5090/sm_120a contract. Other providers do not gain source support automatically.
- Norted does not download, convert, or migrate `.ninfer` model artifacts. The user places supported version-2 containers in configured model directories.
- The public surface is the documented Responses and Chat Completions text subsets plus health/model listing; tools, embeddings, vision, audio, stateful Responses, and multimodal inference are not implemented.
- There is no built-in TLS/certificate management, permissive CORS, rate-limit infrastructure, service installer, model downloader, or web UI.

See [docs/architecture.md](docs/architecture.md) for component boundaries and the exact runtime acquisition, resolution, and launch flow.
