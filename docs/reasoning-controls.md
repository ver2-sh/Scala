# Request reasoning controls

In Unsloth Studio, select **Connections → Scala (Custom) → Enable reasoning
controls → Reasoning parameter style: thinking (enabled / disabled)**.

Scala's `/v1/chat/completions` accepts these request-local controls:

```json
{"thinking":{"type":"enabled"}}
```

```json
{"thinking":{"type":"disabled"}}
```

The object must contain exactly `type`, with one of these two lowercase values.
Null, scalars, arrays, missing `type`, unknown keys and other values fail with
HTTP 400. No User-Agent detection or client-specific routing is involved.

ON uses the selected Model Profile's effective supported reasoning behavior.
For example, a profile effort of `xhigh` remains `xhigh`, and a profile effort of
`medium` remains `medium`. Scala does not select a global effort. A boolean-only
model receives a boolean control without an invented effort. If the effective
policy is `none`, explicit ON removes that disabling request default and leaves
the enabled effort to the native template; it does not guess another tier.

OFF explicitly disables reasoning and prevents an inherited effort from
re-enabling it. An omitted reasoning control preserves existing behavior.
These are ephemeral request overrides: they never modify Settings, Model
Profiles, runtime selection, launch arguments, process environment or artifacts.

## Precedence and conflicts

The precedence remains Runtime default → per-engine Settings → Model Profile →
load/invocation → request override, where the exact native contract supports
that override. Engine-owned process defaults remain process defaults.

`enable_thinking` and `reasoning_effort` remain accepted. An explicit supported
effort wins over inherited effort and mode defaults. `none` requests disable;
non-`none` efforts use native effort semantics. Explicit contradictory controls
fail before inheritance:

| Explicit controls | Result |
| --- | --- |
| thinking enabled + enable_thinking true | Accept if supported |
| thinking disabled + enable_thinking false | Accept if supported |
| thinking enabled + reasoning_effort medium | Use medium if verified |
| thinking disabled + reasoning_effort none | Disable if switchable |
| thinking disabled + reasoning_effort xhigh | HTTP 400 conflict |
| thinking enabled + reasoning_effort none | HTTP 400 conflict |
| thinking enabled + enable_thinking false | HTTP 400 conflict |

Public model discovery's `capabilities.thinking.switchable` and
`capabilities.thinking.effort_options` describe verified overrides, not defaults.
Missing capabilities grant no request controls; empty effort options grant no
effort tiers. `none` is a disable operation requiring switchability, rather than
an enabled effort tier. Unknown and unsupported models receive a clear HTTP 400
reasoning diagnostic. Scala never derives effort support from model names,
quantization, Norted lineage, arbitrary metadata, engine identity or launch help
alone. Existing requests without explicit reasoning controls continue to use
their existing resolved defaults.

## Native engine behavior and current evidence limits

| Engine | ON | OFF | Qualification |
| --- | --- | --- | --- |
| NInfer | Native `enable_thinking: true`; retain effective supported request effort | Native `enable_thinking: false`; omit inherited effort | Reviewed native request semantics and existing embedded-template content capability proof |
| q27 | Native `enable_thinking: true`; retain the process/template effort default | Native `enable_thinking: false`; preserve the process effort default | Reviewed native request semantics, explicitly enabled `q27.request_thinking`, existing runtime/model restrictions, and no arbitrary external template |
| llama.cpp | Currently HTTP 400 for unverified model/template request controls | Currently HTTP 400; never drop OFF silently | Existing Scala discovery does not establish model switchability or exact effort choices |

NInfer's `ninfer.thinking` remains a launch setting. `ninfer.preserve_thinking`
remains separate private history retention, and its reasoning budget remains a
launch default. Request OFF does not change any of these settings.

q27's `q27.thinking` and `q27.reasoning_effort` remain process settings. Scala
does not enable `q27.request_thinking` automatically. q27's existing
trained-template effort settings qualification uses `general.name`; existing
public discovery deliberately declines to turn that into name-independent
request effort grants. Consequently request effort tiers currently fail at
routing even when a process effort is configured. The adapter also rejects
unproved `minimal` → `low` and `high`/`max` → `xhigh` aliases.

