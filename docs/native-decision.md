# Native Decision integration

Scala exposes the engine-neutral `ApiCapability::Decision` and
`EngineFeature::Decision`. The authenticated `POST /v1/systemone` surface uses
normal Model Profile IDs (including Link aliases), session/role routing, JIT
resolution and request leases. It never calls chat, completion, logprobs, grammar
or prompt-based approximations. llama.cpp, q27 and NInfer currently grant no
Decision capability; a valid request to an existing unsupported profile returns
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

## Future adapter contract

1. Wait for an upstream release with a native decision interface. Jev/System One
   and Laya are reference examples, never engine names or privileged concepts.
   In particular, [llama.cpp PR #29363](https://github.com/ggml-org/llama.cpp/pull/29363)
   is not a Scala integration contract; development CLIs and unreleased endpoints
   are not probed or implemented here.
2. Qualify that released interface in the Scala adapter using the ordinary
   installed-runtime identity, probes and compatibility checks. Keep the
   executable separately managed; no custom runtime build, fork or vendor copy
   is required by this contract.
3. Establish compatibility for the concrete runtime, artifact and resolved
   settings. Advertise Decision in the existing engine API/features lists,
   qualify artifact support through `supports_model_capability`, expose it in
   `serving_features`, and opt into `supports_native_decision` only for verified
   pairs. Its default is false, even if the engine-wide lists include Decision.
4. Implement `EngineAdapter::decide` as native request/response translation.
   Its default is `EngineError::Unsupported`; rejection never triggers fallback.
   Preserve native answers, distributions, confidence and identity exactly.
5. The shared `native_decision_supported` gate controls both dispatch and public
   Model Profile discovery (`capabilities.decision: true`). Unknown or unqualified
   runtime/model combinations grant nothing. Existing thinking capabilities stay
   unchanged, and false Decision is omitted from discovery.
   `ModelServingCapabilities` also carries Decision through this same gate when
   concrete runtime and resolved-settings context exists; format-only reports
   cannot grant it.

Profiles continue to bind an ordinary artifact and engine target. GGUF, Q27 and
NInfer formats, filenames, origin and Norted metadata/provenance alone never grant
Decision. There is no new artifact format, profile role or persistent setting.
Tests use fake adapters and synthetic loaded backends; no native inference runs.
