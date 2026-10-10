# Unsloth Studio / Scala / NInfer interoperability audit

Audit date: 2026-10-09. Scala's local and remote master both resolved to
`ea0493970974abf7c60bb0455209130ef025f036`. Work is on local branch
`fix/unsloth-native-chat-contract`; no shared history was rewritten.

**Named-participant Code Mode remains blocked.** There is no semantically correct
Scala adapter translation for the installed NInfer contract. This correction
adds source diagnostics and synthetic regression evidence, not a name-stripping
fallback. No production parser, runtime qualification, settings, profile,
reasoning, launch, streaming or cancellation behavior changes.

## Installed Unsloth message contract

Inspected the installed `studio/backend` from Unsloth `2026.10.3`, including all
five requested inference files, the external-provider transport/builder,
`models/inference.py`, `routes/inference.py`, and the shipped frontend bundle.
No saved chats, prompts, credentials, tool output or service logs were read.

| Workflow/path | Names constructed or retained | Meaning |
| --- | --- | --- |
| Studio Web Search tool loop | `role=tool, name=web_search`, with the executed call's ID. Budget-exhausted and denied results also carry the function name. Newly generated assistant call turns have no participant name. | Function identity, redundant at Scala's boundary only after matching the earlier assistant call by ID and checking its function name. |
| Studio Code Mode tool loop | Tool results use the actual function name, e.g. `python`, `terminal`, `edit_file`, or a selected MCP function. The loop does not create fixed `coder` / `code_helper` identities. | Function identity, subject to the same bounded normalization. The fixture's participant names are synthetic examples, not observed Studio constants. |
| Ordinary named history in either workflow | `named_turn` copies any truthy source name; `format_chat_prompt` and `alternating_turns` use it while rebuilding user/assistant text turns. The initial tool-loop conversation copies incoming dictionaries. | Caller-supplied participant identity. Neither a single identity, a role-like spelling nor the workflow establishes redundancy. Distinct same-role participants must remain distinct. |
| Resumed partial assistant turn | `append_assistant_turn` merges the generated continuation over the old message and retains old keys that the generated message did not repeat, including `name`. | Identity of the same resumed turn. A fresh generated assistant turn is ordinarily unnamed. |
| Installed external-provider route | `_build_external_messages` currently copies `msg.name` only for `role=tool`, in all its string/list branches. It omits names on ordinary participants, including assistant call-only turns. `ExternalProviderClient.stream_chat_completion` subsequently forwards the built message dictionaries. | An upstream identity-loss defect when supplied history names are meaningful. Successful external requests after this omission do not prove named-participant compatibility. |

The frontend's inspected normal user/assistant constructors do not assign fixed
participant names; tool history serialization mirrors the function name onto
the result. Consequently, the features “Web Search” and “Code Mode” do not alone
determine whether a request has named ordinary participants. The installed
internal reconstruction path retains supplied names; the installed external
builder loses them. These are different paths. The prior Scala error proves a
named participant reached Scala, but does not identify its spelling or prove
which Studio route emitted it. This audit uses synthetic requests instead of
claiming to have captured that private request.

Unsloth names are not interchangeable:

- Its request validator can use a tool name to resolve a missing result ID
  before forwarding history. That name is not redundant before resolution.
- Tool names also guide MCP image/result handling. Scala must not retroactively
  change those client-side decisions.
- At Scala's boundary a named result must have an ID matching an earlier call
  with the same function name. Only then is the result's name redundant; call
  ID, function name, argument string, result content and order remain intact.
- Participant names describe speakers. Roles determine instruction priority.
  A user named `system`, `web_search` or `edit_file` remains a user, cannot grant
  permissions and cannot select a Model Profile or register a callable tool.

No nonempty ordinary participant name was proved safe to omit. Omitted/null
names retain their existing equivalence. A function name matching a result ID
does not establish a similar equivalence for user/assistant names.

## Native NInfer capability evidence

Installed source manifest revision:
`d49296868dcc17bd478ec185f0d3a801bcc0bf56`. The read-only runner verifies all
97 `REVIEWED_SOURCE_BLOBS` against that installed source before compiling its
CPU harness. Representative native owners:

| Native source owner | Git blob |
| --- | --- |
| `src/serve/openai_chat_request.cpp` | `2a7611c1e7178925d714fcadf591c9067caaef29` |
| `src/serve/request.h` | `cf87d3621f573876cd47610ee4aceb8645785d0b` |
| `include/ninfer/types.h` | `ac608e2ac6469929b16963f113a0cbe047d408a5` |
| `src/serve/translate.cpp` | `c2effe323b25b5c1f8538ad43f4bc744984fcddc` |
| `src/targets/qwen3_6/impl/frontend/chat_template.h` | `a3be5d9881ca5820a5dabfc8cbf490a3173b0698` |
| `src/targets/qwen3_6/impl/frontend/chat_template.cpp` | `9fba17ab444808b45c2900fe9aeb7cda3b43fe16` |
| `src/targets/qwen3_6/impl/frontend/frontend.cpp` | `75c7138318a4571006b474ab3b189b0a189a0d16` |

