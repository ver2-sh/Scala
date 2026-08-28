# Architecture

Norted Server is an engine-agnostic local inference control plane composed by the `norted-server` binary. It owns model discovery, runtime acquisition and selection, process lifecycle, normalized inference, provenance, and the public protocol. Upstream executables remain separately versioned runtime packs.

## Engine and runtime boundaries

An `EngineAdapter` represents implementation knowledge: identity, supported artifact formats and capabilities, model compatibility, runtime probing, launch semantics, readiness, effective settings, and request/event translation. It never implies that only one executable exists.

An `InstalledRuntime` represents one executable package and immutable provenance. Launch input is therefore:

```text
ModelArtifact + InstalledRuntime + private address
                     │
                     ▼
                EngineAdapter
                     │
                     ▼
                  LaunchSpec
```

`EngineRegistry` contains the llama.cpp and q27 adapters. `RuntimePackManager` finds installed runtimes whose engine adapter and declared formats can serve the model, applies the selection policy, and hands the selected exact runtime to the adapter. No core branch says that GGUF always means llama.cpp or that Q27 always means q27; additional adapters can advertise either format without changing the resolver.

The serving and security boundaries are:

```text
PUBLIC CLIENT
    │ HTTP Bearer authentication when effective
    ▼
PUBLIC NORTED GATEWAY
    ├── GET  /health                 (minimal liveness, never key-authorized control)
    ├── GET  /v1/models              ┐
    ├── POST /v1/responses           ├ authenticated together when required
    └── POST /v1/chat/completions    ┘
              │
       Responses parser ─┐
       Chat parser ──────┴──▶ canonical InferenceRequest
                                      │
                                      ▼
                               RuntimeManager
                                      │ selected EngineAdapter
                                      ▼
                         127.0.0.1:<dynamic private backend>
                              llama-server or q27-server

LOCAL CONTROL CLIENT (CLI / TUI)
    │ distinct random control bearer token from private descriptor
    ▼
PRIVATE LOOPBACK CONTROL API
```

The public listener is configurable and defaults to `127.0.0.1:8742`. `auto` auth is disabled only on loopback and required on every non-loopback address; `required` always requires it, while `disabled` is an explicit insecure override. Backend and authenticated control listeners are always separate loopback endpoints with different credentials. Public keys cannot authorize control requests, and clients are never redirected to an upstream backend.

## Crate responsibilities

- `norted-core` owns platform paths, schema-version-1 configuration, public-auth policy, the versioned atomic API-key store, the typed load-setting/profile domain and five-layer resolver, the versioned atomic profile store, model/auxiliary-artifact discovery, stable IDs, runtime identity/manifest/preferences data, host-independent provenance, application state, and process descriptors.
- `norted-engine` owns `EngineAdapter`, `EngineRegistry`, the provider/catalog/cache, secure installer, runtime store and resolver, runtime/backend manager, generic process supervisor, control client, and normalized inference types.
- `norted-engine-llama-cpp` owns official llama.cpp asset classification, binary probes, flags/environment policy, readiness and `/props`, and Chat Completions JSON/SSE translation.
- `norted-engine-q27` owns official q27 asset/variant classification, tokenizer requirements, usage-signature probes, flags/environment policy, readiness, provable sampler settings, and Chat Completions JSON/SSE translation.
- `norted-api` owns public bearer/request middleware, the shared sanitized error contract, Responses and Chat Completions text surfaces, and the private authenticated control HTTP surface. It does not construct upstream flags or expose backend-native bytes.
- `norted-tui` owns terminal lifecycle, responsive rendering, runtime search/install/selection interaction, the generic schema-driven load-settings editor, and normalized control observation.
- `norted-server` is the composition root and scriptable CLI.

Adapter registration and provider registration are separate. A built-in engine can be enabled even when no runtime is installed; conversely, every installed manifest still requires its corresponding registered adapter before it can launch.

## Load-setting resolution

