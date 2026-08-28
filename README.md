# Norted Server

Norted Server is a terminal-first local inference control plane. It discovers local model artifacts, manages separately versioned inference runtimes, owns backend processes, and exposes a narrow OpenAI Responses-compatible text API.

The central distinction is:

```text
engine   = adapter, compatibility rules, launch semantics, health, protocol translation
runtime  = one concrete executable package: version + platform + architecture + backend
profile  = reusable structured load-setting overrides; it never selects a runtime
load settings = process-start configuration for loading/serving a model
generation settings = request-time sampling/output behavior
```

A GGUF model is not permanently tied to llama.cpp, and a Q27 model is not permanently tied to q27. Selection happens among all installed runtimes whose registered engine can actually use the artifact. Today Norted ships adapters for [llama.cpp](https://github.com/ggml-org/llama.cpp) and [q27](https://github.com/signalnine/q27).

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

Norted can search official upstream releases, verify and install a pack, retain multiple versions side by side, select an exact runtime for an artifact format or model, and launch that exact executable. Ordinary upstream runtime releases are discovered independently of Norted Server releases; a Norted update is needed only when upstream breaks the known packaging, CLI, or protocol contract.

Installed packs are local truth. Starting the CLI, TUI, `status`, or `serve` never waits for GitHub. Network access occurs only for explicit search/update operations or after the user opens TUI search.

Managed installs live under the platform-native Norted data directory printed by `norted-server config show`:

```text
<data>/runtimes/
  <engine>/<platform-architecture-accelerator-variant>/<version>/<runtime-id>/
    runtime.json
    <upstream package files>

<data>/runtime-selections.json
```

Each managed `runtime.json` is an immutable schema-version-1 record. It identifies the engine and package family, upstream version/revision, platform, architecture, accelerator and variant, repository/release/assets, acquisition method, verified archive digest(s), exact entrypoint and its SHA-256, install time, supported artifact formats, and successful adapter probe. A different digest presented under the same claimed release identity is a provenance conflict, never an in-place replacement.

Selections are mutable preferences and are therefore stored separately. Resolution precedence is:

```text
explicit load --runtime
  → model-specific override
  → artifact-format default
  → best compatible installed fallback
```

Every candidate is checked through the selected engine's model+runtime+host compatibility contract as well as the artifact format, adapter, host, and exact installed manifest. The same decision is used for explicit selection, stored overrides/defaults, candidate listing, automatic fallback, and final load admission. When a persisted selection is missing or invalid, fallback is reported rather than hidden. Selecting an older runtime is the rollback mechanism.

## Official providers and compatibility

Catalog entries come from the current GitHub Releases API, not a compiled version list. Only uploaded assets whose names and URLs match the authoritative repository contract and which carry a valid GitHub SHA-256 digest are installable. Search shows compatible, recommended, needs-attention, or incompatible state without downloading an archive.

The llama.cpp provider recognizes these official priority families where an actual release asset exists:

- Windows x86_64 CPU, CUDA, and Vulkan;
- Linux x86_64 CPU and Vulkan.

Windows CUDA releases are composite packages: Norted verifies and extracts both the main llama.cpp archive and the matching official CUDA-runtime archive. The provider searches real assets across releases, so a temporarily incomplete newest build matrix does not create a broken candidate. `Latest` is the newest installable build. `Stable` follows llama.cpp's authoritative stable/nightly pointer when it resolves to a matching published build.

The q27 provider currently exposes only official Linux x86_64 CUDA packages from v0.2.0 onward. Earlier v0.1.x servers omit the terminal streaming finish reason needed for truthful Responses completion state, so they are deliberately excluded. Norted creates selectable W8, W12, and W16 runtime variants. W8 is automatically preferred for a positively observed 24 GiB-class device and is also the conservative fallback when VRAM is unresolved. W12 is q27's normal/default build and is preferred only when the selected device is positively confirmed as 32 GiB-class or larger. W16 remains an explicit specialist choice for repetition-heavy/file-re-emission work and stays last automatically. Because upstream publishes no exact W16 floor, Norted records only the known fact that 24 GiB-class hardware is insufficient; a larger device remains needs-attention rather than being assigned an invented minimum. Recent q27 releases that contain source only are not presented as managed binaries; the latest *installable* release receives the Stable/Latest channel. Upstream currently publishes no managed Windows q27 package, so Windows correctly reports those catalog entries as incompatible. Norted does not invent a Windows package or a managed source-build path.

Host detection uses the OS and architecture plus a bounded `nvidia-smi` query for every NVIDIA GPU's UUID, name, VRAM, driver, and numeric compute capability when supported; older tools fall back to the otherwise complete query and leave compute capability unknown. An unavailable hardware signal does not stop Norted itself; it becomes a needs-attention result unless a known upstream requirement proves incompatibility. Upstream phrases such as "24 GiB-class" are represented separately from exact byte floors: up to 2 GiB of reporting/ECC/reservation shortfall still counts as the nominal class (matching q27's measured 22.6 GiB A10 case), the next 1 GiB is needs-attention, and a larger shortfall is incompatible. Informational runtime notes remain visible but do not change compatibility; only explicitly unverified conditions produce needs-attention.

