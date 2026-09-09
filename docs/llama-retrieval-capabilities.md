# llama.cpp tool calling and structured output

The adapter carries tool definitions, tool choices, assistant call history, tool
results, aggregate calls and live tool deltas through Norted's engine-neutral
interfaces. Capability advertisement is conservative and startup-dependent.
**This does not mean every llama.cpp model supports tools.**

## Exact serving contract

`EngineCapabilities` describes the adapter's theoretical capabilities:
`TextGeneration`, `ToolCalling`, and `StructuredOutput` for generation models.
`serving_features(runtime, model, settings)` returns the subset proved for the
selected tuple. Pooled embedding models still return no generation features.

The reviewed source is **b10786**, commit
`de8656bd94f1163188125542534e4bcbc9f9fb1f`, tree
`ef599001012ff8bee837a832decde4c564702cc4`.
The chat proof accepts managed source builds with that exact commit/tree,
canonical repository/provider, matching recipe/overlay provenance and executable
hash. Accepted recipes are:

* Ordinary CUDA 12 `managed-portable-v4` and CUDA 13
  `managed-portable-cuda13-v2`, only at the reviewed commit, without overlays.
* CUDA 12 `managed-portable-exact-stop-v2` and CUDA 13
  `managed-portable-cuda13-exact-stop-v2`, with the exact V2 overlay digest.

Other revisions, external binaries and unobserved tuples retain TextGeneration;
a nightly tag, model name, architecture name, help flag or JSON field alone does
not grant chat features. Ordinary source discovery continues following its
moving upstream stream. Exact-stop discovery remains pinned and separate.
V1 exact-stop installations retain their original identity, overlay, validation
and update family; V2 is generation 2 within each exact-stop family.

Before launch, the adapter clears stale proof and verifies the selected
executable. On startup it binds `/props` observations to the runtime ID,
executable hash and model ID. The capability key includes model identity and
configured settings, with package hashes represented consistently before and
after preparation. Unload/failure clears launch state. Observations appear under
`normalized_settings.chat_contract`, never in persisted settings overrides.
Changing settings does not transfer a previous tuple's proof.

`StructuredOutput` requires this reviewed runtime and successful startup
properties. Its request path uses upstream schema/grammar generation and checks
the returned payload against the requested schema before reporting success.

`ToolCalling` additionally requires:

* Explicit typed `llama.cpp.jinja = true` in effective configured settings;
  Settings/profile/load precedence remains unchanged. Inherit is not rewritten.
* `/props.chat_template_caps.supports_tools` and `supports_tool_calls` both true.
* Successful `/apply-template` rendering of sentinel tool definitions, assistant
  arguments, tool results, and subsequent conversation continuation.

Parallel requests require the additional upstream
`supports_parallel_tool_calls` property. An explicit true request fails before
inference if that fact is absent. Omitted parallel policy remains upstream's
property-derived default; explicit false is serialized. ToolCalling does not
promise that every tool-capable template supports parallel calls.

## Upstream audit

All links below refer to the reviewed immutable source, not current HEAD.

