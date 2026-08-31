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

`EngineRegistry` contains the llama.cpp, q27, and NInfer adapters. `RuntimePackManager` finds installed runtimes whose engine adapter and declared formats can serve the model, applies the selection policy, and hands the selected exact runtime to the adapter. No core branch binds an artifact format to a particular engine ID; adapters advertise formats and perform their own exact model/runtime compatibility checks.

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
                     llama-server, q27-server, or ninfer-serve

LOCAL CONTROL CLIENT (CLI / TUI)
    │ distinct random control bearer token from private descriptor
    ▼
PRIVATE LOOPBACK CONTROL API
```

The public listener is configurable and defaults to `127.0.0.1:8742`. `auto` auth is disabled only on loopback and required on every non-loopback address; `required` always requires it, while `disabled` is an explicit insecure override. Backend and authenticated control listeners are always separate loopback endpoints with different credentials. Public keys cannot authorize control requests, and clients are never redirected to an upstream backend.

The interactive process preserves this boundary in both lifecycle modes. The TUI attaches and owns no `RuntimeManager` or `ApiServer` only after descriptor observation, the public identity probe, and an authenticated private control `status` request all succeed. A public-healthy descriptor with unusable private control makes ownership uncertain and fails startup rather than admitting attachment or binding a competitor. When no healthy descriptor exists, the composition root starts the real serving stack as an owned Tokio task in the TUI process. The TUI continues to discover and invoke the private HTTP control API; it never calls `RuntimeManager::load` or `unload` directly. A short-lived cross-process startup lock serializes the observe-or-bind decision, and the configured public socket remains the final ownership claim, so startup races cannot silently create a second owner or select another port.

## Crate responsibilities

- `norted-core` owns platform paths, schema-version-1 configuration, public-auth policy, the versioned atomic API-key store, typed serving settings and resolution, user-owned Model Profiles, model/auxiliary-artifact discovery, stable IDs, runtime identity/manifest/preferences data, host-independent provenance, application state, and process descriptors.
- `norted-engine` owns `EngineAdapter`, `EngineRegistry`, the provider/catalog/cache, secure installer, runtime store and resolver, runtime/backend manager, generic process supervisor, control client, and normalized inference types.
- `norted-engine-llama-cpp` owns official llama.cpp asset classification, the immutable managed Linux CUDA source recipe/provider, binary probes, flags/environment policy, readiness and `/props`, and Chat Completions JSON/SSE translation.
- `norted-engine-q27` owns official q27 binary/source capability classification, exact Makefile target recipes, tokenizer requirements, usage-signature probes, flags/environment policy, readiness, provable sampler settings, and Chat Completions JSON/SSE translation.
- `norted-engine-ninfer` owns the canonical source-snapshot catalog, exact target-registry enumeration, `.ninfer` compatibility, source/external runtime probes, settings and GPU policy, startup-log observation, and private Chat JSON/SSE translation.
- `norted-api` owns public bearer/request middleware, the shared sanitized error contract, Responses and Chat Completions text surfaces, and the private authenticated control HTTP surface. It does not construct upstream flags or expose backend-native bytes.
- `norted-tui` owns terminal lifecycle, responsive rendering, runtime search/install/selection interaction, the generic schema-driven load-settings editor, and normalized control observation.
- `norted-server` is the composition root and scriptable CLI.

Adapter registration and provider registration are separate. A built-in engine can be enabled even when no runtime is installed; conversely, every installed manifest still requires its corresponding registered adapter before it can launch.

## Settings and Model Profile resolution

`Engine`, `Runtime`, `ModelArtifact`, `ModelProfile`, and request-time generation patches are
distinct domains. `ModelProfile` is deliberately small: typed ID, display name, concrete bound
artifact ID, bound engine ID, and typed overrides. It is always user-owned and mutable. Technical
compatibility comes from the bound artifact, registered engine, exact runtime, and resulting
settings—not from profile-authored applicability declarations.

`SettingsState` schema 1 lives at `<data>/settings.json` and stores only Global and per-engine
defaults. `ModelProfilesState` schema 1 lives at `<data>/model-profiles.json`. Both stores have
independent locks and atomic replacement. No older combined state is read or migrated.

One resolver applies Global defaults, the bound engine defaults, Model Profile overrides, then
ephemeral invocation overrides. It filters unrelated engine namespaces, records the winning source
as `global-default`, `engine-default:<engine>`, `model-profile:<id>`, or `invocation`, and produces
no entry for an upstream default. There is no model-default or hidden policy layer. Relative typed
paths resolve beneath the data directory before all downstream consumers receive the same absolute
value.

Runtime selection receives the profile's explicit engine and cannot switch engines. Existing
resolution order remains explicit runtime, persisted model/runtime choice when applicable,
engine/format default, then best compatible installed runtime. Adapters expose a typed category-aware
schema for common plus their own namespace, gate it first with facts proved by the bound model, and
then gate it with the exact runtime contract before validating effective settings. Capability
requirements are consequences of selected settings. q27 retains exact source fingerprints and
bounded context/KV/W_MAX startup proof; NInfer retains native-container and schema-18 startup proof;
llama.cpp uses exact help evidence.

`RuntimeProvenance.model_profile` records the ID, display name, deterministic content hash, bound
artifact ID, and engine ID. Artifact lineage and hashes remain separately recorded. Settings
provenance contains the complete effective map and source attribution. No artifact package metadata
selects settings, prompt templates, runtime defaults, or UI controls.

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
provider + repository + acquisition-specific package identity
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

`runtime.json` schema 2 adds acquisition-specific provenance while continuing to read schema-1 release manifests unchanged. Release and preseeded records retain verified primary/additional archive SHA-256 values and complete release-asset identity. A source-build record instead has `acquisition_method = source_build`, no archive digest, and an exact source/build block: repository URL and source ref, full commit/tree SHAs and timestamp, recipe version/build system/arguments/target, observed Make or CMake/Ninja plus C++/CUDA/pkg-config/system-library versions, build platform/architecture/accelerator target and time, and matching relative entrypoint/SHA-256. `RuntimeIdentity` itself did not gain fields, so existing canonical serialization, `RuntimeId`s, selections, and installation paths remain stable. An explicitly configured external binary remains an in-memory external manifest with an absolute canonical entrypoint, no fabricated source/release/install-time fields, and unverified repository provenance.

Scanning validates every manifest, canonicalizes the root and entrypoint, and rejects containment failures, duplicate IDs, invalid schemas, failed probe records, and unsafe staging paths. Removal recomputes the expected directory from the manifest, canonicalizes it under the runtime root, rejects staging/external targets, and removes only that exact owned directory.

Mutable `runtime-selections.json` schema 1 is adjacent to, not inside, the immutable installations. It contains exact format defaults, exact model overrides, and update preferences. Atomic temporary-file replacement is used for preferences and catalog cache data; neither is written into user TOML.

## Live runtime catalog

`RuntimeCatalogProvider` produces engine-neutral `AvailableRuntime` values. `RuntimeCatalog` aggregates providers and attaches local installed/selected and host compatibility state. Provider results are cached per provider in the application cache directory with a bounded freshness window; a fresh cache avoids API calls, and a stale cache can support explicit error reporting. Normal startup never fetches it.

`GitHubReleaseClient` requests the newest 100 releases from the official repository for broad browsing. When a concrete older tag/runtime ID is requested or an installed exact runtime falls outside that window, the matching provider resolves GitHub's exact tag endpoint on demand. This keeps ordinary searches bounded without falsely declaring an older published pin missing. Optional `GITHUB_TOKEN`/`GH_TOKEN` authentication is confined to this API client. Catalog entries carry exact repository, release/tag, asset ID/name/URL/size/digest, channel, and requirements. The installer independently revalidates URL authority and digest presence.

### llama.cpp

The official provider classifies asset names rather than assuming a release matrix. Each platform/architecture/backend line receives its own newest actual `Latest` candidate. Windows CUDA requires the matching same-release `cudart` asset and both identities/downloads become part of the runtime identity. Stable follows the verified semantic-release `nightly-tag.txt` pointer, including its digest and commit relationship. Drafts, non-uploaded files, malformed/missing asset relationships, unknown grammars, and unsupported variants are not guessed.

Initial supported families include Windows x86_64 CPU/CUDA/Vulkan and Linux x86_64 CPU/Vulkan, plus straightforward official architecture variants recognized by the same strict grammar. Linux packages retain their internal relative library symlinks only after extractor containment validation.

A separate provider supplies Linux x86_64 CUDA as a managed source build, not as a fabricated release asset. It checks official nightlies newest-first and selects the newest exact revision whose bounded root, ggml/CUDA, common, tools/UI, and server CMake files still prove the Norted-owned build contract. The check covers the CUDA option and backend path, explicit-architecture override, server target/output/C++17 contract, and the flags that keep optional CCCL, NCCL, LLGuidance, UI, OpenSSL, and subprocess behavior out of the managed build. A drifted newest nightly is skipped in favor of the newest earlier admitted nightly. Candidate revalidation repeats these immutable-file checks as well as the tag/commit/tree proof before installation; Runtime Search never clones or compiles source.

The current source identity is `managed-portable-v2`; the earlier `managed-portable-v1` recipe remains historically distinct. V2 uses CMake/Ninja Release mode, explicitly retains upstream shared libraries, and sets `CMAKE_BUILD_RPATH_USE_ORIGIN=ON` with build RPATHs enabled, so the executable and shared libraries kept together under `build/bin` use `$ORIGIN`-relative build-tree paths after the source tree moves from staging to its immutable runtime-store path. It builds only `llama-server` at `build/bin/llama-server`. Its deterministic real-code CUDA policy is `75-real;80-real;86-real;89-real;90-real;120a-real`, represented as exact compute capabilities 7.5, 8.0, 8.6, 8.9, 9.0, and 12.0 and accelerator target `sm_75+sm_80+sm_86+sm_89+sm_90+sm_120a`. CUDA Toolkit 12.8 is the prerequisite floor because it is the first Toolkit that can compile the fixed Blackwell `sm_120a` target. Before checkout or compilation, the generic prerequisite probe checks both this version floor and the observed compiler's `--list-gpu-code`/`--list-gpu-arch` output for every explicit real/virtual CMake target. Exact CMake arguments plus the observed CMake/Ninja/C++/nvcc toolchain remain in build provenance.

### q27

The provider evaluates two independent acquisition capabilities for every exact `v<version>` release from v0.2.0 onward. An uploaded `q27-v<version>-linux-x86_64.tar.gz` with matching GitHub URL and SHA-256 yields upstream-binary variants. Without that route, the provider resolves the tag to a full commit/tree, bounds exact-revision `Makefile` and `README.md` reads, and admits only targets and toolchain facts proven by those files. W12 maps to `build/q27-server`, W8 to `build/q27-server-w8`, and W16 to `build/q27-server-w16`; an absent target is not advertised. A verified binary wins over a source candidate for the same release/variant. Stable/Latest are computed across admitted releases, so a newer source-only semantic release is not hidden behind an older archive. Source identities include the recipe family and immutable commit, while binary identities retain asset ID/name/digest. There is no fabricated Windows route.

### NInfer

NInfer has no packaged binary or install target. Its provider queries the canonical `Neroued/ninfer` repository, resolves the current default branch and its full HEAD commit/tree metadata, and returns one `Latest` `SourceBuild` candidate. It does not synthesize Stable, semantic versions, release assets, or archive hashes. The human version is an explicit `git-<date>-<short-sha>` snapshot label; the full commit in identity/provenance is authoritative. A bounded cache of previously discovered source descriptors keeps a selected immutable commit addressable after HEAD advances, while installation still revalidates that exact commit/tree against the live provider.

Immediately before install, provider verification reconstructs the canonical candidate and fixed recipe while the installer independently fetches the selected commit metadata and tree. A later default-branch HEAD never replaces the selected commit. Update checks compare installed and current commits through GitHub ancestry: identical is Current, a current descendant is an update, and a backward/diverged branch becomes ProviderError. Pinned selections may report a descendant but never move automatically.

Normal startup and local runtime listing use only installed manifests and cached state. Repository/default-branch/commit/compare requests occur only during explicit search, refresh, install revalidation, or update checks.

## Compatibility and selection

`HostCapabilities` records OS/architecture and every NVIDIA device's stable UUID, model, VRAM, driver, and numeric compute-capability observations from bounded `nvidia-smi` execution. Older tools that reject the compute-capability property fall back to the UUID/name/VRAM/driver query and preserve compute capability as unknown. It separately records a successful probe that positively found no NVIDIA device; that makes a CUDA requirement incompatible, while unavailable or timed-out probe evidence remains unknown and needs attention. It also snapshots an inherited CUDA visibility constraint for engine-owned reconciliation. Compatibility has four states: Recommended, Compatible, NeedsAttention(reason), and Incompatible(reason). Unknown accelerator evidence is not treated as certainty; known platform or upstream minimum mismatches are. `RuntimeRequirements` separates informational advisories from unverified conditions while retaining schema-v1 `notes` as a legacy unresolved-condition field. It also separates exact byte floors, nominal VRAM classes, and a known-insufficient class where no exact higher floor is published; class matching allows a 2 GiB reporting/ECC/reservation shortfall, treats the next 1 GiB as uncertain, and rejects larger shortfalls. This matches q27's documented 22.6 GiB A10 result without presenting a clearly lower class as compatible.

For a model, resolution proceeds over all installed runtimes whose registered adapter declares the artifact format and accepts the concrete model:

```text
invocation RuntimeId
  → model override RuntimeId
  → format default RuntimeId
  → deterministic best compatible fallback