Q27 compatibility is evaluated against one concrete observed GPU, and q27-server is launched with `CUDA_VISIBLE_DEVICES` set to that device's full NVIDIA GPU UUID. Official managed q27 fatbin targets are enforced per runtime version before VRAM ranking, so a supported lower-VRAM device beats a larger unsupported device when it meets the runtime and model floors; an unknown compute capability is needs-attention. Norted never treats an `nvidia-smi` number as a CUDA identity. A parent `CUDA_VISIBLE_DEVICES` containing one uniquely resolvable GPU UUID is honored and normalized to the full UUID; empty, numeric, multiple, unknown, or ambiguous constraints make q27 incompatible rather than allowing assessment and launch to diverge. The q27 configuration cannot set this variable because Norted owns the binding. This is single-device binding for q27, not a general GPU scheduler.

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

GitHub redirects are accepted only among the exact GitHub API/release-asset hosts. API tokens from `GITHUB_TOKEN` or `GH_TOKEN` are optional, are sent only to the GitHub API client, and are neither logged nor persisted. A missing trustworthy digest makes a pack non-installable. Partial downloads are never valid cache entries.

Extraction rejects absolute paths, parent traversal, Windows drive/backslash tricks, duplicate archive paths, special entries, oversized archives, and links that escape staging. The official Linux llama.cpp archives use relative symlinks; those are accepted only after their resolved targets are proven to remain inside staging. A download, extraction, or probe failure removes staging and cannot create an Installed runtime.

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
cargo run -p norted-server -- runtimes select --model <MODEL_ID> <RUNTIME_ID>
cargo run -p norted-server -- runtimes clear-selection --format gguf
cargo run -p norted-server -- runtimes clear-selection --model <MODEL_ID>
```

Search terms match engines, formats, versions, platforms, architectures, backends, and variants, so queries such as `llama`, `q27`, `CUDA`, `Vulkan`, `GGUF`, and `Q27` work. A runtime reference is its stable exact runtime ID, never an internal path. Search opened from the generic Runtimes page remains model-independent. Search opened from a selected model passes that model to every engine adapter and combines catalog, platform, selected-device, runtime-variant, model metadata, and model hardware compatibility before showing recommendations; another engine advertising the same format remains eligible.

Selections default to `--track pinned`. `--track stable` and `--track latest` keep the exact selected runtime in place but constrain future update checks to the provider's truthful channel candidate. Updates remain explicit and side by side; selecting the newly installed runtime is a separate action.

The rest of the command tree remains:

```text
norted-server
├── tui
├── serve
├── status
├── load <MODEL_ID> [--runtime <RUNTIME_ID>] [--profile <NAME>] [--set <ID=VALUE>]...
├── unload
├── models list
├── runtimes ...
├── profiles list|show|create|delete|set|unset|assign|clear-assignment
├── settings show|schema|set|unset
├── engines list       # low-level adapter diagnostics
├── config show
└── doctor
```

Typical use is:

```console
cargo run -p norted-server -- models list
cargo run -p norted-server -- runtimes search gguf
cargo run -p norted-server -- runtimes install <RUNTIME_ID>
cargo run -p norted-server -- runtimes select --format gguf <RUNTIME_ID>
cargo run -p norted-server -- serve
```

Then, in another terminal:

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

## TUI

Run the TUI with no subcommand or with `tui`:

```console
cargo run -p norted-server
cargo run -p norted-server -- tui
```

Its top-level pages are Overview, Models, Runtimes, Server, Logs, Settings, and Help. The Runtimes page shows exact format selections and installed packs, then opens an interactive available-runtime search with keyboard filtering, arrow or `j`/`k` movement, mouse hover/click, details, and install actions. Downloads and extraction run asynchronously and expose Downloading, Verifying, Extracting, Probing, Installed, or Failed state with real byte counts when available.

The TUI draws its pending first frame before model/runtime scans and never performs catalog network I/O merely to start. Settings is a generic schema-driven editor for global/engine defaults and named profiles; Enter edits or cycles, Delete clears the current layer, and profile creation/deletion is explicit. From Models, `p` opens model defaults, assignment selection, exact-runtime support, and effective value/source inspection. Runtime help/usage probing runs in the background. Keyboard, mouse/wheel navigation, narrow layout, `NO_COLOR`, and configured ASCII mode remain supported. Edits never hot-mutate a running backend and apply on its next load.

## External runtimes and configuration

Managed packs are the normal path, but the existing external llama.cpp configuration remains valid:

```toml
version = 1