`Engine`, `Runtime`, `LoadProfile`, and request-time generation settings are distinct domains. `LoadSettingDefinition` describes a stable common or engine-namespaced ID, typed value kind, constraints/choices, documentation, and exact-runtime support. `LoadProfilesState` schema 1 lives at `<data>/load-profiles.json`; it is mutable user state, uses a dedicated cross-process lock plus atomic replacement, and is never written into TOML or a runtime manifest.

One core resolver applies global, selected-engine, model, named-profile, then invocation patches. It filters unrelated engine namespaces while retaining them in model/profile state, records the winning source for every value, and produces no entry for an upstream default. An invocation profile replaces the model assignment for that load. Invocation values are never written back. As the final engine-neutral resolution step, relative structured `Path` values are lexically resolved beneath the absolute application data directory and parent traversal outside that base is rejected; absolute values remain valid. No filesystem canonicalization is required, so an upstream-created target may be absent. Every downstream consumer therefore receives the same absolute effective path.

Runtime selection still happens first. The selected adapter observes the exact executable interface, returns an exact `LoadSettingsSchema`, validates resolved values, detects native argument/environment collisions, and translates only explicit values into arguments/removals. llama.cpp help observations are keyed by exact runtime ID and entrypoint hash. Its one `llama.cpp.load_mode` choice schema is intersected with the modes advertised by that exact executable; configured ownership covers `-lm`/`--load-mode`, deprecated mmap/mlock/direct-I/O aliases, and their positive/negative compatibility environments. q27 combines exact usage observation with known managed-version gates: managed versions before v0.3.1 expose a 4-slot maximum, v0.3.1 and later known contracts expose 8, and external binaries expose no unproven maximum. Explicit q27 context retains the common positive-integer constraint rather than automatic-sizing floors. `LaunchRequest` carries the prepared model, exact runtime, selected accelerator, private address, resolved settings, and exact schema; adapters do not read profile files.

`RuntimeProvenance.profile` remains the selected profile name. The separate `load_settings.effective` snapshot stores stable typed values and precedence sources. Because adapters translate and return this same resolved snapshot in `LaunchSpec`, a structured path shown in effective inspection is byte-for-byte the path passed to the runtime and retained in provenance. Existing normalized adapter/generation facts and redacted native arguments keep their prior meanings.

## Runtime identity and store

`RuntimeIdentity` is structured and hashed into a stable filesystem-safe `RuntimeId`. Its identity fields are:

```text
engine ID
package family
upstream version/tag
upstream revision when authoritative
platform
architecture
accelerator/backend
variant
provider + repository + release + primary/additional asset identity
```

The application-data store is versioned side by side:

```text
<data>/runtimes/
  <engine>/
    <platform-architecture-accelerator-variant>/
      <version>/
        <runtime-id>/
          runtime.json
          <package contents>
  .staging/<random-id>/...
```

`runtime.json` schema 1 adds supported formats, acquisition method, source URL, verified primary/additional archive SHA-256 values, contained entrypoint, entrypoint SHA-256, install time, and the successful probe observation. Managed entrypoints are relative to their installation root. An explicitly configured external binary is represented by an in-memory external manifest with an absolute canonical entrypoint, no fabricated archive or install-time fields, and unverified repository provenance.

Scanning validates every manifest, canonicalizes the root and entrypoint, and rejects containment failures, duplicate IDs, invalid schemas, failed probe records, and unsafe staging paths. Removal recomputes the expected directory from the manifest, canonicalizes it under the runtime root, rejects staging/external targets, and removes only that exact owned directory.

Mutable `runtime-selections.json` schema 1 is adjacent to, not inside, the immutable installations. It contains exact format defaults, exact model overrides, and update preferences. Atomic temporary-file replacement is used for preferences and catalog cache data; neither is written into user TOML.

## Live runtime catalog

