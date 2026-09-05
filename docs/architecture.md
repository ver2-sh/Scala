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
- `norted-model-library` owns the provider-neutral model catalog/acquisition contract, Hugging Face API provider, bounded in-process download scheduler and authoritative job snapshots, streamed/resumable downloads, receipts, safe import, atomic activation, and managed-only removal. It performs no model transformation or runtime acquisition.
- `norted-engine` owns `EngineAdapter`, `EngineRegistry`, the provider/catalog/cache, secure installer, runtime store and resolver, runtime/backend manager, generic process supervisor, control client, and normalized inference types.
- `norted-engine-llama-cpp` owns official llama.cpp asset classification, the immutable managed Linux CUDA source recipe/provider, binary probes, flags/environment policy, readiness and `/props`, and Chat Completions JSON/SSE translation.
- `norted-engine-q27` owns official q27 binary/source capability classification, exact Makefile target recipes, tokenizer requirements, usage-signature probes, flags/environment policy, readiness, provable sampler settings, and Chat Completions JSON/SSE translation.
- `norted-engine-ninfer` owns the canonical source-snapshot catalog, exact target-registry enumeration, `.ninfer` compatibility, source/external runtime probes, settings and GPU policy, startup-log observation, and private Chat JSON/SSE translation.
- `norted-api` owns public bearer/request middleware, the shared sanitized error contract, Responses and Chat Completions text surfaces, and the private authenticated control HTTP surface. It does not construct upstream flags or expose backend-native bytes.
- `norted-tui` owns terminal lifecycle, responsive rendering, runtime search/install/selection interaction, the generic schema-driven load-settings editor, and normalized control observation.
- `norted-server` is the composition root and scriptable CLI.

Adapter registration and provider registration are separate. A built-in engine can be enabled even when no runtime is installed; conversely, every installed manifest still requires its corresponding registered adapter before it can launch.

## Model Library and acquisition

`ModelRegistry` remains the single local model inventory. Every scan includes configured external
paths plus `<data>/models`; there is no parallel registry per engine or format. One receipt at the
managed acquisition root enumerates every primary member and supplies stable logical identities and
per-member acquisition provenance without changing upstream bytes. Ordinary rescans retain each
downloaded artifact's `ModelId` when transient catalog ordering changes, but receipt provenance is
accepted only when the current primary size still matches. Its verified acquisition digest remains
provenance; `ModelArtifact.hash` is reserved for a current content hash actually established by the
load-time package integrity path.

`ModelCatalogProvider` is independent of `RuntimeCatalogProvider`. The initial Hugging Face provider
uses the HTTP metadata API to group relevant siblings by repository and exact revision, and returns
individual `.gguf`, `.q27`, and `.ninfer` variants with size/LFS SHA-256 when published. An exact
reference has provider, repository, revision, and filename identity. The TUI selection retains and
renders that repository identity, so equal filenames cannot alias. Provider results are only
format candidates; they do not advertise a compatible engine or runtime before local inspection.
No repository or model family is hard-coded into this generic provider.

Acquisition streams each response to a cache `.part`, uses a Range request when partial data exists,
checks authoritative size and SHA-256, then assembles all files in a registry-excluded
`.norted-staging` directory on the managed root's filesystem. Raw q27 tokenizer selection calls the
same core selector used by local discovery, including the upstream `MODEL.q27` + sole
`TOKENIZER.tok` layout.

`ModelLibrary` owns a bounded acquisition scheduler. Every request receives a stable UUID and an
authoritative record containing exact reference, resolved provider/repository/artifact identity,
phase, bytes, optional total/percentage, measured rate, elapsed time, optional ETA, queue position,
and status. Up to `server.max_parallel_model_downloads` acquisitions run at once (default 4,
minimum 1); the rest remain in a FIFO queue. Raising the Server Setting fills newly available slots
immediately. Lowering it never cancels work already running and suppresses starts until active work
falls below the new limit. Completion and failure both release a slot. Exact active/queued
references are deduplicated, and a per-reference cache key plus per-job staging directory prevents
cross-acquisition file mixing. Companion files remain sequential within one acquisition.

After resolution, downloads take a per-acquisition `fs2` file lock keyed by the canonical receipt
acquisition identity. The lock lives in the stable model-download cache lock area and covers all
partial-cache mutation, staging, validation, and atomic activation. Managed removal takes the same
lock. Separate process-local schedulers (TUI or synchronous CLI) can therefore run unrelated
acquisitions concurrently, while a same-acquisition waiter rechecks the destination and reports it
as already installed after the owner completes.