| Concern | Reviewed behavior and adapter mapping |
| --- | --- |
| Tool definitions | [server-common.cpp](https://github.com/ggml-org/llama.cpp/blob/de8656bd94f1163188125542534e4bcbc9f9fb1f/tools/server/server-common.cpp) accepts OpenAI function tools with Jinja enabled. Emit `type=function`, name, optional description and the original parameters JSON value. |
| Tool choice | The same request parser reads a **string**; [common/chat.cpp](https://github.com/ggml-org/llama.cpp/blob/de8656bd94f1163188125542534e4bcbc9f9fb1f/common/chat.cpp) accepts `auto`, `none`, `required`. Named-function choice is translated to only that supplied tool plus `required`; missing/ambiguous names are rejected. No unsupported OpenAI object choice is sent upstream. |
| Parallel calls | `parallel_tool_calls` is consumed by the request parser and native grammar/parser. The template capability supplies the upstream default. Explicit Norted values are preserved. |
| Assistant history | `common_chat_msgs_parse_oaicompat` reads call IDs, function names and encoded argument strings. Norted sends the original `assistant.tool_calls` structures, including existing IDs. |
| Tool results | The same parser reads `tool_call_id`. Norted retains `role=tool`, content and the matching ID, in conversation order. Existing system/developer aggregation is unchanged. |
| Aggregate response | [server-task.cpp](https://github.com/ggml-org/llama.cpp/blob/de8656bd94f1163188125542534e4bcbc9f9fb1f/tools/server/server-task.cpp) emits assistant tool calls and `finish_reason=tool_calls`. Each becomes an `InferenceToolCall`; the encoded argument string is retained without reserialization. |
| Streaming response | [server-chat.cpp](https://github.com/ggml-org/llama.cpp/blob/de8656bd94f1163188125542534e4bcbc9f9fb1f/tools/server/server-chat.cpp) emits indexed `delta.tool_calls`. Norted emits actual `ToolCallDelta` events as frames arrive, carrying the backend index and optional ID/name headers plus incremental arguments. |
| JsonObject | `response_format.type=json_object` accepts a `schema` member. Norted supplies `{"type":"object"}` there. This avoids the empty-schema path, which does not activate a schema grammar in some specialized template parsers. |
| JsonSchema | The request parser consumes `response_format.json_schema.schema`. Norted preserves name, description, schema and strict in the existing wrapper. |
| Strict behavior | Upstream does not interpret the wrapper's `strict` flag separately; schema requests use generated grammar. Its [schema-to-grammar support is a subset](https://github.com/ggml-org/llama.cpp/blob/de8656bd94f1163188125542534e4bcbc9f9fb1f/grammars/README.md). Norted additionally validates the entire returned JSON value against the schema and fails closed on invalid output or incomplete termination. This is not a claim that every JSON Schema keyword is enforced during sampling. |
| Built-in Jinja | [server-context.cpp](https://github.com/ggml-org/llama.cpp/blob/de8656bd94f1163188125542534e4bcbc9f9fb1f/tools/server/server-context.cpp) initializes templates from model metadata and exposes template source/capabilities through `/props`. [Jinja capability probes](https://github.com/ggml-org/llama.cpp/blob/de8656bd94f1163188125542534e4bcbc9f9fb1f/common/jinja/caps.cpp) exercise template behavior. Norted supplies no replacement Grep template. |
| Qwen3.5 compatibility | `common/chat.cpp` detects the built-in XML tool syntax (`<tool_call>`, `<function=`, `<parameter=`), uses its specialized parser/grammar, renders tool-response messages and supports multiple calls. Norted contains no family-name conditional. The real-model evidence below verifies the actual embedded template. |

No upstream tool-request, tool-parser, Jinja or schema extension was necessary.
The **existing Norted stop overlay did need a two-classifier correction**: V1
forced OpenAI `stop` on exact-token termination even when decoded calls existed.
V2 retains upstream `tool_calls` for those turns in aggregate and streaming
responses. Sampling, vocabulary validation, speculation suppression, terminal
exclusion and token accounting are unchanged. The original overlay is untouched.
The new full-overlay SHA256 is
`f1a579cb1e2484bd8bf29f987a7fe9a79ddae015f04733521a445d776bdfa873`.
See [exact-stop ownership](stop-token-ids.md).

Malformed tool structures fail with adapter errors: missing/empty IDs or names,
non-function types, non-string or invalid/object-incompatible encoded arguments,
duplicate IDs, malformed indexes, conflicting stream headers, multiple choices,
or disagreement between calls and finish reason. Stream calls are checked again
at completion. ID/name fields follow Norted's optional-header contract;
arguments alone are appended. Streaming never synthesizes IDs or emits backend
JSON as text.

Structured output is validated in aggregate and streaming modes. Stream text
remains incremental; accumulated content is validated before `Completed`, and
invalid/incomplete content produces an error instead. Partial deltas are not a
structured-output success. Local stream validation has a 16 MiB content limit.
JSON Schema remote/file resolution is disabled in the validator dependency;
unresolvable references fail before generation. The existing typed
`llama.cpp.structured_output_schema` launch setting remains intact; request-owned
schemas use `response_format` and do not become launch settings.

Active tools plus structured response format are rejected as an unreviewed
combined mode. This does not block Retrieval: tool rounds and strict finalization
are separate requests on the same loaded runtime. History remains available
when finalization clears tools and sets choice none and parallel false.

## Real-model validation: 2026-09-09

Starting master: `753d0a1db06a5e68086834e627d0e2559fcbd40e`.
Implementation commit: `e58567b` on `feat/llama-retrieval-capabilities`.
[Machine-readable evidence](llama-retrieval-validation.json) records runtime
provenance, profile settings, responses, tool IDs/deltas, exact stops and actual
benchmark outcomes. Full local logs are retained at
`/srv/norted/scratch/llama-retrieval-validation/evidence/`.

The isolated validation instance used a side-by-side pack assembled from the
existing managed-recipe CMake build, rebuilt with the V2 overlay, then validated
and launched by Norted's runtime manager. This was not a fresh catalog download
or installer build. Reverse-apply verification confirms the tested source is
exactly the reviewed commit plus V2 overlay. Its runtime ID is:
`llama-cpp-b10786-linux-x86-64-cuda-managed-portable-cuda13-exact-stop-v2-3d7babe5ecd4f911`.

Hardware: RTX 5090, driver 595.84, CUDA toolkit 13.3. Both Q4_K_M artifacts came
from the existing `Norted/dist/grep-variants/gguf/{4b,9b}/` deployment directories.
Package integrity was checked by Norted; artifacts/manifests were not edited.
Both retain built-in template SHA256
`a4aee8afcf2e0711942cf848899be66016f8d14a889ff9ede07bca099c28f715`.
The observed tool, tool-history and parallel capabilities were true. Explicit
settings included Jinja, 16384 context, full GPU offload, BF16 KV cache, reasoning
off and temperature zero. A saved `reasoning_effort=none` override was removed
from the isolated copies because the typed schema rejected it; reasoning off
remained. User profiles were not edited.

For **both models**, aggregate and streaming manual conversations returned
parallel glob/grep calls, resent those calls and matching results, made a forced
read call, then completed a tools-cleared strict `retrieval::final_schema()`
request. Finalization returned only a schema-valid object identifying
`src/leases.rs`, lines 2–7. JsonObject and ordinary text passed in both modes.
These checks used Norted's public Chat API, exercising its neutral tool-call and
stream event boundary, not just direct llama-server requests.

| Live streaming observation | 4B | 9B |
| --- | ---: | ---: |
| First tool delta | 0.166 s | 0.179 s |
| Terminal ToolCalls event | 0.376 s | 0.453 s |
| Tool delta events | 12 | 12 |
| Calls in the turn | 2 | 2 |

Wire capture confirms mandatory `[248044,248046]` and matching history/result
IDs. Separate native probes forced 248044, 248046 and added ID 42: each stopped
after one sampled token, emitted no content and reported `stop_type=token_id`,
both aggregate and streaming. An independent public text-stop request also
passed. No text strings substituted for the artifact stop IDs.

Malformed-backend manual probes rejected invalid tool structures in both modes,
retained whitespace in a valid encoded argument string, and rejected schema
extra properties, prose and an integer below its minimum. No tests were added.

## Unchanged Retrieval results

Both real benchmark runs have Retrieval **state=complete, scored=4/4**, with no
missing-feature/unavailable reason. The category was not bypassed or rescored.

| Profile | Run ID | Retrieval | Tool calls | Serial rounds | Tool protocol failures |
| --- | --- | ---: | ---: | ---: | ---: |
| Grep 4B Q4_K_M | `31512119-0edb-444d-9c91-3caaadd52463` | **0/100** | 26 | 15 | 0 |
| Grep 9B Q4_K_M | `4196b6f3-fdf8-4d43-8e46-60a55e43aadb` | **0/100** | 23 | 14 | 0 |

All eight final responses had valid JSON/schema shape but invalid repository
ranges. Examples: 4B invented `contracts/Lease.sol:84–124`; 9B returned
`src/leases.rs:1–11` for a ten-line file. Other responses similarly invented paths
or exceeded EOF. The benchmark classifies these as `malformed_final_output`;
that classification includes repository validity, not only JSON syntax. The
adapter must not change those ranges or the score. The benchmark used strict
finalization on three tasks per model and accepted an early final attempt on
the remaining task, preserving its original protocol.

The **whole suite** status is `completed_unavailable` because other measurements
remain unverified (including native prefill timing at b10786 and configuration
identity). This is distinct from Retrieval, which completed and was scored.
The benchmark build did not embed `NORTED_SOURCE_REVISION`; the evidence records
the matching implementation commit separately. No NF4+adapter or scientific
Grep score is attributed to these GGUF runs.

Validation passed: `cargo fmt --all --check`, `cargo check --workspace`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`, and
`cargo test --workspace` (214 passed, 0 failed, 1 ignored). Existing ordinary
runtime/update, exact-stop, q27 and NInfer checks remain unchanged. No tests,
benchmark scoring/task changes, GGUF changes, GitHub Actions, push or merge were
introduced. The material remaining finding is the models' Retrieval range
quality; arbitrary newer llama.cpp revisions remain unreviewed for advertisement.