`RuntimeCatalogProvider` produces engine-neutral `AvailableRuntime` values. `RuntimeCatalog` aggregates providers and attaches local installed/selected and host compatibility state. Provider results are cached per provider in the application cache directory with a bounded freshness window; a fresh cache avoids API calls, and a stale cache can support explicit error reporting. Normal startup never fetches it.

`GitHubReleaseClient` requests the newest 100 releases from the official repository for broad browsing. When a concrete older tag/runtime ID is requested or an installed exact runtime falls outside that window, the matching provider resolves GitHub's exact tag endpoint on demand. This keeps ordinary searches bounded without falsely declaring an older published pin missing. Optional `GITHUB_TOKEN`/`GH_TOKEN` authentication is confined to this API client. Catalog entries carry exact repository, release/tag, asset ID/name/URL/size/digest, channel, and requirements. The installer independently revalidates URL authority and digest presence.

### llama.cpp

The official provider classifies asset names rather than assuming a release matrix. Each platform/architecture/backend line receives its own newest actual `Latest` candidate. Windows CUDA requires the matching same-release `cudart` asset and both identities/downloads become part of the runtime identity. Stable follows the verified semantic-release `nightly-tag.txt` pointer, including its digest and commit relationship. Drafts, non-uploaded files, malformed/missing asset relationships, unknown grammars, and unsupported variants are not guessed.

Initial supported families include Windows x86_64 CPU/CUDA/Vulkan and Linux x86_64 CPU/Vulkan, plus straightforward official architecture variants recognized by the same strict grammar. Linux packages retain their internal relative library symlinks only after extractor containment validation.

### q27

The provider accepts only exact uploaded `q27-v<version>-linux-x86_64.tar.gz` assets under a matching `v<version>` release and with GitHub SHA-256 metadata. Managed support begins at v0.2.0 because v0.1.x streaming omits the terminal finish reason required to distinguish stop from maximum-output truncation truthfully. It exposes real q27-server W8/W12/W16 executables as distinct variants, with authoritative CUDA driver/VRAM notes. Releases with source but no package are ignored, so Stable/Latest refer to the newest installable release. There is no fabricated Windows pack or source builder.

## Compatibility and selection

`HostCapabilities` records OS/architecture and every NVIDIA device's stable UUID, model, VRAM, driver, and numeric compute-capability observations from bounded `nvidia-smi` execution. Older tools that reject the compute-capability property fall back to the UUID/name/VRAM/driver query and preserve compute capability as unknown. It also snapshots an inherited CUDA visibility constraint for engine-owned reconciliation. Compatibility has four states: Recommended, Compatible, NeedsAttention(reason), and Incompatible(reason). Unknown accelerator evidence is not treated as certainty; known platform or upstream minimum mismatches are. `RuntimeRequirements` separates informational advisories from unverified conditions while retaining schema-v1 `notes` as a legacy unresolved-condition field. It also separates exact byte floors, nominal VRAM classes, and a known-insufficient class where no exact higher floor is published; class matching allows a 2 GiB reporting/ECC/reservation shortfall, treats the next 1 GiB as uncertain, and rejects larger shortfalls. This matches q27's documented 22.6 GiB A10 result without presenting a clearly lower class as compatible.

For a model, resolution proceeds over all installed runtimes whose registered adapter declares the artifact format and accepts the concrete model:

```text
invocation RuntimeId
  → model override RuntimeId
  → format default RuntimeId
  → deterministic best compatible fallback
```

