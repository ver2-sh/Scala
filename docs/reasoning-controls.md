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
Missing capabilities grant no generic On/Off controls; empty effort options provide
no public effort grants. Discovery is optional and can be incomplete. The manager
uses adapter-owned admission before and after inheritance: NInfer retains its
verified capability check, while llama.cpp and q27 validate explicit efforts
through their existing native schema/execution contracts. Conflicts are checked
before inheritance. Generic On/Off still requires verified switchability, even
when combined with an effort. An effort-only `none` retains native disabling
semantics without synthesizing a separate boolean request.

Unknown and unsupported native controls fail before inference. No additional
aliases or effort translations are granted by incomplete discovery. Scala never
derives new generic thinking capabilities from launch arguments or engine names.
Existing requests without explicit reasoning controls continue to use
their existing resolved defaults.

## Native engine behavior and current evidence limits

| Engine | ON | OFF | Qualification |
| --- | --- | --- | --- |
| NInfer | Native `enable_thinking: true`; retain effective supported request effort | Native `enable_thinking: false`; omit inherited effort | Reviewed native request semantics and existing embedded-template content capability proof |
| q27 | Native `enable_thinking: true`; retain the process/template effort default | Native `enable_thinking: false`; preserve the process effort default | Reviewed native request semantics, explicitly enabled `q27.request_thinking`, existing runtime/model restrictions, and no arbitrary external template |
| llama.cpp | HTTP 400 for unverified boolean controls | HTTP 400 for unverified boolean controls | Explicit native efforts retain exact runtime-schema admission; launch arguments grant no boolean switchability |

NInfer's `ninfer.thinking` remains a launch setting. `ninfer.preserve_thinking`
remains separate private history retention, and its reasoning budget remains a
launch default. Request OFF does not change any of these settings.

q27's `q27.thinking` and `q27.reasoning_effort` remain process settings. Scala
does not enable `q27.request_thinking` automatically. q27's existing
trained-template effort settings qualification uses `general.name`; existing
public discovery deliberately declines to turn that into name-independent
request effort grants. Explicit requests retain qualified native `low`, `medium`
and `xhigh` semantics,
including overriding a configured process effort. These enabled efforts do not
require the boolean request toggle. `none` requires the qualified native disable
contract and explicitly enabled `q27.request_thinking`; it leaves process defaults
untouched. An unsupported native model effort schema rejects enabled tiers.
The adapter also rejects unproved `minimal` → `low` and `high`/`max` → `xhigh` aliases.

