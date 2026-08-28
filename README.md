# Norted Server

Norted Server is a terminal-first local inference control plane. It discovers local model artifacts, manages separately versioned inference runtimes, owns backend processes, and exposes a narrow OpenAI Responses-compatible text API.

The central distinction is:

```text
engine   = adapter, compatibility rules, launch semantics, health, protocol translation
runtime  = one concrete executable package: version + platform + architecture + backend
```

A GGUF model is not permanently tied to llama.cpp, and a Q27 model is not permanently tied to q27. Selection happens among all installed runtimes whose registered engine can actually use the artifact. Today Norted ships adapters for [llama.cpp](https://github.com/ggml-org/llama.cpp) and [q27](https://github.com/signalnine/q27).

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
├── load <MODEL_ID> [--runtime <RUNTIME_ID>]
├── unload
├── models list
├── runtimes ...
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

The TUI draws its pending first frame before model/runtime scans and never performs catalog network I/O merely to start. Existing keyboard focus, mouse/wheel navigation, responsive layout, `NO_COLOR`, and configured ASCII mode remain supported. Model loading remains an explicit action; selecting a model does not silently change its runtime.

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

Both adapters accept an engine-namespaced native `arguments` array and environment map for ordinary tuning, while rejecting flags or variables that can replace Norted-owned model inputs, identity, loopback host/port, authentication, API behavior, or provable generation settings. Because q27 uses a hand-written positional parser, its custom arguments are limited to the known, non-conflicting q27 v0.6.2 tuning options with their exact separate-token arity (for example, `"--ctx", "8192", "--kv-fp16"`). Raw environment values are never placed in provenance.

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

Private control status identifies the model, engine, exact runtime ID/version/variant, executable SHA-256, process, and private endpoint. Launch provenance additionally retains the immutable runtime manifest, selection source, the exact selected accelerator UUID and observations when one is bound, model path/hash when already known, every materially used auxiliary artifact's role/canonical path/size/SHA-256 (including q27's exact tokenizer), upstream release/revision/assets and verified digests, acquisition method, entrypoint/install time, option-associated hashes for redacted native values, every effective explicit/inherited environment variable name with a value hash, authoritative sampler settings, process identity, and launch time. External and managed acquisition are never blurred. Public `/v1/models` deliberately retains its four OpenAI-style model fields and receives no Norted-specific runtime fields.

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