[server]
host = "127.0.0.1"
port = 8742

[models]
paths = ["D:/models"]

[engine."llama.cpp"]
enabled = true

[engine."llama.cpp".settings]
binary_path = "C:/tools/llama.cpp/llama-server.exe"
```

An external q27 executable can use the same setting under `[engine.q27]`. Relative paths resolve against the directory containing `config.toml`. Its observed path, digest, and usage contract remain exact, but its build width, runtime-specific VRAM floor, and compiled CUDA targets are not guessed; otherwise-usable external q27 selections therefore remain needs-attention. Explicit and persisted external selections are still honored when known model/device checks do not prove incompatibility. If an engine section is absent, its built-in adapter remains enabled for managed discovery; setting `enabled = false` disables it.

External binaries participate in the same resolver as managed packs. Norted canonicalizes and probes the executable, records its SHA-256 and observed facts, labels acquisition as `ExternalBinary`, leaves the repository unverified, and treats updates as unmanaged. External runtime manifests are synthesized in memory and are never mistaken for Norted-owned installations.

Both adapters accept an engine-namespaced native `arguments` array and environment map for ordinary tuning, while rejecting flags or variables that can replace Norted-owned model inputs, identity, loopback host/port, authentication, API behavior, or provable generation settings. A native tuning option remains usable until a resolved structured setting owns the same option; that load then fails with the stable setting ID and conflicting native option. Because q27 uses a hand-written positional parser, its custom arguments are limited to the known, non-conflicting q27 v0.6.2 tuning options with their exact separate-token arity (for example, `"--ctx", "8192", "--kv-fp16"`). Raw environment values are never placed in provenance.

## Models and q27 companions

Discovery recognizes `.gguf` and `.q27` primary artifacts. Q27 admission reads only its fixed 16-byte `Q27F` v1 header and a metadata JSON blob capped at 1 MiB; it never maps or hashes the tensor payload. The current q27 runtime family requires the metadata-declared `qwen35` 65-block/MTP architecture and the upstream shape constants. Published Qwen3.6 tiers are proven from the exact `quant_policy`/`q4_head`/`q8_extra` tuple: default/q4s/q5f require 24 GiB-class, q6/q6f/q6k require 32 GiB-class, and q8 requires 48 GiB-class. Qwen3.8's distinct v2 tuples map q4s/default/q6 to 24 GiB-class and q6k to 32 GiB-class. A valid architecture with an unknown recipe remains explicit needs-attention rather than being guessed from its filename.

Q27 serving also requires one unambiguous `.tok` companion. An exact same-stem tokenizer is preferred; otherwise discovery may associate a unique boundary-safe prefix match for quantized filenames. The tokenizer must have the current `Q27T` magic/version header. It is recorded as an auxiliary artifact, never listed as an independent model, and does not change the stable primary model ID. During load the adapter canonicalizes and hashes the declared tokenizer into a prepared engine-neutral input. q27-server receives that exact path, and size/SHA-256 are revalidated immediately before launch; the adapter no longer performs a second companion search.

## Serving and Responses subset

The public gateway defaults to `127.0.0.1:8742`:

```text
GET  /health
GET  /v1/models
POST /v1/responses
```

The public API has no authentication in this release, so keep it on loopback unless the surrounding network is protected. Each backend and the authenticated control listener use separate OS-assigned ports on `127.0.0.1`; clients are never redirected to an upstream server.

The Responses text subset accepts `model`, string or text-message `input`, optional `instructions`, positive `max_output_tokens`, and `stream`. Unsupported fields and non-text inputs are rejected rather than ignored. Norted translates both llama.cpp and q27 Chat Completions JSON/SSE into its canonical internal inference events and constructs public Responses objects itself; upstream bytes are not proxied through.

For q27, Norted explicitly owns `temperature = 0.0` and `top_p = 1.0` on every request and strips conflicting inherited environment controls, so the reported effective settings are provable. For llama.cpp, settings are read from its authoritative `/props` after readiness. Public token usage is omitted unless every required Responses detail is actually known.

Example:

```console
curl http://127.0.0.1:8742/v1/responses \
  -H "Content-Type: application/json" \
  -d '{"model":"<MODEL_ID>","input":"Reply with exactly: Norted works."}'