`validate_message_name` rejects nonempty names outside `role=tool`, with native
code `message_name_not_supported`. Tool-result names are ignored compatibility
hints. `ChatTurn`, public `ninfer::ChatMessage` and frontend `ChatMessage` have
roles, content, reasoning, calls and result IDs, but no participant-name field.
`to_prompt_input` and `convert_messages` consequently have no identity to copy.
The compiled renderer selects role-based speaker headers and never reads
participant identity. Scala's typed `unsupported_message_name` error is an
accurate capability rejection, not a public-parser failure.

`CompiledChatTemplate::resolve` accepts two exact template digests:

- Thinking toggle: `e84f32a23fdda27689f868aa4a1a5621f41133e51a48d7f3efcbea2839574259`.
- Reasoning effort: `c3cf9e34abf4f9e36c2d72165aa9c132d3e2a725b6c2586aaa3a8af9d7a81041`.

The inspected Swift 1.5 HF source template at revision
`bc7a1e10b689648585a3ef41494c8d84cf77271a` matches the second digest. It also
does not render participant names. This is template-source evidence, not a
production Model Profile load or a claim about an uninspected artifact. The
limitation follows native/template semantics, not model name, quantization,
producer provenance or Norted packaging.

There is no supported alternate identity mechanism in this installed contract:

- `chat_template_kwargs` admits only `enable_thinking` and `preserve_thinking`
  non-null options. Unknown identity options are rejected. Scala has no native
  external-template setting for this runtime.
- Responses and Anthropic routes share the same identity-free internal types;
  `tool_result_name` in Responses asserts function identity, not participant
  identity. Responses call-graph normalization can reorder results into call
  order and is not a roles/order-preserving Chat replacement.
- A raw prompt API cannot establish identity equivalence without an accepted
  model-facing representation. No speaker labels, delimiters or instructions
  were invented, and no raw-prompt bypass was implemented.
