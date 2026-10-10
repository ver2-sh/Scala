# Chat Completions message names

For installed Unsloth workflow construction, native CPU renderer evidence and
the exact remaining upstream boundary, see the
[Unsloth / NInfer interoperability audit](unsloth-ninfer-interop.md).

Scala previously allowed `messages[].name` only when omitted or null, before
parsing the role. That rejected named messages in Unsloth Studio tool loops and
replayed history with HTTP 400, including Web Search and file-editing workflows.
The original failures do not establish which role was named in every request.

The [official OpenAI Chat Completions contract](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)
defines optional participant names on `system`, `developer`, `user`, and
`assistant`. These distinguish participants of the same role. Its current
`tool` schema instead associates a result through `tool_call_id`; it does not
define a tool-result `name` field. Scala accepts that common compatible-client
extension with the validation below.

## Public parsing and identity

Omitted and null names remain equivalent. Scala validates non-null names as
strings matching `[A-Za-z0-9_-]{1,64}`. Empty names, whitespace, non-ASCII names,
control characters, punctuation outside `_` and `-`, oversized strings, arrays,
objects, numbers and booleans are rejected. This is Scala's bounded identifier
contract; the current OpenAI reference describes participant names as strings
without specifying that grammar.

Participant names are retained in the engine-neutral `InferenceMessage`, with
backward-compatible serde defaults for unnamed history. A user named `system`
remains a user. Names never authorize requests, select an engine or profile,
grant capabilities, register callable tools, execute code, or supply trusted
instructions. Existing API authentication and role validation still apply.

A named tool result still requires its non-empty `tool_call_id`. An earlier
assistant `tool_calls` entry with that ID must establish the same function name.
Lookup follows IDs, including parallel calls and consecutive results; result
order is never changed. A conflicting name, unmatched ID, or a call appearing
only later in the history is rejected. Once validated, the redundant tool name
is omitted internally. IDs, assistant call names and argument strings, result
content, and message order remain intact. Historical calls need not be present
in the current request's callable tool definitions. Unnamed tool results retain
their existing parsing behavior.

Other unsupported roles and unknown fields retain their existing errors.
Responses parsing is unchanged; its message constructors initialize the new
participant-name field to `None`.

## Native runtime evidence and limits

This change follows source inspection, without running native inference or
template probes against a loaded model.