The selected ID does not bypass manifest, host, adapter, or model validation. Each adapter supplies engine-neutral installed and available-runtime model+runtime+host compatibility results, optional semantic preferences, and an exact accelerator selection when it binds one. Explicit invocation, model overrides, format defaults, compatible-runtime listing, fallback, the model picker, model-triggered catalog search, and load admission use that contract. Generic catalog search remains model-independent. A stale stored preference emits a runtime notice before fallback. Fallback ranks actual compatibility, engine preference, managed provenance, current version, and finally runtime ID. For q27, official managed CUDA targets are attached to each version and checked on each candidate device before VRAM; a known unsupported device is rejected and an unobserved compute capability is needs-attention. W8 leads on confirmed 24 GiB-class or unresolved-VRAM devices, W12 leads only when the selected compatible device positively satisfies the 32 GiB class, and W16 is a valid explicit specialist choice but never wins fallback accidentally. W16 rejects confirmed 24 GiB-class hardware and remains needs-attention above that known lower bound because upstream publishes no exact floor. External q27 binaries retain their exact executable identity but remain needs-attention because their width, CUDA targets, and runtime-specific floor are unverified; this makes a verified suitable managed pack win ordinary fallback without invalidating an explicit external choice. Selection source and exact accelerator are carried into launch provenance.

A direct selection always writes an exact ID and defaults to a pinned update preference. CLI callers may instead track the provider-defined Stable or Latest channel without weakening the concrete selection: checks consider only that channel, installs remain explicit and side by side, and `update` does not rewrite selection. Missing truthful channel candidates, catalog outages, provider errors, unpublished exact assets, pins, and newer compatible packs remain distinct states. Users switch only through an explicit select action.

## Download, extraction, and activation

The secure installer accepts only a catalog value from a registered provider and requires an expected SHA-256 for every package component. URLs must be HTTPS on the authoritative GitHub release path. Redirects are manually restricted to exact GitHub API/release-asset hosts; authentication is not forwarded to the asset client.

Each component streams into a `.part` temporary cache file while hashing and reporting actual byte counts. Both response length and digest must match metadata before the content-addressed final cache name is made visible. A mismatch deletes the temporary data.

ZIP and tar.gz extraction runs off the async executor and enforces entry-count and expanded-size limits. It rejects absolute paths, parent components, drive prefixes, backslashes/colon tricks, duplicate entries, special devices/FIFOs, and overwrites. Tar symlink/hardlink targets are normalized and must resolve within staging; links are created only after their targets exist and are canonically contained.

After extraction the installer requires exactly one regular file with the provider-declared entrypoint basename, hashes it, and creates a provisional `InstalledRuntime`. The matching engine adapter probes that exact entrypoint and verifies its identity/required interface. Only a successful observation is written into `runtime.json`; the directory and manifest are synced before one atomic staging-to-destination rename.

Activation refuses an existing different runtime ID and also scans for a different digest under the same claimed repository/tag/assets identity. All error paths clean staging. Shared per-runtime file leases span backend loading and execution; removal requires an exclusive lease, so another Norted process cannot race an active load. Thus extraction alone never produces local truth, failed probes cannot be selected, and updates cannot damage an older installation.

## Model artifacts and q27 tokenizers

`ModelArtifact` retains the primary artifact and stable ID while adding engine-neutral `AuxiliaryArtifact` values with roles. Q27 discovery prefers `model.q27` + `model.tok`; for quantized names it may use the unique longest boundary-safe prefix tokenizer. Candidates must be beside the model and contain `Q27T` magic with supported header version 1. `.tok` is never a primary model. Missing, invalid, or ambiguous companions are expressed by q27's model compatibility result and prevent launch.

Q27 model inspection reads a fixed 16-byte `Q27F` v1 header, rejects metadata lengths above 1 MiB before allocation, reads only that JSON blob, and never maps/hashes tensor payloads. The q27 adapter validates the current `qwen35` 65-block/MTP architecture constants. Exact published tiers come from `quant_policy` plus the presence/value of `q4_head` and `q8_extra`, never the filename. Qwen3.6 default/q4s/q5f map to 24 GiB-class, q6/q6f/q6k to 32 GiB-class, and q8 to 48 GiB-class; Qwen3.8 v2 q4s/default/q6 map to 24 GiB-class and q6k to 32 GiB-class. Unknown recipe tuples remain NeedsAttention.

## Adapter launch contracts

### llama.cpp

