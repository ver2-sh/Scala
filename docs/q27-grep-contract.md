# q27 Grep contract review

Review basis: Norted-Server master
`094b2538ad9d16d8de3a160aefd289bc5d42f171`, with the existing q27 source contract
unchanged. The reviewed upstream commit is
[`4770e053656af9aababdc49c81f280ad21b74986`](https://github.com/signalnine/q27/tree/4770e053656af9aababdc49c81f280ad21b74986),
tree `ff712f78fd17b5fe12149679114b6def003f16a6`.
Norted-Server's existing provider checks commit/tree, source owners, build
provenance and installed executable identity. No newer/custom/external binary
inherits proof from its version or model filename.

## Constrained finalization: blocked upstream

The exact source contains **no `response_format`, JSON Schema or user grammar
request route**. Its constraint facility has a different contract:

- [`src/toolconstrain.h`](https://github.com/signalnine/q27/blob/4770e053656af9aababdc49c81f280ad21b74986/src/toolconstrain.h)
  `scan_round()` returns without engaging when registered tool names are empty.
  It engages on a tool-call opener (or the XML bare function opener), not an
  arbitrary JSON object. `drop()` removes the mask when leaving that state.
- [`src/toolgram.h`](https://github.com/signalnine/q27/blob/4770e053656af9aababdc49c81f280ad21b74986/src/toolgram.h)
  defines hardcoded JSON tool-call bodies and XML function/parameter bodies.
  JSON mode constrains a tool-name/arguments envelope, not arbitrary response
  schema; XML mode adds parameter-name/required-key restrictions. Neither can
  express an assistant ranges object on a no-tools turn through the server API.
- [`src/server.cu`](https://github.com/signalnine/q27/blob/4770e053656af9aababdc49c81f280ad21b74986/src/server.cu)
  enables `tc` only with `--constrain-tools`, non-forced choice and greedy sampling.
  OpenAI, background and other serving paths feed registered tool names/keys into
  that facility. The sampled/forced limitations are explicit in upstream comments.
  There is no response grammar translation to adapt.

Canonical Grep removes every callable definition after four executed rounds.
Consequently the native tool constraint cannot engage on its fifth ranges turn.
Passing an ignored `response_format`, asking for JSON in prose, retaining a dummy
function, or borrowing an internal token mask would not implement the reviewed
runtime's serving contract. None is done here.

`StructuredOutput` stays absent from both q27 adapter and serving features;
`JsonObject` and `JsonSchema` remain rejected, now with the precise reason. q27
Retrieval is **unavailable**, not zero. Normal text and native tools remain on
their existing paths. Enabling q27 Retrieval requires an upstream serving facility
that constrains no-tools ranges, followed by a new immutable runtime review and
adapter translation. No q27 source/runtime or Norted model is modified.

## Qwen3.8 native rendering evidence and limits

The audit follows
[`src/api_common.h`](https://github.com/signalnine/q27/blob/4770e053656af9aababdc49c81f280ad21b74986/src/api_common.h)
through `openai_msgs`, `tool_call_text_dialect`, `tool_response_text`,
`openai_tools_decl` and `chatml_prompt`:

| Grep conversation shape | Exact reviewed behavior |
|---|---|
| System + query | System content retained; trained tools preamble precedes it. |
| Assistant parallel calls | All calls render as Qwen XML function/parameter blocks, in call-array order. |
| Tool results | JSON text is trimmed and wrapped; consecutive results share one user turn, in positional order. Transport IDs are not rendered. |
| Repeated rounds | Message loop preserves assistant/tool history; existing goldens exercise two consecutive rounds. |
| No-tools finalization user message | Tools preamble depends on current definitions, while historical calls render independently. A following ordinary user turn remains ordinary user content. |
| Assistant ranges content | Ordinary assistant text is preserved, with the trained reasoning framing. This is rendering support, not a generation constraint. |

The upstream
[`tools/test_template_golden.cpp`](https://github.com/signalnine/q27/blob/4770e053656af9aababdc49c81f280ad21b74986/tools/test_template_golden.cpp)
and `tools/golden/qwen38_tools_request.{openai,anthropic}.json` compare against
Qwen3.8 template output captured from llama.cpp's minja `/apply-template` route.
Built and ran the **existing**, unmodified golden executable in a disposable
checkout of the exact reviewed commit:

```console
make build/test_template_golden
./build/test_template_golden
```

Result: all boundary checks passed; OpenAI and Anthropic goldens each passed at
2,228 bytes. Source inspection additionally covers grouping of parallel results
and no-tools finalization; these are not claimed as newly executed golden cases.
No new test code was added.

This establishes the upstream native Qwen conversation semantics; it does **not**
prove byte-identical rendering of every canonical Grep trajectory. In particular,
q27 reconstructs argument dictionaries through sorted `nlohmann::json`, whereas
Norted's training template preserves argument insertion order. Client JSON field
ordering and configured thinking/effort can also change bytes. No trained-template
claim is inferred from `general.name` alone, and no Sharp/external-template route
is selected. External-template mode still does not gain native tool capability.

For reproducibility, additional reviewed file SHA-256 values are:

| File | SHA-256 |
|---|---|
| `src/api_common.h` | `e8f87b59bafa6237f9514c90d7a517e1227b808a3384c426fed8ac00425171b7` |
| `src/toolconstrain.h` | `fbaa14821ed872db4d0ff4e07958e0008b0d5c2852a3e18d1bef34db0683755f` |
| `src/toolgram.h` | `e14c8a51760efa42805628b3d7cdd1c7b23bcea0fa950d605288a0fb0e88671d` |
| `tools/test_template_golden.cpp` | `fcb814c7bc062b6968d38f764698a500a167d551d6df217a484f1694427a0813` |

The existing immutable commit/tree binds these files; these hashes document the
review, without adding a second capability allowlist.

## Static implementation validation

The following traces were inspected in the product code. They are static
validation, not simulated model results or a claim of live inference coverage.

| Scenario | Result of code-path inspection |
|---|---|
| Existing general benchmark | Same question, single-tool, Agentic and Coding tasks/oracles; only ceilings change. Performance formulas unchanged. |
| Supported adapter | Running adapter `serving_features(installed runtime, loaded model, resolved running settings)` and initial/history/final request validation precede inference; all four tasks retain their denominator. No currently bundled adapter qualifies, so this path has not had a live smoke test. |
| Unsupported adapter | Four unavailable task records, null category score/raw observations, precise missing reason; other categories continue. |
| Early final | No calls + valid terminal ranges returns immediately, before incrementing executed rounds or constructing finalization. |
| Four-round final | Four successful read-only batches lead to exactly one fifth model request with zero tools, none choice and strict schema. |
| Parallel calls | 1–8 calls, bounded canonical call JSON and unique IDs; independent outputs share equal bytes and retain positional order. |
| Recoverable tool error | Invalid read/grep arguments return deterministic error payloads and increment tool error counts; a later valid final is scored normally with terminal failure false. |
| Region grounding | Any direct tool evidence overlap grounds a gold region; every region must overlap, but every line need not be returned. |
| Imperfect valid final | Partial F0.5/pollution remain objective quality metrics; terminal failure false and clean success false. Aggregate failures/rate count terminal errors only. |
| Malformed final/protocol | Strict parser/semantic rejection or protocol error yields task zero; no capability-unavailable transition. |
| Capability rejection during inference | Relevant unsupported/400/422 engine rejection marks the whole category unavailable, removes provisional task scores and skips subsequent tasks; traces remain. |
| q27 gating/tools | Structured formats remain rejected for reviewed and unproved runtimes; existing native tool and external-template gates untouched. |
| Q6/Q6K admission | `Qwen38Q6` and `Qwen38Q6k` inspection/tokenizer handling unchanged; 24/32 GiB static classes retained. |
| Deadline | 516 work + 51 stops + 15 bookkeeping + 16 cleanup = 598 ≤ 600 seconds, leaving 2 seconds margin. Existing monotonic global timeout/quarantine remains. |
| History/compare/UI | Generic evidence/scorecard persistence carries retrieval; manifest binds semantics; quality deltas and all seven shared display metrics include it. Old record files are never rewritten. |

Focused correction examples (manual code-path and arithmetic review, not live
model trajectories or new automated tests):

- Admission: a future adapter may omit StructuredOutput globally yet return it
  alongside ToolCalling for one running runtime/model/settings tuple. That tuple
  proceeds to all three request validators. A tuple omitting either serving
  feature remains unavailable even if both appear in static capabilities.
- Recovery on `grep-lease-1`: a read of `src/leases.rs` lines 0–7 returns
  `{"error":"invalid line range"}`. A later read of lines 2–7 followed by a
  valid Stop final selecting 2–7 yields task score 100, `tool_errors: 1`,
  `malformed_calls: 1`, `recovered_tool_errors: true`, `failure: false`.
- Selecting lines 2–8 instead yields file F0.5 = 1, line precision = 6/7,
  recall = 1, line F0.5 = 15/17, returned lines = 7 and pollution = 1.
  Task credit is approximately 88.2353, `failure: false`, `clean_success: false`.
- Direct evidence at line 3 alone overlaps this task's gold region 2–7:
  `target_ranges_grounded: 1`, `grounded_success: true`. For multi-region
  tasks every region needs its own overlap. This does not alter final-range F0.5.
- A terminal `{}` fails the final parser: score 0, `failure: true`,
  `malformed_final: true`. A tool-protocol violation remains terminal and is
  recorded as such instead of being relabeled a malformed final.
- One such malformed final among four observed tasks gives `failures: 1`,
  `failure_rate: 0.25`, `malformed_final_rate: 0.25`; imperfect valid tasks do
  not increase these rates. A headline still needs all four scored tasks.

`./validate.sh` covers formatting, workspace check, all-target Clippy with warnings
as errors, and the existing workspace tests. No model benchmark or inference
smoke run was performed; no canonical history was created. Model ranking and
representative-hardware timing calibration remain future work. The material
runtime blocker is genuine constrained no-tools finalization, not Q6/Q6K artifact
admission.