For llama.cpp, the already pinned
[b10665 native request parser](https://github.com/ggml-org/llama.cpp/blob/ca3d5a3e10d53f7ea672cb9b6178faca3e2807bc/tools/server/server-common.cpp)
merges `chat_template_kwargs.enable_thinking` as a boolean and accepts
`reasoning_effort`, with `none` disabling reasoning. This proves a wire parser,
not that every template can disable reasoning or accepts the same efforts.
The pinned [template capability implementation](https://github.com/ggml-org/llama.cpp/blob/ca3d5a3e10d53f7ea672cb9b6178faca3e2807bc/common/jinja/caps.cpp)
reports whether an effort variable is used, but neither switchability nor an
effort vocabulary. Scala therefore keeps generic boolean controls closed.
Existing explicit
`reasoning_effort` requests remain admitted by the selected runtime's qualified
schema: the setting must be supported and the exact choice must be present. This
restores the existing native request path independently of optional public model
discovery. Unadvertised choices remain errors, including `none` when the native
schema does not advertise it. Existing `llama.cpp.reasoning` and
`llama.cpp.reasoning_effort` launch settings retain their admission path.

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
its text-based timer through Scala's default redacted stream.

A duration-only Scala event is not compatible with the inspected normal Custom
Connection path. No default-private numeric thinking-time feature is implemented
or claimed.

On 2026-10-09 the installed Unsloth 2026.10.3 package was inspected read-only at
`/srv/norted/cache/uv/archive-v0/J-IFFZ7wDrINOGGx/studio`. Its
`backend/core/inference/external_provider.py` relays Custom Chat SSE through
`sanitize_provider_sse_line` (lines 2073–2076). The installed
`backend/core/inference/sse_control_frames.py` is byte-identical to the
[upstream sanitizer at 6c723f7](https://github.com/unslothai/unsloth/blob/6c723f747799b56c8ecda16d0a93523eca3a2d61/studio/backend/core/inference/sse_control_frames.py):
SHA-256 `9016bfa9c261d1fc2edd2c85317c79cbd8c4f604e8e68d72d5cf9b65dedada36`.
The relay explicitly strips top-level `_reasoningDurationMs` and strips/drops
`type: reasoning_summary`. A synthetic execution of the installed sanitizer
confirmed that both pure duration envelopes are dropped, and a duration attached
to an ordinary `choices` chunk is removed while the chunk survives.

The installed frontend `dist/assets/chat-Owi-soaD.js` (SHA-256
`e4ade65952bf49e7b4d8a6eaf2c0423bcb3b216607062f6ffd68ac2a870f08f1`)
consumes `_reasoningDurationMs` via `recordServerDuration` and maps
`reasoning_summary.duration_ms` to that private key. Its tracker assigns a server
duration only when a visible reasoning group has already started. The reviewed
upstream [tracker](https://github.com/unslothai/unsloth/blob/6c723f747799b56c8ecda16d0a93523eca3a2d61/studio/frontend/src/features/chat/utils/reasoning-duration.ts)
and [adapter](https://github.com/unslothai/unsloth/blob/6c723f747799b56c8ecda16d0a93523eca3a2d61/studio/frontend/src/features/chat/api/chat-adapter.ts)
confirm this behavior. Thus merely allowing the numeric key through the relay
would still not establish a timer for a text-free stream.

Additional integration would require an external-provider duration contract
accepted by Unsloth's relay plus frontend support for duration observations
without reasoning text/groups. Alternatively, a client facility could explicitly
opt into real reasoning text with `include_reasoning`, accepting that disclosure.
Neither integration is changed here. Installed source, production services and
client settings are unchanged, and no end-to-end UI test was performed. Scala
continues to keep observed intervals private and never invents timing or thoughts.

## Local validation

Use static workspace checks and synthetic contract tests only. Do not launch
models, run inference/benchmarks, alter production configuration or qualify
capabilities by model-name guesses. Focused tests cover parsing, conflicts,
default/request precedence, distinct effort vocabularies, boolean/unsupported
models, native admission, ordering/redaction, observed intervals, interruptions,
tool/usage/terminal events and dropping the source on cancellation.

This correction starts from `dca2af4708e66465f80191585300ab15c94d2d47` on the
local `fix/reasoning-native-admission` branch. Shared history is unchanged; no
push, runtime launch, real inference, benchmark or training is authorized.

Focused validation includes `cargo test -p scala-engine --lib reasoning --offline`
and the full synthetic library suites for `scala-api`, `scala-engine-ninfer`,
`scala-engine-llama-cpp` and `scala-engine-q27`, plus Scala Core's settings
precedence test. The native regression tests exercise admission and request
serialization with missing/empty public discovery, exact supported choices,
unsupported schemas and q27's disable gate. The manager regression proves the
native adapter gets effort admission while generic On/Off remains closed.

All checks below passed locally on 2026-10-09:

| Offline library test suite/filter | Passed |
| --- | --- |
| `scala-api` | 35 |
| `scala-engine`, filter `reasoning` | 5 |
| `scala-engine-ninfer` | 42 |
| `scala-engine-llama-cpp` | 47 |
| `scala-engine-q27` | 28 |
| `scala-core`, filter `resolution_has_exact_three_layer_precedence` | 1 |

The q27 native-effort regression was rerun after adding checks for disabled
request-thinking and unsupported model schemas; it also passed.

Static checks: `cargo fmt --all -- --check`,
`cargo check --workspace --all-targets --offline`, `git diff --check`, and
`cargo clippy -p scala-api -p scala-engine -p scala-engine-ninfer
-p scala-engine-llama-cpp -p scala-engine-q27 --all-targets --offline -- -D warnings`.

Changed correction files: engine `lib.rs` and `manager.rs`; the engine's
`decision.rs` synthetic adapter fixture; llama.cpp and q27 adapter `lib.rs`;
`README.md` and this document. Chat/Responses SSE and NInfer protocol code are
unchanged, retaining privacy, ordering, usage, cancellation and error behavior.