The adapter probes a managed runtime's exact contained entrypoint, binary hash, `--version`, and `--help`, requiring `--model`, `--alias`, `--host`, and `--port`. For managed build tags it also checks the reported build/revision relationship rather than trusting the archive name alone. External `binary_path` uses the same interface probe but keeps its source unverified.

Launch is conceptually:

```text
llama-server --model <canonical-gguf> --alias <stable-id>
             --host 127.0.0.1 --port <dynamic> [allowed native arguments]
```

The adapter polls `/health`, then obtains authoritative effective `temperature` and `top_p` from `/props` before Running. It maps internal `/v1/chat/completions` responses/SSE to normalized inference output/events.

### q27

Current q27-server has no stable `--version` response. The adapter therefore validates the exact binary/hash and its real zero-argument usage signature; managed version/revision provenance remains supplied by the verified package manifest and probe reports only what was observed.

Launch is conceptually:

```text
q27-server <canonical-model.q27> <canonical-tokenizer.tok>
           --host 127.0.0.1 --port <dynamic> --no-think
           [allowed native arguments]
```

Norted rejects q27 options/environment that could replace positional inputs, binding/authentication, thinking semantics, sampling truth, or Norted's GPU binding. Compatibility chooses one UUID-identified NVIDIA device and launch sets `CUDA_VISIBLE_DEVICES` to that same full UUID; numeric indices are never correlated across CUDA and `nvidia-smi`. A single inherited UUID constraint is reconciled, while numeric, multiple, unknown, empty, or ambiguous constraints fail closed. Before launch, q27 prepares the already-discovered tokenizer as an engine-neutral auxiliary identity containing role, canonical path, size, and SHA-256. The launch spec uses that exact path and rechecks size/hash immediately before process creation; q27 never rediscovers a companion at launch. It strips conflicting inherited q27 variables and sends explicit `temperature: 0.0` and `top_p: 1.0` on every Chat Completions request; those values are therefore the reported effective settings. Readiness requires `/health` status `ok`. JSON and SSE are translated through the same normalized inference types as llama.cpp.

## Process, control, and provenance

`RuntimeManager` maintains one Stopped/Loading/Running/Stopping/Failed backend. Loading resolves the discovered primary model, exact runtime, and adapter, obtains a dynamic loopback address, builds a `LaunchSpec`, and delegates the child to `TokioProcessSupervisor`. The supervisor drains both pipes, includes runtime identity in process facts, observes unexpected exit, supports startup cancellation, and owns graceful/forced cleanup. A second load conflicts instead of implicitly replacing the active backend.

Cross-process CLI/TUI control uses schema-version-2 descriptors under:

```text
<state>/runtime/servers/<instance-id>.json
```

The serving process atomically writes its random identity, PID, public probe address, private loopback endpoint, and random bearer token. Observers prove identity through public health before sending authenticated `status`, `load`, or `unload` requests. Tokens are redacted and absent from public/status/provenance output.

Private backend status carries model ID, engine ID, runtime ID/version/variant, executable SHA-256, PID, and private endpoint. `RuntimeProvenance` retains the exact immutable runtime manifest, selection source, selected accelerator identity and observations, primary model facts and every materially used auxiliary artifact's role/canonical path/size/SHA-256, sanitized native arguments with option/value association and value hashes, every effective explicit/inherited environment variable name with a value hash, inheritance policy, authoritative effective sampler settings, process identity, endpoint, and launch time. Missing facts stay optional; external acquisition never gains invented release provenance.

## Public protocol

Responses remains canonical and Chat Completions is compatibility-only. Both public parsers normalize `developer`, `system`, `user`, and `assistant` text into the same engine-neutral `InferenceMessage` list and attach a typed `GenerationSettingsPatch`. `RuntimeManager` asks the selected adapter to validate explicit settings, merges them with the running backend's observed defaults for truthful public reporting, and does not mutate those defaults. Load profiles, runtime selection, and immutable launch provenance never contain request-time values.