Progress broadcasts carry the job UUID, but they are presentation invalidations rather than the
source of truth; the TUI re-snapshots all jobs after events and on its render cadence, reconciles
unseen Installed job IDs into model discovery refreshes, and therefore does not depend on receiving
an individual completion broadcast. Search is a separate task and managed removal uses only its own
narrow acquisition lock. Terminal history is bounded. Scheduler state is process-local and survives
ordinary TUI navigation but not restart; resumable partial cache files do survive and are reused by
the same exact reference.

Norted package acquisition planning is a pure `norted-core` interpretation of the same locally
accepted `BUILD-MANIFEST.json`, `Q27-MANIFEST.json`, and `NINFER-MANIFEST.json` schemas. A provider
searches only exact ancestor manifest paths for the selected repository filename; filenames inside
the manifest resolve relative to that manifest directory. It never chooses a repository-global
manifest by basename. The plan contains the complete required closure, primary membership,
auxiliary roles, and declared size/hash facts. The HTTP provider maps those safe package-relative
paths to repository filenames without interpreting lineage or coupling core to Hugging Face.

Staging preserves the repository/package directory tree, rejects absolute, parent, backslash,
collision, and containment escapes, creates exact parent directories, and verifies all declared
members before activation. GGUF packages retain the manifest and declared projector; q27 packages
retain the exact manifest-bound tokenizer, Sharp, all outputs, and lineage; NInfer packages retain
Sharp, all outputs, native identity, and lineage. The complete staged directory is rediscovered and
validated before one rename to:

```text
<data>/models/huggingface/<publisher>/<repository>/<revision>/<artifact-key>/
<data>/models/imports/<stable-import-key>/
```

GGUF uses bounded metadata inspection, q27 uses adapter-owned bounded architecture/tier inspection
plus validated tokenizer presence, and NInfer uses its native v2 container inspector. NInfer does
not gain invented tokenizer/template sidecars. Failed or cancelled staging is excluded from discovery;
all required files must complete before activation. The package-level receipt records one managed
acquisition identity and every primary member, with provider, repository, revision, remote filename,
local size, source URL, acquisition time, and authoritative acquisition digest when available. It is
model-acquisition provenance, not a substitute for Norted Builder lineage.

Import copies by default. If the source is a valid Norted package, only the manifest-planned file
closure is copied, with nested relative paths intact; unrelated neighboring files are excluded and
the original package manifest remains authoritative. Every primary output becomes an individual
registry/Profile target but shares one atomic managed acquisition. Removal derives a contained plan
from the receipt, revalidates every member, and exposes all affected `ModelId`s. The CLI and TUI
compare that full set with private control state and refuse deletion if any member is active, then
remove only the acquisition root below `<data>/models`. Configured external artifacts are read-only.

The TUI Models screen is one integrated Model Library with Installed and Discover modes, query and
format controls, concrete artifact details, a responsive multi-job panel with progress/rate/ETA or
FIFO position when calculable, download, managed removal, and the existing runtime picker for
verified compatibility. The CLI exposes the same search, synchronous download,
import, remove, list, and info model. Runtime search/install/update and model build/transformation
remain separate systems.

## Settings and Model Profile resolution

`Engine`, `Runtime`, `ModelArtifact`, `ModelProfile`, and request-time generation patches are
distinct domains. `ModelProfile` is deliberately small: typed ID, display name, concrete bound
artifact ID, bound engine ID, and typed overrides. It is always user-owned and mutable. Technical
compatibility comes from the bound artifact, registered engine, exact runtime, and resulting
settings—not from profile-authored applicability declarations. Runtime IDs must be qualified by the
bound engine; unqualified or foreign namespaces are rejected at every layer.

`SettingsState` schema 3 lives at `<data>/settings.json` and stores Server Settings separately from
independent per-runtime defaults. `ModelProfilesState` schema 2 lives at
`<data>/model-profiles.json`. Both stores have
independent locks and atomic replacement. Settings schemas 1 and 2, model-profiles schema 1, and
obsolete Global fields are rejected; no compatibility reader, alias, or migration path exists.
Current inference overrides use fully qualified engine setting IDs; unqualified names are not
accepted. Because the application is unreleased, stale development state should be recreated or
manually updated by the developer/user.

One resolver applies the selected runtime's defaults, Model Profile overrides, then ephemeral
invocation overrides. It rejects unrelated runtime namespaces and records the winning inference
source as runtime default, Model Profile, or invocation. In boot-loading presentation, invocation
is labeled `boot inference`. There is no Global inference parent, model-default layer, or hidden
policy. Relative typed paths resolve beneath the data directory before downstream consumers receive
the same absolute value.