```

The selected ID does not bypass manifest, host, adapter, or model validation. Each adapter supplies engine-neutral installed and available-runtime model+runtime+host compatibility results, optional semantic preferences, and an exact accelerator selection when it binds one. Explicit invocation, model overrides, format defaults, compatible-runtime listing, fallback, the model picker, model-triggered catalog search, and load admission use that contract. Generic catalog search remains model-independent. A stale stored preference emits a runtime notice before fallback. Fallback ranks actual compatibility, engine preference, managed provenance, current version, and finally runtime ID. For q27, official managed CUDA targets are attached to each version and checked on each candidate device before VRAM; a known unsupported device is rejected and an unobserved compute capability is needs-attention. W8 leads on confirmed 24 GiB-class or unresolved-VRAM devices, W12 leads only when the selected compatible device positively satisfies the 32 GiB class, and W16 is a valid explicit specialist choice but never wins fallback accidentally. W16 rejects confirmed 24 GiB-class hardware and remains needs-attention above that known lower bound because upstream publishes no exact floor. External q27 binaries retain their exact executable identity but remain needs-attention because their width, CUDA targets, and runtime-specific floor are unverified; this makes a verified suitable managed pack win ordinary fallback without invalidating an explicit external choice. Selection source and exact accelerator are carried into launch provenance.

Managed NInfer candidates are fixed to Linux/x86_64, exact numeric compute capability 12.0, separately observed `NVIDIA GeForce RTX 5090`, and `sm_120a`. Compatibility evaluates concrete UUID-bearing NVIDIA devices independently, so a proven supported device outranks a larger incompatible one. Unknown name or compute capability remains needs-attention; a known contradiction is incompatible. A single inherited UUID visibility constraint is safely resolved through the same generic machinery used by q27. Launch normalizes it to the selected full UUID and passes NInfer `--device 0`, making its CUDA-local zero the exact selected physical device retained in provenance. External NInfer remains usable where not disproved, but its source, target registry, build recipe, and compiled hardware contract remain unknown.

A direct selection always writes an exact ID and defaults to a pinned update preference. CLI callers may instead track the provider-defined Stable or Latest channel without weakening the concrete selection: checks consider only that channel, installs remain explicit and side by side, and `update` does not rewrite selection. Missing truthful channel candidates, catalog outages, provider errors, unpublished exact assets, pins, and newer compatible packs remain distinct states. Users switch only through an explicit select action.

## Runtime acquisition and activation

The secure installer accepts only a catalog value from a registered provider and requires an expected SHA-256 for every package component. URLs must be HTTPS on the authoritative GitHub release path. Redirects are manually restricted to exact GitHub API/release-asset hosts; authentication is not forwarded to the asset client.

Each component streams into a `.part` temporary cache file while hashing and reporting actual byte counts. Both response length and digest must match metadata before the content-addressed final cache name is made visible. A mismatch deletes the temporary data.

ZIP and tar.gz extraction runs off the async executor and enforces entry-count and expanded-size limits. It rejects absolute paths, parent components, drive prefixes, backslashes/colon tricks, duplicate entries, special devices/FIFOs, and overwrites. Tar symlink/hardlink targets are normalized and must resolve within staging; links are created only after their targets exist and are canonically contained.

After extraction the installer requires exactly one regular file with the provider-declared entrypoint basename, hashes it, and creates a provisional `InstalledRuntime`. The matching engine adapter probes that exact entrypoint and verifies its identity/required interface. Only a successful observation is written into `runtime.json`; the directory and manifest are synced before one atomic staging-to-destination rename.

Activation refuses an existing different runtime ID and also scans for a different digest under the same claimed repository/tag/assets identity. All error paths clean staging. Shared per-runtime file leases span backend loading and execution; removal requires an exclusive lease, so another Norted process cannot race an active load. Thus extraction alone never produces local truth, failed probes cannot be selected, and updates cannot damage an older installation.

Source build is a parallel generic acquisition arm, not a provider branch in the runtime manager. For q27 it is:

```text
q27 release tag
  → exact canonical Git commit/tree + bounded Makefile/README evidence
  → Linux/x86_64/Git/Make/C++/nvcc prerequisite validation
  → transactional exact-commit checkout and tree verification
  → selected upstream Makefile target only
  → executable bit + SHA-256 + q27 adapter probe
  → source-build runtime manifest
  → atomic runtime activation