```

Streaming emits Norted-generated Responses SSE events and ends in completed, incomplete, or failed state. llama.cpp and q27 health/readiness, native requests, and stream parsing remain isolated in their adapters.

## Lifecycle and provenance

The common runtime manager resolves a concrete runtime before asking its engine adapter for a launch specification. The common supervisor owns process creation, stdout/stderr draining, crash observation, cancellation, bounded shutdown, and cleanup. Load still transitions through Stopped, Loading, Running, Stopping, and Failed; unloading during startup cancels and cleans up the child.

Private control status identifies the model, engine, exact runtime ID/version/variant, executable SHA-256, process, and private endpoint. Launch provenance additionally snapshots the selected profile and every explicitly resolved structured load value with its global/engine/model/profile/invocation source. Absent upstream-default settings are not recorded as configured. This is separate from existing adapter/generation `normalized_settings` and redacted native argument provenance. Provenance also retains the immutable runtime manifest, selection source, accelerator UUID and observations, model/tokenizer identity, upstream release and verified digests, environment names/value hashes, process identity, endpoint, and launch time. External and managed acquisition are never blurred. Public `/v1/models` and Responses objects receive no profile metadata.

## Development

Rust 1.88 or newer is required; `rust-toolchain.toml` pins 1.88.0. Normal validation is:

```console
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Focused tests cover security-sensitive archive traversal, digest verification, failed activation, catalog parsing, selection, and adapter contracts. Runtime archives, extracted binaries, model/tokenizer files, caches, generated manifests/selections, logs, control credentials, and build output must not be committed.

## Current limitations

- Only one model/backend can run at a time; there is no scheduler or implicit swap.
- Managed q27 is Linux x86_64 CUDA only because that is what upstream currently publishes; Windows can use only a separately supplied compatible external binary.
- q27 source-only releases are not managed builds, and Norted does not compile runtime packs from source.
- The public surface is the documented Responses text subset plus health/model listing; tools, embeddings, vision, audio, and multimodal inference are not implemented.
- There is no public API authentication, service installer, model downloader, or web UI.

See [docs/architecture.md](docs/architecture.md) for component boundaries and the exact runtime acquisition, resolution, and launch flow.