`POST /v1/responses` accepts the documented text subset and constructs the current non-streaming document or ordered Responses SSE sequence itself. `POST /v1/chat/completions` constructs current text ChatCompletion objects/chunks from the same normalized inference output. Each adapter owns only its private upstream JSON/SSE: llama.cpp omits absent sampler fields and q27 materializes its established 0/1 defaults while accepting `top_p < 1` only with a positive effective temperature. Output-limit completion maps to incomplete/length state, basic Chat usage is emitted when known, and richer Responses usage is emitted only when every required detail is known. Stream options are valid only on streams; explicit disabled obfuscation is compatible, but Norted does not emit OpenAI stream padding. Unsupported input or top-level behavior is rejected rather than forwarded or silently ignored.

Public middleware assigns an independent `req_...` ID and returns it as `x-request-id` on success, JSON errors, and streams. `X-Client-Request-Id` is accepted only as ASCII correlation metadata up to 512 characters. Authentication runs before bounded JSON extraction; inference bodies are capped at 32 MiB. Errors share the OpenAI-style `error { message, type, param, code }` envelope and map internal conditions deliberately without local paths, private addresses, credentials, or debug text. No permissive CORS layer is installed.

`GET /v1/models` remains the audited OpenAI-style list with exactly `id`, `object`, `created`, and `owned_by`. Runtime metadata remains private control-plane state rather than leaking into this public compatibility surface.

The richer `ModelServingCapabilities` view is local/private: it derives format, compatible registered engines, compatible installed runtimes, resolved runtime, active state, and gateway features from real compatibility and selection data. Tools, vision, and structured output are false for this milestone. `norted-server models info <MODEL_ID>` exposes the view without expanding the public Model object.

## Public API-key state and transport

`<data>/api-keys.json` schema 1 contains bounded key records with stable IDs, labels, display prefixes, SHA-256 digests, and creation/revocation timestamps. A separate inter-process lock and atomic replacement serialize writers; corruption is an error rather than an empty-store fallback. Creation draws 256 bits from the operating-system RNG and returns the `norted_sk_...` plaintext only to that one CLI invocation. Verification accepts only bounded Bearer credentials, hashes the supplied secret, compares fixed-size digests in constant time, and considers only active records. The small atomic file is reread per authenticated request so revocation is live.

`serve` reads key state and resolves configured/effective auth before creating the public socket. Required auth with zero active keys therefore fails closed before exposure. Non-loopback `disabled` remains allowed only because it is an explicit operator choice and is labelled insecure throughout local status surfaces.

Application authentication provides no confidentiality. Loopback is local; a trusted VPN such as Tailscale can encrypt remote transport; or an operator-managed reverse proxy can terminate TLS. Norted does not include certificate management, and plain HTTP over an untrusted network exposes credentials and content.

## TUI and network independence

The TUI enters terminal mode and draws its pending first frame before starting model discovery, local runtime scanning, control observation, or API-key file reads. The Server page derives configured bind/auth policy without I/O for that frame, then refreshes the small local key count asynchronously. The Runtimes page can load local selections/installs asynchronously. Remote search begins only after a user search action, and install/update work remains off the event loop with real installer progress messages. Keyboard, mouse, narrow layout, ASCII mode, and `NO_COLOR` are presentation concerns isolated in `norted-tui`.

The hard invariants are:

- no ordinary startup path depends on the runtime catalog network;
- managed packages require authoritative SHA-256 verification;
- extraction and removal remain contained within exact Norted-owned roots;
- activation happens only after an exact adapter probe;
- runtime versions are immutable and side by side;
- selection always names a concrete runtime and never hard-codes format-to-engine identity;
- only the supervisor owns generic child mechanics;
- only adapters own backend-native behavior;
- only the API crate owns public Responses and Chat representations;
- public API keys and private control credentials remain separate authentication domains;
- no public path exposes a private backend or control operation;
- at most one backend is active.