```

The q27 Make contract is a provider-reviewed exact dependency/command closure, represented by the immutable Makefile SHA-256 in the v2 recipe. Discovery admits only a recognized closure; installation hashes the checked-out Makefile again before invoking Make. This fail-closed binding covers includes, sub-makes, and helper commands without pretending to implement a partial GNU Make parser: any upstream build-definition change needs a new provider audit and recipe identity before it can run. Environment overrides for compilers, CUDA flags, link flags, and Make flags are removed so the upstream target is authoritative. The recipe records tag, commit/tree, audited Makefile digest, width/target, CUDA architecture set and floor, compiler identities, Make identity, build host/time, and result digest. Search performs only bounded exact-revision reads and prerequisite probes; clone and compilation occur only during install.

For NInfer the same arm is:

```text
NInfer catalog provider
  → exact canonical Git commit/tree
  → build prerequisite validation
  → transactional GitHub HTTPS checkout
  → HEAD and HEAD^{tree} verification
  → fixed Norted CMake recipe
  → build/apps/ninfer-serve
  → SHA-256 + adapter probe
  → source-build runtime manifest
  → atomic runtime activation
```

Prerequisites are checked before staging/clone where practical: Linux x86_64, Git, CMake ≥3.28, Ninja, a compiling C++20 toolchain, nvcc/CUDA ≥13.1, pkg-config, libavformat ≥60, libavcodec ≥60, libavutil ≥58, libswscale ≥7, and libcurl ≥7.85. Checks are bounded direct processes and never invoke a package manager or modify the host. Missing prerequisites are build-readiness failures, not model-admission failures.

The versioned recipe configures Release with Ninja, apps on, tests/benchmarks off, and CUDA architecture `120a`, then builds only `ninfer-serve`. Catalog-controlled data is never embedded in a shell command. Build-control environment variables are removed explicitly. Before CMake runs, a bounded scan of CMake definitions rejects FetchContent, ExternalProject, `file(DOWNLOAD)`, Git clones, and HTTP URLs; the inspected upstream tree otherwise uses vendored source and explicit system libraries. The exact source/build tree is retained because upstream defines no install layout and the adapter has not assumed the executable is independently relocatable.

Git and build children use `kill_on_drop`, stream bounded failure tails, and run asynchronously. Checkout mismatch, dependency audit, configure/build failure, missing entrypoint, hash change, probe failure, cancellation, or store failure removes unique staging. Manifest creation and directory sync precede one atomic activation, so none of these failures can create local installed truth.

## Model artifacts and native identity

`ModelArtifact` retains the primary artifact and stable ID, engine-neutral `AuxiliaryArtifact` values with roles, and an optional typed `ArtifactNativeIdentity`. Q27 discovery prefers `model.q27` + `model.tok`; for quantized names it may use the unique longest boundary-safe prefix tokenizer. Candidates must be beside the model and contain `Q27T` magic with supported header version 1. `.tok` is never a primary model. Missing, invalid, or ambiguous companions are expressed by q27's model compatibility result and prevent launch.

Q27 model inspection reads a fixed 16-byte `Q27F` v1 header, rejects metadata lengths above 1 MiB before allocation, reads only that JSON blob, and never maps/hashes tensor payloads. The q27 adapter validates the current `qwen35` 65-block/MTP architecture constants. Exact published tiers come from `quant_policy` plus the presence/value of `q4_head` and `q8_extra`, never the filename. Qwen3.6 default/q4s/q5f map to 24 GiB-class, q6/q6f/q6k to 32 GiB-class, and q8 to 48 GiB-class; Qwen3.8 v2 q4s/default/q6 map to 24 GiB-class and q6k to 32 GiB-class. Unknown recipe tuples remain NeedsAttention.

NInfer version-2 containers begin with `NINFER\0\x02` and a little-endian 64-bit JSON directory length at byte 8. Discovery caps the directory at 16 MiB, proves it lies within the file, validates the closed identity/object descriptor shapes and ordered non-overlapping object ranges, and reads no later weight bytes. Version 1 is not migrated or rewritten. The resulting typed NInfer identity contains `container_version`, `model_id`, and `weights_id`; it is carried through prepared input, compatibility, private model information, and launch provenance without being placed in `architecture` or exposed by public `/v1/models`.

The `.ninfer` artifact is one primary file whose tokenizer/template/frontend resources remain embedded. The NInfer adapter re-inspects bounded metadata during preparation and again before launch. Installed managed runtime manifests enumerate exact native identities by parsing the declarative target registry in their own retained source snapshot. The parser fails all-or-nothing: if upstream changes the registry structure, compatibility becomes NeedsAttention and startup remains definitive rather than accepting a hard-coded filename or stale global list. No discovery path invokes a runtime, GitHub, Hugging Face, payload hashing, model download, conversion, or v1 migration.

## Adapter launch contracts

### llama.cpp

The adapter probes a managed runtime's exact contained entrypoint, binary hash, `--version`, and `--help`, requiring `--model`, `--alias`, `--host`, and `--port`. For managed build tags it also checks the reported build/revision relationship rather than trusting the archive name alone. External `binary_path` uses the same interface probe but keeps its source unverified.

Launch is conceptually:

```text
llama-server --model <canonical-gguf> --alias <stable-id>
             --host 127.0.0.1 --port <dynamic> [allowed native arguments]