- The [upstream Chat parser inspected during this audit](https://raw.githubusercontent.com/Neroued/ninfer/master/src/serve/openai_chat_request.cpp)
  also retains the participant-name rejection. This does not qualify upstream
  master or admit its v3 artifact/runtime contract; the reviewed v2 pin remains.

An additional native limit is observable without inference: Chat preserves
`tool_call_id` through translation and frontend conversion, but both compiled
templates render tool results positionally, without IDs. Swapping only the
result IDs changes the native conversation while leaving the rendered prompt
identical. The ordinary Studio loop emits results in assistant-call order and
that ordered path is covered. Arbitrarily permuted OpenAI results are not proved
semantically equivalent. No Scala reorder or normalization was introduced to
hide this native limitation.

## Smallest correct upstream boundary

For meaningful participant identities, NInfer needs one complete semantic change:

1. Retain a validated optional participant name in `ChatTurn`, public
   `ChatMessage` and frontend `ChatMessage`; copy it in the parser,
   `to_prompt_input` and `convert_messages`, retaining canonical roles and order.
2. Render it through an upstream-supported, model/template-defined identity
   representation. Merely removing `validate_message_name` or adding a field is
   insufficient: both current templates omit identities. The encoding and its
   semantics must be defined upstream, not guessed in Scala.
3. Expose/qualify participant-name support for the exact runtime and selected
   template. Keep rejection for templates without support. Scala can then
   forward the original name only after a new source-owner/capability review.

This requires a new upstream native/template capability contract; no installed
release, binary, artifact or source was patched, and no custom fork was created.
Any general solution for arbitrarily permuted parallel results must also give
the renderer an unambiguous call/result association without changing requested
ordering. Current positional templates do not provide that contract.

On Unsloth's side, the smallest identity-preservation correction is to retain
ordinary `msg.name` in every `_build_external_messages` output branch, including
assistant call-only messages, as its internal `named_turn` paths already do.
That fixes identity loss; it does **not** make the current NInfer support names.
Unsloth could omit names only when the producer establishes that no participant
distinction was intended. No such assertion exists here. Tool-result names can
be omitted after ID resolution and matching, never before their client-side
relationship/MCP uses. Installed Unsloth was not modified.

## Regression evidence and reproduction

The existing shared fixture now also includes distinct named users, distinct
named assistants and mixed named/unnamed later replay. Edit-file arguments use
synthetic `path` / `edits` / `old_string` / `new_string` structures. Existing
parser, llama.cpp, NInfer and q27 tests consume the expanded fixture.

New tests compose real public parsing, normalized messages, Model Profile ID
conversion and synthetic profile binding lookup, actual NInfer adapter admission,
and the actual private NInfer request serializer. A separate RuntimeManager
fixture verifies both streaming and ordinary profile routing deliver unchanged
history to the selected adapter. It stops at dispatch, without launching a
process or contacting an inference endpoint. These stages share the same
explicit expected normalized messages; no live service or production profile
is required.

The optional CPU harness then executes the installed native parser,
`resolve_prompt_semantics`, `to_prompt_input`, exact extracted native
`convert_messages`, and compiled renderer for both accepted template sources.
Extraction isolates message conversion from tokenizer/media/Engine code;
message conversion itself is copied verbatim. NInfer's own template-fixture
newline handling is used. No Engine, tokenizer, GPU library, model weights,
network transport or inference execution is linked or started.

Coverage includes Web Search and edit-file continuation; named Code Mode;
parallel calls/results; later replay; mixed history; distinct same-role
participants; validated redundant tool names; meaningful names and role-like
spellings; missing request/tool capability proof; unsupported identity kwargs;
unnamed clients; cross-engine preservation/rejection; and the positional native
tool-relationship limitation. Assertions check native conversation fields and
synthetic content ordering in the rendered prompt, not merely parser acceptance.

Run from the Scala repository, supplying explicit installed source/header roots:

```sh
python3 scripts/test-unsloth-native-contract.py \
  --ninfer-source /path/to/reviewed/ninfer/source \
  --unsloth-backend /path/to/site-packages/studio/backend \
  --native-include /path/to/cuda/include
```

The runner AST-loads only installed Unsloth function bodies with synthetic
inputs. Image promotion is intercepted after message construction, so its
external-builder audit covers text and text-part history only. It reports source
hashes and structural findings. C++ products are created and removed in a
temporary directory. Normal `cargo test` runs the Rust stages; this runner is
required to execute the native CPU stage. Its native bridge environment variables
are test-only and are absent from production code.

Installed Unsloth SHA-256 evidence:

| Relative to `studio/backend` | SHA-256 |
| --- | --- |
| `core/inference/message_content.py` | `ab652333f6f6a4156067533cefaea5400fb51374f777e7e9aedbb25c860e3c4b` |
| `core/inference/inference.py` | `129eec3e3ab73d13ba7f04833ca491c3acc02176fa00d0775b8d605f5c52fc63` |
| `core/inference/chat_template_helpers.py` | `ec5cd5543054c537cfeaee3c1a26fdc2ff88ee8e72ee5e86ef497958e4ca1244` |
| `core/inference/tool_loop_controller.py` | `ed871e5c55f8cc02e8d4be03f9dc8d5095e6da01498a04f18c3602e4c09526f7` |
| `core/inference/studio_tool_loop.py` | `9c3d100de4112e4b789870122172493df947fafe1cd4c108869075873de94a79` |
| `core/inference/external_provider.py` | `73dfc3561f710d2de89e7deab3065b88399a8a31e125eafd21828504f6073ae2` |
| `routes/inference.py` | `f72902b3b925280838f638f92403d27cb0e18c2786956463d474420cd95f70aa` |
| `models/inference.py` | `c5c1f282241568a8ef74a9f0ccfd7786cecbc305a49f9c9b5731e627d5cb4447` |

Validation completed on this branch:

- `cargo fmt --all -- --check`: passed.
- `cargo check --workspace --all-targets --offline`: passed.
- Four API/adapter crate suites: 175 tests passed. This includes the existing
  private NInfer protocol suite compiled into API contract tests as well as its
  original adapter location; these are not all new distinct tests.
- `cargo test --offline -p scala-engine`: 89 passed; the existing manual file
  throughput benchmark remains ignored. Profile-routing coverage passed.
- Focused `named_` tests across those five crates: 10 passed.
- Explicit source runner: three public/native CPU contract tests passed for
  both accepted templates, plus the installed Unsloth structural audit.
- `cargo clippy --workspace --all-targets --all-features --offline -- -D warnings`:
  passed.
- `git diff --check`: passed.

No real inference, production process/model load, service or settings change,
runtime patch, fork, push, merge, tag, release or deployment was performed.

## Cross-engine readiness

| Path | Result |
| --- | --- |
| NInfer Web Search / edit-file / unnamed Code Mode history | Synthetic ordered tool continuation reaches the native renderer. Ready for a scoped end-to-end test with an already qualified profile and no meaningful participant names, subject to its existing settings/tool-choice controls. |
| NInfer named-participant Code Mode or named replay in either workflow | Not ready. Correctly rejected. Requires the upstream native/template capability above and preservation through Unsloth's external builder. |
| llama.cpp | Existing forwarding preserved. Name metadata reaches Jinja; actual identity support requires a name-aware template. No blanket compatibility claim for templates that ignore names. |
| q27 native Chat | Existing participant-name rejection preserved. Validated redundant tool names retain the existing transport behavior and positional-renderer limitation. |
| q27 explicit external template | Names remain in local template context, but existing tool-calling restrictions remain. This path does not establish Code Mode tool interoperability. |

No full Code Mode compatibility or completed live end-to-end inference test is
claimed. Remaining blockers are demonstrated at the architectural boundary.