| Engine/path | Native evidence | Scala behavior |
| --- | --- | --- |
| llama.cpp Chat | `common_chat_msgs_parse_oaicompat` reads `name` into `common_chat_msg.tool_name` for every role; `common_chat_msg::to_json` returns it as `name` for Jinja. Inspected installed b11430 `common/chat.cpp`, lines 236–237 and 456–457. | Forward participant `name` with unchanged content and calls. When instructions are named, preserve individual messages and positions instead of combining them; the existing developer-to-system role translation remains. Unnamed instruction translation is unchanged. |
| NInfer Chat | Reviewed [`d4929686` request parser](https://github.com/Neroued/ninfer/blob/d49296868dcc17bd478ec185f0d3a801bcc0bf56/src/serve/openai_chat_request.cpp), `validate_message_name`, explicitly rejects non-empty names outside `tool`; tool names are ignored compatibility hints. Confirmed against installed source. | Validated redundant tool names are omitted. Participant names fail with a typed capability error before native request delivery; no unsupported name field is forwarded. |
| q27 native Chat | Reviewed [`v0.10.0 api_common.h`](https://github.com/signalnine/q27/blob/4770e053656af9aababdc49c81f280ad21b74986/src/api_common.h) and [`v0.14.3 api_common.h`](https://github.com/signalnine/q27/blob/a4d5fc4be1231214e25c578eda7ab659a55689e7/src/api_common.h): `Msg` and `openai_msgs` retain role/content/reasoning, not participant names. Their fetched SHA-256 values match the existing [q27 audit](q27-grep-contract.md). | Validated redundant tool names are omitted. Named participants fail before native request delivery, including the unverified external native-chat path. |
| q27 explicitly configured external template | Scala's existing local Jinja renderer receives message objects before submitting the rendered raw prompt. | Retain participant `name` in template context. No `name` field is sent to the native raw-completion route. Existing external-template tool-calling restrictions remain. |

Unsupported participant identity returns HTTP 400 with
`code: unsupported_message_name` and `param: messages`, through both streaming
and ordinary request error handling. The client-safe error contains no private
runtime paths, endpoints, or client name values.

Passing names to llama.cpp Jinja or a configured q27 external template makes
the metadata available; actual speaker rendering depends on the template using
`message.name`. Templates that ignore names, including legacy llama.cpp built-in
rendering, cannot be assumed to distinguish participants. Scala does not patch
templates or synthesize speaker labels inside message content.

Consequently, named user/assistant Code Mode history is representable through
llama.cpp's name-aware Jinja templates and name-aware q27 external templates.
It remains unsupported on the reviewed NInfer and q27 native-chat paths. Those
runtimes need native participant-identity support and a new source audit for
full compatibility. q27 external-template mode cannot be used to bypass its
existing native tool-calling requirement. q27 native tool rendering also retains
its existing positional-result behavior; this change preserves transport IDs
and ordering but does not change the native renderer.

## Offline regression evidence

The shared synthetic fixture at
`crates/scala-engine/testdata/named-chat-messages.json` contains a fresh ordinary
conversation, Web Search and edit-file continuations, named Code Mode turns,
multiple tool calls and consecutive results, and a later user turn replaying
the existing history. Public parser tests compare each conversation prefix to
explicit normalized messages. Native serializer tests consume those same
normalized messages and compare complete native message arrays, or assert the
verified capability limitation. Both streaming flags are exercised.

Additional tests cover canonical roles, omitted/null names, old serialized
history, malformed names, missing tool IDs, name/ID conflicts, out-of-order
results, unsupported roles and unrelated fields, instruction identity, template
name access, and sanitized capability-error mapping. Existing reasoning,
streaming/cancellation, settings and profile contracts remain unchanged.

Validation on 2026-10-09:

- `cargo test --offline -p scala-api -p scala-engine-llama-cpp -p scala-engine-ninfer -p scala-engine-q27`: 162 tests passed.
- `cargo test --offline -p scala-engine named_message`: two tests passed.
- After adding unnamed parallel-history coverage, `cargo test --offline -p scala-api -p scala-engine-llama-cpp -p scala-engine-ninfer -p scala-engine-q27 -p scala-engine named_`: nine focused tests passed.
- `cargo check --workspace --all-targets --offline`: passed.
- `cargo clippy --workspace --all-targets --all-features --offline -- -D warnings`: passed.
- `cargo fmt --all --check` and `git diff --check`: passed.

No real inference, benchmarks, native model loads, service changes, persistent
settings changes, or upstream source modifications were used. Changes remain
local; no push, merge, tag, release, or deployment was performed.

## Files changed

- `crates/scala-api/src/chat.rs`: role-aware name validation, tool-name matching and public request regression tests.
- `crates/scala-api/src/error.rs`: specific sanitized unsupported-name error and test.
- `crates/scala-api/src/responses.rs`: initialize the added optional metadata field without changing Responses behavior.
- `crates/scala-engine/src/lib.rs`: optional participant identity, typed capability error and serde compatibility test.
- `crates/scala-engine/src/manager.rs`: preserve the typed error through runtime mapping and test it.
- `crates/scala-engine-llama-cpp/src/lib.rs`: retain participant names and individual named instructions; native serialization tests.
- `crates/scala-engine-ninfer/src/lib.rs`: reject unsupported participant names during request admission.
- `crates/scala-engine-ninfer/src/protocol.rs`: enforce the name limitation before native serialization; history tests.
- `crates/scala-engine-q27/src/lib.rs`: reject unsupported names on native chat, retain names in external template context; serialization/rendering tests.
- `crates/scala-engine/testdata/named-chat-messages.json`: shared synthetic public, normalized and native expected message shapes.
- `README.md`: link to the name contract and its native limits.
- `docs/chat-message-names.md`: compatibility contract, source evidence, limitations and validation record.