```

The adapter polls `/health`, then obtains authoritative effective `temperature` and `top_p` from `/props` before Running. Configured `temperature`, `top_p`, `top_k`, and `min_p` are emitted only through exact help-advertised process-default controls; omission preserves upstream behavior, while request-time `temperature` and `top_p` are explicit request fields that override those defaults. It maps internal `/v1/chat/completions` responses/SSE to normalized inference output/events.

### q27

Current q27-server has no stable `--version` response. The adapter therefore validates the exact binary/hash and its real zero-argument usage signature; managed version/revision provenance remains supplied by the verified package manifest and probe reports only what was observed.

Launch is conceptually:

```text
q27-server <canonical-model.q27> <canonical-tokenizer.tok>
           --host 127.0.0.1 --port <dynamic> --no-think
           [allowed native arguments]
```

Norted rejects q27 options/environment that could replace positional inputs, binding/authentication, thinking semantics, sampling truth, or Norted's GPU binding. Compatibility chooses one UUID-identified NVIDIA device and launch sets `CUDA_VISIBLE_DEVICES` to that same full UUID; numeric indices are never correlated across CUDA and `nvidia-smi`. A single inherited UUID constraint is reconciled, while numeric, multiple, unknown, empty, or ambiguous constraints fail closed. Before launch, q27 prepares the already-discovered tokenizer as an engine-neutral auxiliary identity containing role, canonical path, size, and SHA-256. The launch spec uses that exact path and rechecks size/hash immediately before process creation; q27 never rediscovers a companion at launch. It strips conflicting inherited q27 variables and sends explicit `temperature: 0.0` and `top_p: 1.0` on every Chat Completions request; those values are therefore the reported effective settings. Readiness requires `/health` status `ok`. JSON and SSE are translated through the same normalized inference types as llama.cpp.

### NInfer

The adapter probes the exact binary SHA-256 and requires the current `ninfer-serve --help` contract, without inventing a version/revision that the executable does not report. Managed revision/build identity comes from the source manifest; an external `binary_path` is canonicalized relative to `config.toml` and remains `ExternalBinary` with unknown source/build facts.

Launch is conceptually:

```text
CUDA_VISIBLE_DEVICES=<selected full GPU UUID>
ninfer-serve <canonical-model.ninfer>
             --host 127.0.0.1 --port <dynamic>
             --model-id <stable Norted ModelId>
             --device 0
             --request-log-jsonl <private restrictive temporary path>
             [explicit structured settings]
             [strictly allowlisted operational arguments]
