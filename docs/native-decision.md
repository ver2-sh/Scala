# Native Decision integration

Scala exposes the engine-neutral `ApiCapability::Decision` and
`EngineFeature::Decision`. The explicit `native_decision` source-runtime engine
also supports native label and trained linear-readout serving through verified
local source bundles; see [source runtimes and Norted qualification](native-decision-source-runtimes.md). The authenticated `POST /v1/systemone` surface uses
normal Model Profile IDs (including Link aliases), session/role routing, JIT
resolution and request leases. It never calls chat, completion, logprobs, grammar
or prompt-based approximations. llama.cpp supports qualified official upstream
runtime/model pairs through its native `/v1/systemone` interface. q27 and NInfer
still grant no Decision capability. A valid request to an existing unsupported profile returns
HTTP 400 with the existing `invalid_request_error` envelope, `param: "model"`
and `code: "unsupported_capability"`. Missing profiles retain the normal 404.

## Wire contract

The request follows the [official System One schema](https://api.typesafe.ai/openapi.json):

```json
{
  "model": "my-profile",
  "state": {"message": "Please review this item."},
  "questions": {
    "route": {"type": "choice", "criteria": {"accept": "Allowed", "reject": "Disallowed"}},
    "urgency": {"type": "score", "criteria": ["Can wait", "Needs attention"]},
    "safe": {"type": "noul", "instructions": "Is this safe?"}
  }
}
```

`DecisionContent` retains string/object/array state without converting it into
text. Optional instructions accept the same shapes or null. Choice criteria map
names to descriptions (or null); score criteria retain array order; optional noul
criteria use `true` and `false` descriptions. Named questions must be nonempty;
choice/score criteria must be nonempty. Native adapters own additional limits.
Unknown fields and text-generation controls are rejected.

`DecisionRequest` carries the resolved profile ID, state and question map.
`DecisionOutput` carries named, type-tagged `DecisionAnswer`s: `choice` (string),
`score` (number, with optional native `legend`), or `noul` (number).
`probabilities` and `confidence` are optional native observations and omitted
when absent; Scala neither calculates nor fills them. Optional token usage uses
the existing `InferenceUsage` contract. The response contains `model` and
`answers`, optional `usage` with input/output counts, and a `scala` identity
extension derived from existing launch provenance: profile ID, artifact ID/hash,
native artifact identity and immutable runtime identity. Private paths/endpoints
are excluded. `model` preserves a native reported name when available, otherwise
it remains the client's profile alias; that alias is not a native model identity.
Full launch/package lineage remains available through existing private control
provenance. No decision observations enter persisted overrides.

## Native llama.cpp qualification

The supported interface is official llama-server's
[POST `/v1/systemone`](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md#post-v1systemone-typesafe-compatible-system-one-api),
merged in [#29818](https://github.com/ggml-org/llama.cpp/pull/29818).
Laya, Julia-1, Lev, OpenJev, Kev, Nimble and Clef are native model types, never
Scala engines. They remain ordinary GGUF artifacts bound to ordinary llama.cpp
Model Profiles. No fork or custom runtime is required.

The bounded GGUF reader retains `{general.architecture}.decision.type`. A
nonempty native type admits a candidate for JIT loading and suppresses generation
and embedding claims; it does not grant Decision. Filename, repository, GGUF
format and Norted metadata grant nothing. Upstream owns recognition of the native
type, so Scala maintains no classifier architecture/name allowlist.

Before launch, Scala binds a process-local proof to the immutable runtime ID,
verified executable SHA-256, artifact ID/path/hash/size/native identity, profile
ID and resolved configured settings. After startup it checks the private
backend's `/props` model path, alias and build identity, and the single loaded
entry in `/v1/models`. Current upstream
[#29987](https://github.com/ggml-org/llama.cpp/pull/29987) reports
`architecture.output_modalities`; when present it must include `decisions`.
Unknown additional modalities are tolerated. Earlier System One nightlies omit
that field.

Scala also sends `{}` to `/v1/systemone`. Official upstream checks the native
decision type before parsing questions: an ordinary model returns 501, an old
server without the route returns 404, and an initialized Decision model returns
400 `invalid_request_error`, message `"state" must be provided`. That exact
validator response proves the native interface/model pair without running an
inference task. Generic HTTP 400, a version string, or metadata alone cannot
qualify it. Decision candidates skip Scala's generative chat capability probe.
Proofs are cleared on unload/crash and replaced before every launch attempt.
Changes to the runtime, executable, artifact, profile or configured settings
require a fresh proof. Observations never enter persisted settings.

The shared `native_decision_supported` gate still controls dispatch,
`capabilities.decision: true` and contextual `ModelServingCapabilities`. An
unloaded/unverified pair cannot advertise execution-qualified Decision.

Public Model Profile discovery additionally exposes
`capabilities.decision_candidate: true` for a resolved, configured native
candidate with a compatible installed runtime. This is a selectable opportunity
to attempt native qualification, not proof of runtime support. The adapter must
explicitly opt in; the shared gate checks engine/API declarations, immutable
runtime identity, compatible probe/format/model and resolved settings. llama.cpp
opts in only for architecture-scoped native GGUF Decision metadata bound to a
profile. Names and versions grant nothing. Missing/incompatible runtimes or
invalid settings do not produce a selectable candidate. A compatible old llama
runtime can be attempted but still cannot advertise `decision: true` or execute;
its missing native route fails qualification.

Public discovery supports both client conventions: a genuine candidate or
qualified Decision pair also exposes `architecture.output_modalities: ["decisions"]`.
`GET /v1/models?output_modalities=decisions` filters this same catalogue; unfiltered
discovery and Link IDs/ownership retain their existing behavior. This architecture
field describes the modality available for qualification and grants no execution
capability. Ordinary chat, incompatible artifacts, missing runtimes and invalid
configuration receive neither Decision modality nor candidate capability.

Runtime inventory observations are shared across profiles, API requests, startup
status and settings resolution. They remain process-local and retain the exact
verified runtime identity; no observations enter Settings or Model Profiles.
Warm inspection checks file/directory metadata without hashing runtime payloads
or relaunching integrity probes. Changes invalidate the observation, explicit
runtime-list refresh discards it, and its verification age is bounded to five
minutes (warm reads do not renew that age). Selections, profile settings and
artifact compatibility are resolved on each discovery request. Cold external
probes run concurrently and discovery bounds each adapter's probe wait to eight
seconds; a timeout returns an unavailable runtime with a warning, and subsequent
inspection can retry. This discovery bound does not change authoritative launch
verification. An observation is never execution proof: each adapter still verifies
the selected runtime before launching, and native Decision's wrapper repeats
complete closure validation before loading weights.

Coded consumes the explicit candidate through its existing transient discovery
catalogue, Decision picker, identity-only role selection and native tool. Manual
or persisted flags cannot grant either field. Cold starts and unloads therefore
retain discoverability without retaining any process-local proof. The first
ordinary `/v1/systemone` request JIT-loads, checks the exact pair and returns
`unsupported_capability` if qualification fails, with no fallback. A Scala
restart creates a new manager and adapter; unload clears proof and the next
request qualifies again. No observation or synthesized instruction is persisted.

Old runtimes remain unsupported. Decision classifiers advertise neither
`TextGeneration` nor chat, Responses, streaming or embedding capability. Ordinary
chat and pooled embedding GGUF behavior is unchanged.

## Native mapping and runtime discovery

The adapter sends only `state` and `questions` to the already-running private
llama-server's `/v1/systemone`. It never sends a model selector. JSON content,
choice descriptions/nulls, score order and noul `true`/`false` criteria remain
unchanged. Answers share Scala's type-tagged schema; native probabilities,
confidence, legends and model identity are retained and absent observations stay
absent. Native `usage.input_tokens` and `usage.output_tokens` map to existing
usage counters; `total_tokens` is their checked sum. No timing/cache observations
are invented. HTTP, malformed-response and transport failures use existing
`EngineError` behavior with no alternate route or fallback.

Concrete upstream validation is stricter than Scala's structural contract:
question instructions must be provided and score criteria must have 2–10
levels. The llama.cpp adapter serializes `instructions: ""` only when the public
question has `instructions: None` (including public null). Explicit string,
object and array instructions retain their JSON semantics. It does not mutate
the public request, infer prompt text or persist the empty value. Current
[upstream parser](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/server-decision.cpp)
rejects only missing/null instructions, so an empty string is accepted. The
adapter also rejects scores outside 2–10 levels locally before HTTP execution,
with the question name and received count. Valid levels/order/content are
unchanged. These limits are llama-specific; the engine-neutral optional
instructions and nonempty score contract, q27 and NInfer remain unchanged.

As reviewed on 2026-10-05, official nightly
[b11425](https://github.com/ggml-org/llama.cpp/releases/tag/b11425), commit
`e117148a41d8e9bedb72e4c6c3f003ab0fe7f857`, contains #29818, Nimble/Clef support and
the Laya correctness fix [#29903](https://github.com/ggml-org/llama.cpp/pull/29903).
It predates the model modality field and qualifies through the native validator.
Current master identifies itself as the 0.6.0 development line; version alone is
never a support gate.

Scala's existing official nightly/source providers can surface this revision.
Managed CUDA source recipes are now `managed-portable-v5` (CUDA 12) and
`managed-portable-cuda13-v3` (CUDA 13), explicitly disabling both historical and
current upstream CCCL download guards. Old recipe/runtime identities and
selections remain immutable. No runtime is installed or automatically selected
by this integration. Laya/Clef native batches must fit the selected runtime's
`--ubatch-size`; native model limits remain upstream-owned. Actual installation,
host/toolkit qualification, model acquisition and operational inference are
separate steps.

Profiles continue to bind an ordinary artifact and engine target. GGUF, Q27 and
NInfer formats, filenames, origin and Norted metadata/provenance alone never grant
Decision. For llama.cpp there is no new artifact format, profile role or persistent setting.
Tests use fake adapters and synthetic loaded backends; no native inference runs.