Server Settings use only `server.*` and never enter inference resolution or provenance. Runtime
definitions and values use only `llama.cpp.*`, `q27.*`, or `ninfer.*`. Generic code may construct
repeated metadata shapes but owns no inference setting identity, runtime value, default, or
adapter-specific conflict rule. A future apply-to-all-runtimes action must copy a value independently into
each applicable runtime; it must not create a shared parent layer.

`ResolvedSettings` carries explicit launch configuration separately from the complete effective map.
The exact adapter schema materializes every supported effective row as a concrete value, winning
source, and optional derivation detail. Runtime/model/host calculations remain part of the runtime-
default layer. A genuine unresolved runtime policy is represented canonically (for example `auto`),
not replaced with a presentation-only scalar. Only deliberately Norted-owned execution defaults are
materialized into launch configuration. Startup-confirmed results supersede automatic policies in
the running effective map without creating a new source layer; when the value changes, structured
`requested_value` retains the pre-start policy/value. The Model Profile editor consumes current
resolution for its primary rows and presents a matching backend's effective map only as secondary
running state. Server/status/control surfaces continue to consume the running map. No consumer
reconstructs a value from descriptive default prose.

Runtime selection receives the profile's explicit engine and cannot switch engines. Existing
resolution order remains explicit runtime, persisted model/runtime choice when applicable,
engine/format default, then best compatible installed runtime. Adapters expose a typed,
category-aware, wholly engine-qualified schema, gate it first with facts proved by the bound model, and
then gate it with the exact runtime contract before validating effective settings. Capability
requirements are consequences of selected settings. q27 retains exact source fingerprints and
bounded context/KV/W_MAX startup proof; NInfer retains native-container identity, capability-domain
source fingerprints, and schema-20 startup proof;
llama.cpp uses exact help evidence for every structured launch control and keeps Norted-owned
system-prompt/context management separate from native launch flags.

`RuntimeProvenance.model_profile` records the ID, display name, deterministic content hash, bound
artifact ID, and engine ID. Artifact lineage and hashes remain separately recorded. Settings
provenance contains the complete concrete effective map and source attribution. Values confirmed by
startup observations replace pre-launch calculations without changing the winning layer, while a
changed requested policy/value remains available as structured provenance. No artifact package metadata
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

A separate provider supplies Linux x86_64 CUDA as managed source builds, not as fabricated release assets. It checks official nightlies newest-first and selects the newest exact revision whose bounded root, ggml/CUDA, common, tools/UI, and server CMake files still prove the Norted-owned build contract. The check covers the CUDA option and backend path, explicit-architecture override, server target/output/C++17 contract, and the flags that keep optional CCCL, NCCL, LLGuidance, UI, OpenSSL, and subprocess behavior out of the managed build. A drifted newest nightly is skipped in favor of the newest earlier admitted nightly. One admission scan produces both supported CUDA-toolkit variants from the same commit/tree; candidate revalidation uses the selected exact variant to reconstruct its recipe without substituting the parallel variant. Runtime Search never clones or compiles source.

The current CUDA-12 source identity is `managed-portable-v4`; `managed-portable-v1`, `managed-portable-v2`, and `managed-portable-v3` remain immutable historical identities. V4 preserves V3's CMake/Ninja Release build, shared-library layout, and `CMAKE_BUILD_RPATH_USE_ORIGIN=ON` policy, so the executable and libraries kept together under `build/bin` use `$ORIGIN`-relative build-tree paths after staging moves into the immutable runtime store. Its only toolchain-contract change is replacing V3's PATH-selected nvcc semantics with the stable `/usr/local/cuda/bin/nvcc` activation boundary. Exact managed V1 identities are no longer serving-compatible: V1 predates the origin-relative policy and can retain staging/build-tree library paths after atomic relocation. Its manifest, RuntimeId, installation, selections, and update identity are not rewritten or deleted; V1/V2/V3 remain on the CUDA-12 update line so V4 is discoverable.

The parallel CUDA-13 source identity is `managed-portable-cuda13-v2`; V1 remains its immutable historical predecessor. It deliberately is not an update generation of the CUDA-12 line because its host toolchain and driver contract differs. Both current recipes build only `llama-server` at `build/bin/llama-server` and use the same fixed CMake/RPATH behavior. Their deterministic real-code policy is `75-real;80-real;86-real;89-real;90-real;120a-real`, represented as exact compute capabilities 7.5, 8.0, 8.6, 8.9, 9.0, and 12.0 and accelerator target `sm_75+sm_80+sm_86+sm_89+sm_90+sm_120a`. CUDA 13.3 nvcc was also checked directly: all base targets are reported by its list actions and an `sm_120a` compile succeeds.