```

The positional artifact, binding, alias, device, auth/CORS surface, structured load options, cache/vision/Responses-state controls, sampler/greedy controls, and startup log are reserved. `/health` alone is insufficient for readiness. Norted reads a bounded schema-18 `server_start` JSONL record, validates the public alias, artifact target/weights identity, context-cost identity, selected GPU UUID/name/compute capability, and non-greedy state, and derives exact effective temperature/top-p from the runtime's selected thinking preset plus server overrides. The file is then unlinked while the Linux child retains its descriptor; supervisor/cancellation paths remove it on every failed startup or exit.

Public Responses and Chat messages both become one ordered canonical `InferenceRequest`, then private NInfer Chat JSON. Omitted sampler fields stay absent; explicit values are range-checked and forwarded. Non-streaming and bounded SSE parsers expose answer content only, discard separate reasoning text, map only stop/length, preserve only reported usage details, require a terminal finish reason and `[DONE]`, and own the underlying HTTP stream directly so client cancellation drops it. NInfer's vision, tools, Anthropic surface, raw/stateful Responses routes, and request history are intentionally outside this adapter contract.

## Process, control, and provenance

`RuntimeManager` maintains one Stopped/Loading/Running/Stopping/Failed backend. Loading resolves the discovered primary model, exact runtime, and adapter, obtains a dynamic loopback address, builds a `LaunchSpec`, and delegates the child to `TokioProcessSupervisor`. The supervisor drains both pipes, includes runtime identity in process facts, observes unexpected exit, supports startup cancellation, and owns graceful/forced cleanup. A second load conflicts instead of implicitly replacing the active backend.

While loading, `RuntimeManager` publishes an engine-neutral `BackendLoadProgress` through the private control status. Progress carries a generic phase (selecting runtime, resolving settings, loading model, verifying startup, etc.), an optional fraction/current/total when the exact runtime exposes trustworthy measurable progress, and an optional human-readable message. Percentages are never invented from phase transitions alone. Engine-specific log parsing stays inside each adapter via an optional `startup_progress` hook; the generic manager owns the progress state and sanitizes untrustworthy numeric values. Progress is cleared on Running, Failed, cancellation, and unload; an old load generation can never overwrite a newer load's progress. Both owned and attached TUI modes observe the same control status.

Private Load is asynchronous admission rather than a model-startup-duration HTTP transaction. While holding the backend mutation guard, `RuntimeManager` rejects shutdown/busy state, validates model existence, allocates a generation, and publishes Loading before transferring the owned guard to one server-owned Tokio task. `POST /control/v1/load` then returns `202 Accepted` with that status. The TUI uses this start operation and status observation; the CLI convenience remains blocking by polling the admitted generation to Running or Failed. A dropped handler/client future cannot cancel or orphan the operation. Unload and shutdown advance the existing cancellation epoch, terminate a loading process when present, wait for the same guarded pipeline to unwind, and release its runtime lease. Adapter pre-launch validation remains PreparingLaunch; SpawningBackend is published only immediately before `ProcessSupervisor::spawn`.

Cross-process CLI/TUI control uses schema-version-2 descriptors under:

```text
<state>/runtime/servers/<instance-id>.json
```

The serving process atomically writes its random identity, PID, public probe address, private loopback endpoint, and random bearer token. Observers prove identity through public health before sending authenticated `status`, `load`, or `unload` requests. Private backend status includes a monotonic manager generation for correlating an admitted load with later observations. Tokens are redacted and absent from public/status/provenance output.

`norted-server serve` is the explicit headless owner. `norted-server` and `norted-server tui` first attach to an already healthy owner whose authenticated private control status also succeeds; when none exists, they construct the same registry, shared runtime-pack manager, runtime manager, public-auth policy, and `ApiServer`, then wait for the published private control API to answer before entering normal interaction. Headless composition requires completed model discovery before reporting readiness. Owned interactive composition permits the registry to remain NotScanned/Scanning so the TUI can draw first and launch `ApplicationCore::start_model_discovery`; public model enumeration and load admission use only the actual registry, so pending discovery cannot fabricate a loadable model. The owned task is coupled to the TUI lifetime: normal exit and TUI errors signal `ApiServer::run`, await its graceful listener shutdown and `RuntimeManager::shutdown`, and let the owned `RuntimePublisher` remove only its own descriptor. Attached mode creates no serving task, so TUI exit cannot stop the external process. Scriptable `load` and `unload` remain control clients and never launch a server.

Private backend status carries Model Profile ID, underlying artifact Model ID, engine ID, runtime ID/version/variant, executable SHA-256, PID, and private endpoint. `RuntimeProvenance` retains the immutable Model Profile identity/hash/bindings, exact runtime manifest and selection source, selected accelerator observations, primary model facts including typed native identity, every materially used auxiliary artifact's role/canonical path/size/SHA-256, source-attributed effective settings, sanitized native arguments and environment hashes, authoritative effective sampler settings, process identity, endpoint, and launch time. For NInfer, the embedded manifest supplies source repository/commit/tree, recipe/toolchain, build/result hashes separately from the artifact's model/weights identity. Missing facts stay optional and external acquisition never gains invented provenance.

## Public protocol

Responses remains canonical and Chat Completions is compatibility-only. Both public parsers normalize `developer`, `system`, `user`, and `assistant` text into the same engine-neutral `InferenceMessage` list and attach a typed `GenerationSettingsPatch`. Chat `reasoning_effort` and Responses `reasoning.effort` share the same typed low/medium/high value. `RuntimeManager` validates request values against the active adapter/runtime and merges supported request generation fields over configured defaults without mutating runtime selection or immutable launch provenance. The public serving alias and `/v1/models` identity are the active/user-created Model Profile ID, while the artifact Model ID remains private provenance.

`POST /v1/responses` accepts the documented text subset and constructs the current non-streaming document or ordered Responses SSE sequence itself. `POST /v1/chat/completions` constructs current text ChatCompletion objects/chunks from the same normalized inference output. Each adapter owns only its private upstream JSON/SSE: llama.cpp omits absent sampler fields and q27 materializes its established 0/1 defaults while accepting `top_p < 1` only with a positive effective temperature. Output-limit completion maps to incomplete/length state, basic Chat usage is emitted when known, and richer Responses usage is emitted only when every required detail is known. Stream options are valid only on streams; explicit disabled obfuscation is compatible, but Norted does not emit OpenAI stream padding. Unsupported input or top-level behavior is rejected rather than forwarded or silently ignored.

Public middleware assigns an independent `req_...` ID and returns it as `x-request-id` on success, JSON errors, and streams. `X-Client-Request-Id` is accepted only as ASCII correlation metadata up to 512 characters. Authentication runs before bounded JSON extraction; inference bodies are capped at 32 MiB. Errors share the OpenAI-style `error { message, type, param, code }` envelope and map internal conditions deliberately without local paths, private addresses, credentials, or debug text. No permissive CORS layer is installed. NInfer follows this same canonical path; its upstream Responses/Anthropic/tools/vision/state are not proxied into a second public surface.

`GET /v1/models` remains the audited OpenAI-style list with exactly `id`, `object`, `created`, and `owned_by`. Runtime metadata and NInfer model/weights identities remain private control-plane state rather than leaking into this public compatibility surface; local `models info` may show the typed identity.

The richer `ModelServingCapabilities` view is local/private: it derives format, compatible registered engines, compatible installed runtimes, resolved runtime, active state, and gateway features from real compatibility and selection data. Tools, vision, and structured output are false for this milestone. `norted-server models info <MODEL_ID>` exposes the view without expanding the public Model object.

## Public API-key state and transport

`<data>/api-keys.json` schema 1 contains bounded key records with stable IDs, labels, display prefixes, SHA-256 digests, and creation/revocation timestamps. A separate inter-process lock and atomic replacement serialize writers; corruption is an error rather than an empty-store fallback. Creation draws 256 bits from the operating-system RNG and returns the `norted_sk_...` plaintext only to that one CLI invocation. Verification accepts only bounded Bearer credentials, hashes the supplied secret, compares fixed-size digests in constant time, and considers only active records. The small atomic file is reread per authenticated request so revocation is live.

The common serving composition used by headless `serve` and an owned TUI reads key state and resolves configured/effective auth before creating the public socket. Required auth with zero active keys therefore fails closed before exposure in either mode. Non-loopback `disabled` remains allowed only because it is an explicit operator choice and is labelled insecure throughout local status surfaces.

Application authentication provides no confidentiality. Loopback is local; a trusted VPN such as Tailscale can encrypt remote transport; or an operator-managed reverse proxy can terminate TLS. Norted does not include certificate management, and plain HTTP over an untrusted network exposes credentials and content.

## TUI and network independence

The CLI composition root establishes attached or owned serving mode before the TUI enters terminal mode, so bind/auth/control startup failures are reported directly instead of becoming a misleading control-unavailable screen. It does not wait for owned model discovery. Once serving ownership is established, the TUI draws its pending NotScanned frame, starts local model discovery asynchronously, and renders Scanning then Ready/Ready with warnings or Failed from the core registry state while periodic control observation and API-key refresh also run. The Server page derives configured bind/auth policy for that first frame and then observes the real server endpoint and small local key count asynchronously. Remote search begins only after a user search action, and install/update work remains off the event loop. Release acquisition reports bounded byte progress; source acquisition reports prerequisite/fetch/verify/configure/build/probe/activation phases without fake bytes. The generic format picker, model list, runtime details, and settings editor consume NInfer's domain/schema data without an engine-specific TUI. Keyboard, mouse, narrow layout, ASCII mode, and `NO_COLOR` are presentation concerns isolated in `norted-tui`.

The hard invariants are:

- no ordinary startup path depends on the runtime catalog network;
- release packages require authoritative archive SHA-256 verification;
- source builds require canonical commit/tree + fixed recipe + observed toolchain + result SHA-256 provenance;
- extraction and removal remain contained within exact Norted-owned roots;
- activation happens only after an exact adapter probe;
- runtime versions are immutable and side by side;
- selection always names a concrete runtime and never hard-codes format-to-engine identity;
- model discovery is bounded local metadata work and never triggers model download or runtime build;
- only the supervisor owns generic child mechanics;
- only adapters own backend-native behavior;
- only the API crate owns public Responses and Chat representations;
- public API keys and private control credentials remain separate authentication domains;
- no public path exposes a private backend or control operation;
- at most one backend is active.