For llama.cpp, the already pinned
[b10665 native request parser](https://github.com/ggml-org/llama.cpp/blob/ca3d5a3e10d53f7ea672cb9b6178faca3e2807bc/tools/server/server-common.cpp)
merges `chat_template_kwargs.enable_thinking` as a boolean and accepts
`reasoning_effort`, with `none` disabling reasoning. This proves a wire parser,
not that every template can disable reasoning or accepts the same efforts.
The pinned [template capability implementation](https://github.com/ggml-org/llama.cpp/blob/ca3d5a3e10d53f7ea672cb9b6178faca3e2807bc/common/jinja/caps.cpp)
reports whether an effort variable is used, but neither switchability nor an
effort vocabulary. Scala therefore cannot safely admit these request overrides
using its existing evidence. A verified model/template capability contract is
required before adding forwarding. Existing `llama.cpp.reasoning` and
`llama.cpp.reasoning_effort` launch settings remain supported through their
existing admission path. Explicit request efforts lacking model evidence also
fail rather than treating launch help as a model effort grant.

## Reasoning streams and thinking time

Ordinary Chat SSE and Responses SSE continue to redact native reasoning text.
Enabling thinking does **not** authorize reasoning-text disclosure.

For applications that explicitly want native reasoning text, Chat streaming
offers an independent, request-local opt-in:

```json
{
  "model": "your-profile",
  "messages": [{"role":"user","content":"Your prompt"}],
  "stream": true,
  "thinking": {"type":"enabled"},
  "include_reasoning": true
}
```

`include_reasoning: true` requires streaming. It exposes sensitive native text
using the established compatible `choices[0].delta.reasoning_content` format,
in native order before answer/tool deltas. No synthetic thoughts, placeholder
reasoning, private Unsloth events or made-up timing numbers are sent. The opt-in
currently carries NInfer's actual native reasoning deltas; llama.cpp and q27
do not currently supply separate reasoning events through Scala. Non-streaming
responses and the Responses API retain their existing redaction behavior.
This opt-in does not add support for replaying assistant `reasoning_content`
in incoming history; the existing message schema still applies.

NInfer's decoder records a monotonic **observed phase interval**, beginning at
the first nonempty native reasoning delta and ending at the first answer/tool
delta or an observed finish reason. It excludes prompt time and the remaining
answer stream. It cannot recover computation before the first reasoning delta,
or separate token production from transport buffering. It is an internal
observation, not a claim of exact engine compute time, and is not serialized as
a public duration. Missing reasoning produces no interval; interruption before
an observed phase end produces no interval. Cancellation drops the owned native
source as before. Native usage, tool deltas, errors and terminal checks remain
intact.

The inspected [Unsloth Chat adapter](https://github.com/unslothai/unsloth/blob/6c723f747799b56c8ecda16d0a93523eca3a2d61/studio/frontend/src/features/chat/api/chat-adapter.ts)
starts its reasoning timer on nonempty `delta.reasoning_content` and closes
that phase when answer text arrives. Its
[duration tracker](https://github.com/unslothai/unsloth/blob/6c723f747799b56c8ecda16d0a93523eca3a2d61/studio/frontend/src/features/chat/utils/reasoning-duration.ts)
measures observed stream time. Synthetic Scala tests verify that the real Chat
SSE serializer delivers those recognizable signals in order when opted in,
and delivers neither signal nor text without the opt-in.

The normal Unsloth thinking toggle alone does not send `include_reasoning`.
Without an explicit client facility to add that opt-in, Unsloth cannot obtain
its text-based timer through Scala's default redacted stream. Its alternative
`_reasoningDurationMs` event is private and is not implemented here. A local
Unsloth timer may include transport delay, and terminal-only/interrupted streams
do not prove a complete reasoning duration. End-to-end UI testing against an
installed Unsloth runtime has not been performed; no default-private numeric
thinking-time feature is claimed.

## Local validation

Use static workspace checks and synthetic contract tests only. Do not launch
models, run inference/benchmarks, alter production configuration or qualify
capabilities by model-name guesses. Focused tests cover parsing, conflicts,
default/request precedence, distinct effort vocabularies, boolean/unsupported
models, native admission, ordering/redaction, observed intervals, interruptions,
tool/usage/terminal events and dropping the source on cancellation.

The implementation was inspected against a clean `master` checkout at
`21bcda84100062c60bbefd8f88dd1317064d030e` on 2026-10-09. No runtime was launched
for this validation.

| Executed synthetic tests | Passed |
| --- | --- |
| `cargo test -p scala-api --lib --offline` | 35 |
| `cargo test -p scala-engine --lib reasoning --offline` | 4 |
| `cargo test -p scala-engine-ninfer --lib --offline` | 42 |
| `cargo test -p scala-engine-q27 --lib --offline` | 27 |
| `cargo test -p scala-engine-llama-cpp --lib --offline` | 46 |

Static checks use `cargo check --workspace --all-targets --offline`,
`cargo fmt --all -- --check`, `git diff --check`, and
`cargo clippy -p scala-api -p scala-engine -p scala-engine-ninfer
-p scala-engine-llama-cpp -p scala-engine-q27 --all-targets --offline -- -D warnings`.

## Changed files for audit

- `crates/scala-api/src/chat.rs`: strict controls, explicit text opt-in, SSE
  serialization and public wire contracts.
- `crates/scala-api/src/error.rs`: client-safe reasoning diagnostics while
  retaining general backend error sanitization.
- `crates/scala-api/src/responses.rs` and `crates/scala-api/src/completions.rs`:
  preserve redaction and existing streaming contracts for the new private events.
- `crates/scala-engine/src/lib.rs`: typed reasoning validation/errors and private
  reasoning stream events; distinct synthetic capability vocabularies.
- `crates/scala-engine/src/manager.rs`: actual serving-tuple qualification,
  request precedence, source-preserving defaults and stream activity.
- `crates/scala-engine/src/benchmark/runner.rs`: handle the additional event
  variants without collecting private reasoning; no benchmark was run.
- `crates/scala-engine-ninfer/src/protocol.rs` and
  `crates/scala-engine-ninfer/src/lib.rs`: qualified native request translation,
  conflicts, ordered reasoning events and observed phase intervals.
- `crates/scala-engine-q27/src/lib.rs`: native gating, explicit conflicts,
  rejection of unproved aliases and preservation tests.
- `crates/scala-engine-llama-cpp/src/lib.rs` and
  `crates/scala-engine-llama-cpp/src/chat.rs`: reject unverified boolean
  requests, retain launch policies, handle additional event variants and test
  admission without native inference.
- `README.md` and this document: configuration, current evidence limits,
  privacy/timer behavior and validation record.