CUDA-12 V4 explicitly admits Toolkit `>=12.8,<13.0` and NVIDIA Linux driver `>=525.60.13`. CUDA-13 V2 explicitly admits Toolkit `>=13.0,<14.0` and driver `>=580.65.06`, NVIDIA's exact CUDA 13.0 GA Linux floor. The exclusive ceilings keep later major families from silently changing either immutable contract. See NVIDIA's [minor-version compatibility table](https://docs.nvidia.com/deploy/cuda-compatibility/minor-version-compatibility.html), [CUDA 13.0 release table](https://docs.nvidia.com/cuda/archive/13.0.0/cuda-toolkit-release-notes/index.html#cuda-toolkit-and-minimum-required-driver-version-for-cuda-minor-version-compatibility), and [CUDA 13 compiler target list](https://docs.nvidia.com/cuda/archive/13.0.0/cuda-compiler-driver-nvcc/index.html#gpu-compilation). Before checkout or compilation, the generic prerequisite probe executes the recipe's exact `/usr/local/cuda/bin/nvcc` prerequisite against the Toolkit bounds and every explicit real/virtual target. It uses `--list-gpu-code`/`--list-gpu-arch`; because CUDA 13 list output omits accepted architecture-specific suffixes, the compiler's own option enumeration supplies that exact supplemental evidence. One generic plan function constructs the effective CMake configuration: it starts from provider-owned recipe arguments, rejects any competing provider-authored `CMAKE_CUDA_COMPILER`, and appends the typed compiler binding. Prerequisite validation, the configure process, persisted effective arguments, and installed-candidate matching use that same contract. The manifest retains provider arguments separately, and observed CMake/Ninja/C++/nvcc versions remain toolchain identity. Older schema-2 source manifests omitted the effective list; they remain readable and can match their unchanged recipe without inferred data or on-read rewriting, while a present effective list must match exactly. A known driver below the relevant floor is incompatible; a wrong active Toolkit makes that variant needs-attention. Norted never installs driver, Toolkit, or toolchain packages.

Update discovery and installed fallback ranking ask each engine for a logical functional variant and optional source-recipe generation. llama.cpp maps only exact managed Linux x86_64 CUDA V1, V2, and V3 identities to the CUDA-12 family ordered 1, 2, and 3; CUDA-13 V1 is a separate family. On otherwise identical source, the higher generation ranks newer before RuntimeId lexical ordering, while a genuinely newer upstream commit still wins. CPU, Vulkan, Windows CUDA, external runtimes, unknown future generations, and unrelated toolchain families retain exact variant lines. Pinned/model/format selections remain exact and are never rewritten when a side-by-side update is installed. The generic source-prerequisite memoization key is derived beside the checker from the complete immutable recipe and prerequisite structs, so the two Toolkit contracts cannot share a compatibility result accidentally.

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

The selected ID does not bypass manifest, host, adapter, or model validation. Each adapter supplies engine-neutral installed and available-runtime model+runtime+host compatibility results, optional semantic preferences, and an exact ordered accelerator binding when it binds devices. Explicit invocation, model overrides, format defaults, compatible-runtime listing, fallback, the model picker, model-triggered catalog search, and load admission use that contract. Generic catalog search remains model-independent. A stale stored preference emits a runtime notice before fallback. Fallback ranks actual compatibility, engine preference, managed provenance, current version, and finally runtime ID. For q27, official managed CUDA targets are attached to each version and checked on each candidate device before VRAM; a known unsupported device is rejected and an unobserved compute capability is needs-attention. W8 leads on confirmed 24 GiB-class or unresolved-VRAM devices, W12 leads only when the selected compatible device positively satisfies the 32 GiB class, and W16 is a valid explicit specialist choice but never wins fallback accidentally. W16 rejects confirmed 24 GiB-class hardware and remains needs-attention above that known lower bound because upstream publishes no exact floor. External q27 binaries retain their exact executable identity but remain needs-attention because their width, CUDA targets, and runtime-specific floor are unverified; this makes a verified suitable managed pack win ordinary fallback without invalidating an explicit external choice. Selection source and the full ordered binding are carried through residency/status and into launch provenance.

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

The current `ninfer-serve-v2` recipe configures Release with Ninja, apps on, tests/benchmarks off, CUDA architecture `120a`, and the stable `/usr/local/cuda/bin/nvcc` compiler contract, then builds only `ninfer-serve`. The prerequisite checker executes that path and the persisted effective CMake arguments prove CMake received it as `CMAKE_CUDA_COMPILER`; PATH is not the CUDA compiler authority. Catalog-controlled data is never embedded in a shell command. Build-control environment variables are removed explicitly. Before CMake runs, a bounded scan of CMake definitions rejects FetchContent, ExternalProject, `file(DOWNLOAD)`, Git clones, and HTTP URLs; the inspected upstream tree otherwise uses vendored source and explicit system libraries. The exact source/build tree is retained because upstream defines no install layout and the adapter has not assumed the executable is independently relocatable. Historical V1 manifests remain valid and truthful about their original PATH-selected recipe; V2 is ordered after V1 on the same functional update line.

Git and build children use `kill_on_drop`, stream bounded failure tails, and run asynchronously. Checkout mismatch, dependency audit, configure/build failure, missing entrypoint, hash change, probe failure, cancellation, or store failure removes unique staging. Manifest creation and directory sync precede one atomic activation, so none of these failures can create local installed truth.

## Model artifacts and native identity

`ModelArtifact` retains the primary artifact and stable ID, engine-neutral `AuxiliaryArtifact` values with roles, and an optional typed `ArtifactNativeIdentity`. Q27 discovery prefers `model.q27` + `model.tok`; for quantized names it may use the unique longest boundary-safe prefix tokenizer. Candidates must be beside the model and contain `Q27T` magic with supported header version 1. `.tok` is never a primary model. Missing, invalid, or ambiguous companions are expressed by q27's model compatibility result and prevent launch.

GGUF inspection reads a bounded version-2/3 metadata header and never maps tensor payloads. Its
typed identity retains architecture, context length, expert/expert-used counts, and a digest of the
exact encoded `tokenizer.ggml.*` metadata. The architecture supplies the only legal
`<architecture>.expert_used_count` override key; absence of the exact metadata makes the active
expert setting model-unsupported. Draft GGUF admission requires matching tokenizer metadata and a
recorded whole-file SHA-256, while exact llama-server startup remains the definitive draft
architecture/tensor check.

Q27 model inspection reads a fixed 16-byte `Q27F` v1 header, rejects metadata lengths above 1 MiB before allocation, reads only that JSON blob, and never maps/hashes tensor payloads. The q27 adapter validates the current `qwen35` 65-block/MTP architecture constants. Exact published tiers come from `quant_policy` plus the presence/value of `q4_head` and `q8_extra`, never the filename. Qwen3.6 default/q4s/q5f map to 24 GiB-class, q6/q6f/q6k to 32 GiB-class, and q8 to 48 GiB-class; Qwen3.8 v2 q4s/default/q6 map to 24 GiB-class and q6k to 32 GiB-class. Unknown recipe tuples remain NeedsAttention.

NInfer version-2 containers begin with `NINFER\0\x02` and a little-endian 64-bit JSON directory length at byte 8. Discovery caps the directory at 16 MiB, proves it lies within the file, validates the closed identity/object descriptor shapes and ordered non-overlapping object ranges, and reads no later weight bytes. Version 1 is not migrated or rewritten. The resulting typed NInfer identity contains `container_version`, `model_id`, and `weights_id`; it is carried through prepared input, compatibility, private model information, and launch provenance without being placed in `architecture` or exposed by public `/v1/models`.

The `.ninfer` artifact is one primary file whose tokenizer/template/frontend resources remain embedded. The NInfer adapter re-inspects bounded metadata during preparation and again before launch. Installed managed runtime manifests enumerate exact native identities by parsing the declarative target registry in their own retained source snapshot. The parser fails all-or-nothing: if upstream changes the registry structure, compatibility becomes NeedsAttention and startup remains definitive rather than accepting a hard-coded filename or stale global list. No discovery path invokes a runtime, GitHub, Hugging Face, payload hashing, model download, conversion, or v1 migration.

## Adapter launch contracts

### llama.cpp

The adapter probes a managed runtime's exact contained entrypoint, binary hash, `--version`, and `--help`, requiring `--model`, `--alias`, `--host`, and `--port`. For managed build tags it also checks the reported build/revision relationship rather than trusting the archive name alone. External `binary_path` uses the same interface probe but keeps its source unverified.

Launch is conceptually:

```text
llama-server --model <canonical-gguf> --alias <stable-id>
             --host 127.0.0.1 --port <dynamic>
             [--device CUDA0[,CUDA1,...]] [explicit structured settings]
```

The adapter polls `/health`, then obtains authoritative effective `temperature` and `top_p` from
`/props` before Running. Exact-help-gated launch controls cover context/parallelism, samplers,
threads/batches, weight and KV offload, unified KV, context checkpoints, Flash Attention, cache
types, RoPE base/scale, `load_mode`, CPU MoE placement, architecture-aware active experts,
reasoning, chat template, and speculative mode/draft artifact. Every setting
owns its current aliases and environment variables for collision removal/rejection. Omission emits
nothing. `load_mode` intentionally replaces deprecated mmap/mlock/direct-I/O controls.

For CUDA runtimes, omitted `llama.cpp.devices` retains automatic selection of one compatible stable GPU
UUID. An explicit setting selects a nonempty duplicate-free ordered set of exact currently visible compatible
UUIDs. Launch writes that same ordered list to `CUDA_VISIBLE_DEVICES` and only then converts it to the
runtime-local `CUDA0,CUDA1,...` names accepted by current llama.cpp. `main_gpu`, tensor split, and fit-target
arity are validated against the selected set using upstream index/broadcast/partial-list behavior;
`split_mode=none` is passed through with its upstream main-GPU semantics. Raw native arguments are disabled,
and every exact-help option must be classified before the runtime is admitted. Inherited llama/MTMD/GGML/
LLGuidance/AIP controls are dynamically scrubbed; raw device/RPC, multimodal, router, API/security, one-shot,
and other contract-changing aliases cannot bypass structured configuration.
Inference-affecting LoRA/control-vector files are canonicalized and SHA-256-bound before launch,
with each scaled entry retaining its scale in both arguments and provenance.

The three-layer resolver normalizes llama.cpp semantic alternatives before compatibility, launch,
inspection, and provenance: a higher-layer built-in/file template choice or all/exact CPU-MoE
choice suppresses its inherited sibling, while `off`, n-gram, and `draft-mtp` modes suppress an
inherited external draft identity. Same-layer contradictions remain visible and invalid.

Private Chat requests carry explicit seed, stop, repeat/presence/frequency penalty, reasoning effort, token
limit, and OpenAI-compatible response format only after the active exact schema proves the
corresponding mechanism. JSON Schema wrappers retain schema/name/description/strict fields and use
llama.cpp grammar-backed sampling in both normal and streaming paths. `/props` supplies exact slot
capacity and `/v1/chat/completions/input_tokens` supplies exact fully templated token counts for the
engine-neutral truncate-middle policy; failure of either endpoint fails the request truthfully.
Configured JSON Schemas are per-request defaults on this path rather than launch-time
`--json-schema` constraints, allowing explicit text, JSON object, or request-schema selection to
override them. Current upstream `draft-mtp` consumes main-model MTP heads, not an external draft;
without a bounded architecture-neutral proof of usable heads, compatibility remains
needs-attention until exact server load.

### q27

Current q27-server has no stable `--version` response. The adapter therefore validates the exact binary/hash and its real zero-argument usage signature; managed version/revision provenance remains supplied by the verified package manifest and probe reports only what was observed.

Launch is conceptually:

```text
q27-server <canonical-model.q27> <canonical-tokenizer.tok>
           --host 127.0.0.1 --port <dynamic> --no-think
           [explicit structured settings]
```

Norted disables q27 raw native arguments and rejects every configured `Q27_*` variable. Before launch it dynamically scrubs every inherited `Q27_*` name, including unknown future and diagnostic controls such as `Q27_MPROBE`, then adds back only values generated by reviewed typed settings. Compatibility still requires an accelerator binding containing exactly one UUID-identified NVIDIA device and launch sets separately owned `CUDA_VISIBLE_DEVICES` to that same full UUID; numeric indexes are never correlated across CUDA and `nvidia-smi`. A single inherited UUID constraint is reconciled, while numeric, multiple, unknown, empty, or ambiguous constraints fail closed. Before launch, q27 prepares the already-discovered tokenizer as an engine-neutral auxiliary identity containing role, canonical path, size, and SHA-256. The launch spec uses that exact path and rechecks size/hash immediately before process creation; q27 never rediscovers a companion at launch. The exact v0.10 source contract forwards sampled request seed/top-k/min-p, while greedy requests reject controls upstream would ignore. Request thinking enable/budget and reasoning-effort `none` are accepted only with the explicit `--request-think` profile setting; positive request effort remains an independent trained-template override. The Qwen3.8 trained-template reasoning-effort process default is separately gated by the exact source contract plus bounded model metadata, translated to adapter-owned `Q27_REASONING_EFFORT`, and overridden only for an individual request by deliberately canonicalized q27 request semantics; Qwen3.6 receives no effort setting. Ordinary function definitions, tool choice, assistant/tool history, non-streaming calls, and streaming deltas use engine-neutral types; raw external-template delivery is text-only. Stable `Q27_BATCH` and `Q27_SAMPLED` behavior is typed, while diagnostic/checksum/trace, benchmark, kernel-development, and unstable post-release controls remain unsupported. Readiness requires `/health` status `ok`; JSON and SSE are translated through the same normalized inference types as the other adapters.

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
```

The positional artifact, binding, alias, device, auth/CORS surface, typed load/cache/media/store
controls, sampler/greedy controls, and startup log are reserved. `/health` alone is insufficient for
readiness. Norted selects the bounded schema-20 `server_start` record reviewed at source commit
`863aa8a...`, validates public alias, artifact target/weights and context-cost identity, selected GPU
identity, context/KV/cache/CUDA/speculation state, queue and media limits, thinking state, greedy
state, and configured sampler defaults, then records the runtime-resolved effective values. The file
is unlinked before requests are served; failure and cancellation paths remove it as well.

The current contract admits DFlash plus Vision only for exact
`qwen3.6-35b-a3b`/`groupwise-int`; older reviewed `a140e7ae...` runtimes retain the previous
rejection. MTP and DFlash remain one exclusive backend choice. Capability retention fingerprints
the admission/resource scheduler, context/KV and checkpoint stores, target program/layout/request
plan/resource projection, speculation, Vision residency/prefill, state, and request owners in
addition to immutable commit/tree identity. The structured context-cost preset is canonicalized and
SHA-256-bound before launch; there is no raw native bypass.

Public Responses and Chat messages both become one ordered canonical `InferenceRequest`, then
private NInfer Chat JSON. The reviewed source contract forwards seed, top-k/min-p,
presence/frequency penalties, stops, reasoning effort and thinking enable; its Chat route has no
per-request thinking budget, so the positive configured budget is a launch default. Function tools
are translated only for auto/none choice with parallel calls permitted; required/named/strict and
single-call guarantees fail rather than weaken. Assistant tool calls and tool-result history are
preserved, and tool deltas become Responses and Chat events. Exact registered artifacts expose user
image/video and tool-result image content only when startup proves `--vision`; HTTP(S)/data URLs are
accepted, local paths and unsupported roles/modalities are not. Structured output, raw/stateful
upstream Responses, and Anthropic remain outside this bridge. Later revisions can retain
help- and source-blob-proven launch facts without inheriting changed, unreviewed protocol domains.

## Process, control, and provenance

`RuntimeManager` owns one authoritative collection keyed by Model Profile. Each managed backend independently retains role, JIT/pinned residency, lifecycle, generation, process, private endpoint, settings, exact runtime lease and provenance, request count, last use, and retirement state. Loading uses the same exact resolution and `LaunchSpec` pipeline for manual pinned and inference-triggered JIT loads. Lifecycle operations are serialized, but inference against already-running independent backends is not. Unexpected process exit fails only the matching profile/generation.

Transient logical sessions hold at most one primary-profile lease. Missing attribution maps to the in-memory `default` session. A primary switch releases the old lease and drains/removes the old unpinned JIT backend only when no other session owns it; auxiliary routing never changes the primary. `X-Norted-Session` carries a validated opaque 1–128 byte value and `X-Norted-Role` carries `primary` or `auxiliary`; neither is serialized into engine requests or provenance. Every dispatch takes an atomic inference lease. Streaming wraps the engine stream with the lease guard, so EOF, error, or consumer drop releases it without awaiting a Tokio lock.

A small reaper expires sessions and idle JIT residency, using 3600 seconds for primary/session idleness and 300 seconds for auxiliaries by default, with an LRU cap of two idle auxiliaries. It excludes pinned models, active requests, and leased primaries, and terminates with manager shutdown.

While loading, `RuntimeManager` publishes an engine-neutral `BackendLoadProgress` through the private control status. Progress carries a generic phase (selecting runtime, resolving settings, loading model, verifying startup, etc.), an optional fraction/current/total when the exact runtime exposes trustworthy measurable progress, and an optional human-readable message. Percentages are never invented from phase transitions alone. Engine-specific log parsing stays inside each adapter via an optional `startup_progress` hook; the generic manager owns the progress state and sanitizes untrustworthy numeric values. Progress is cleared on Running, Failed, cancellation, and unload; an old load generation can never overwrite a newer load's progress. Both owned and attached TUI modes observe the same control status.

Private Load is asynchronous admission rather than a model-startup-duration HTTP transaction. While holding the lifecycle-operation guard, `RuntimeManager` validates model existence, allocates a per-backend generation, and publishes Loading before transferring the guard to one server-owned Tokio task. `POST /control/v1/load` returns `202 Accepted`; the CLI polls that exact profile/generation to Running or Failed. Loading an already-resident JIT profile promotes it to pinned. Targeted unload marks only its profile for cancellation, drains inference leases, then terminates it; shutdown cancels and drains every profile. Adapter pre-launch validation remains PreparingLaunch, and SpawningBackend is published only immediately before `ProcessSupervisor::spawn`.

Cross-process CLI/TUI control uses schema-version-2 descriptors under:

```text
<state>/runtime/servers/<instance-id>.json
```

The serving process atomically writes its random identity, PID, public probe address, private loopback endpoint, and random bearer token. Observers prove identity through public health before sending authenticated `status`, `load`, or profile-targeted `unload` requests. Every managed backend status includes its monotonic generation for correlating an admitted load with later observations. Tokens are redacted and absent from public/status/provenance output.

`norted-server serve` is the explicit headless owner. `norted-server` and `norted-server tui` first attach to an already healthy owner whose authenticated private control status also succeeds; when none exists, they construct the same registry, shared runtime-pack manager, runtime manager, public-auth policy, and `ApiServer`, then wait for the published private control API to answer before entering normal interaction. Headless composition requires completed model discovery before reporting readiness. Owned interactive composition permits the registry to remain NotScanned/Scanning so the TUI can draw first and launch `ApplicationCore::start_model_discovery`; public model enumeration and load admission use only the actual registry, so pending discovery cannot fabricate a loadable model. The owned task is coupled to the TUI lifetime: normal exit and TUI errors signal `ApiServer::run`, await its graceful listener shutdown and `RuntimeManager::shutdown`, and let the owned `RuntimePublisher` remove only its own descriptor. Attached mode creates no serving task, so TUI exit cannot stop the external process. Scriptable `load` and `unload` remain control clients and never launch a server.

Private status exposes all managed backends with Model Profile/artifact identity, role, residency, lifecycle/generation, engine/runtime, ordered accelerator binding, process/endpoint, progress/failure/provenance, active request and non-sensitive primary-lease counts, last use, and retirement state. `RuntimeProvenance` remains independent per backend and retains the immutable Model Profile identity/hash/role/bindings, exact runtime manifest and selection source, every selected accelerator observation in launch order, model and auxiliary-artifact facts, source-attributed settings, sanitized native arguments and environment hashes, process identity, endpoint, and launch time. Transient session IDs and mutable pinned/JIT residency are excluded.

## Public protocol

Responses remains canonical and Chat Completions is compatibility-only. Both public parsers
normalize canonical messages, typed text/image/video content, function definitions/choice,
assistant calls and tool results into the same engine-neutral `InferenceMessage`/tool contract,
then attach sampler/seed/stop/penalty/reasoning settings plus an optional structured output
contract. Chat `reasoning_effort` and Responses `reasoning.effort` share typed effort levels.
`RuntimeManager` applies configured system prompt, output limit, sampler/penalty/stop/reasoning, and JSON
Schema defaults only when their request counterparts are omitted, validates request values against
the active exact schema, and never mutates runtime selection or immutable launch provenance. The
resolved effective output format is returned with both streaming and non-streaming routes so a
Responses document reports a configured default schema or an explicit text/schema override
truthfully. The public serving alias and `/v1/models` identity are the active/user-created Model
Profile ID, while
the artifact Model ID remains private provenance.

`POST /v1/responses` constructs response messages/function-call Items and ordered text/tool SSE
events itself. `POST /v1/chat/completions` constructs compatible assistant `tool_calls`, tool-call
deltas, and text chunks from the same normalized inference output. Each adapter owns only its
private upstream JSON/SSE. Output-limit and tool-call terminal reasons map distinctly; unsupported
tool choice, modality, structured output, or sampler behavior is rejected rather than forwarded or
silently weakened.

Public middleware assigns an independent `req_...` ID and returns it as `x-request-id` on success, JSON errors, and streams. `X-Client-Request-Id` is accepted only as ASCII correlation metadata up to 512 characters. Authentication runs before bounded JSON extraction; inference bodies are capped at 32 MiB. Errors share the OpenAI-style `error { message, type, param, code }` envelope and map internal conditions deliberately without local paths, private addresses, credentials, or debug text. No permissive CORS layer is installed. NInfer follows this same canonical path; its upstream Responses/Anthropic/state surfaces are not proxied into a second public surface.

`GET /v1/models` remains the audited OpenAI-style list with exactly `id`, `object`, `created`, and `owned_by`. Runtime metadata and NInfer model/weights identities remain private control-plane state rather than leaking into this public compatibility surface; local `models info` may show the typed identity.

The richer `ModelServingCapabilities` view is local/private: it derives format, compatible
registered engines, compatible installed runtimes, resolved runtime, active state, and gateway
features from real compatibility and selection data. Structured output becomes true only when the
selected exact runtime schema and end-to-end bridge implement it. q27 tool calling requires the
audited runtime-chat route; NInfer tools require a reviewed protocol snapshot, and NInfer Vision
also requires a compatible registered artifact plus startup-configured residency.
`norted-server models info <MODEL_ID>` exposes the view without expanding the public Model object.

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
- backend residency is profile-keyed and active inference is always lease-protected.
