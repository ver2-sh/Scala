# q27 Grep contract review

The original review below records the historical v0.10.0 contract. The
[v0.14.3 static audit](#v0143-static-runtime-audit-2026-10-05) adds a separate immutable contract;
it does not claim the historical golden execution was repeated.

Historical review basis: Scala master
`094b2538ad9d16d8de3a160aefd289bc5d42f171`. The reviewed upstream commit is
[`4770e053656af9aababdc49c81f280ad21b74986`](https://github.com/signalnine/q27/tree/4770e053656af9aababdc49c81f280ad21b74986),
tree `ff712f78fd17b5fe12149679114b6def003f16a6`.
Scala's existing provider checks commit/tree, source owners, build
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
adapter translation. No q27 source/runtime or Scala model is modified.

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
Scala's training template preserves argument insertion order. Client JSON field
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

## v0.14.3 static runtime audit (2026-10-05)

This review used `git fetch --no-tags https://github.com/signalnine/q27.git
refs/tags/v0.14.3:refs/audit/q27-v0.14.3` in Scala's object database only, followed by
`git show`/`git ls-tree` and GitHub read APIs for the exact release/tag. No upstream
checkout, build, golden execution, inference, model work, services or Actions ran.

Immutable tag object `cd3e2c4faa0dc476c49f73eab7736341689a268b` targets commit
[`a4d5fc4be1231214e25c578eda7ab659a55689e7`](https://github.com/signalnine/q27/tree/a4d5fc4be1231214e25c578eda7ab659a55689e7),
tree `db65c9363346935f2f12498d2438418e16e12557`.
SHA-256 of exact unmodified source bytes:

| Owner | SHA-256 |
|---|---|
| `Makefile` | `2065409f8f5365b3474aa91deb1f72e6d280253f1ae288b272b60cbc9f4897d1` |
| `README.md` | `5bb6d0684905a105010c82e139d5840ecd6ca8e03e027024dd338c85c880d33b` |
| `src/server.cu` | `608ec913486bd437b60a81e1a96ece7628a10261eba5fe077c5f987ffd6a8a1a` |
| `src/engine.cuh` | `9909cd4ae5800b820c73708c0ac5332bea47599e5951667337a6d9717e371a07` |
| `src/api_common.h` | `afa192e456f9627cc539276c6d00c49bd49040f8bc4a95e02a6beac922eae942` |
| `src/conductor.h` | `8116523aa64db6c01376c85794293a2dbef4444c85de62d95c77fe8410496fdf` |
| `src/depthctl.h` | `a3fb1158f35423b2330c1bce097b5e7c8537adb020c62f3d2ab95251a511f9d7` |
| `src/prefill.cu` | `6fdfd91261057878ba5ccccf9092158fe5ed5015f6b81f2aa43342db7b6cc6ce` |
| `tools/repack.py` | `8f7e9df3cf6b92151a7f3bf2e96d039435abf8d3b61ec094e86c0db6a9ffa63b` |

The provider matches commit/tree and the first four file digests, selects recipe
`q27-upstream-make-v3-a4d5fc4`, and rechecks persisted source build provenance.
The immutable tree binds secondary owners as well; no upstream source is copied.
The old v0.10.0 source and v0.6.2 binary contracts remain separate. Unknown binaries,
version labels, help tokens and a changed source digest receive no new grants.
The audited source recipe is offered alongside, rather than inferred from, the binary.

GitHub release ID `403127767`, published `2026-10-04T17:27:10Z`, has archive asset
ID `610271166`, `q27-v0.14.3-linux-x86_64.tar.gz`, 27,522,833 bytes, digest
`sha256:eb3db6102879c4239119958e83711af4948ed7a6e3144b82e18bc3ee146004f3`.
Checksum asset ID `610271173`, `SHA256SUMS-0.14.3`, 98 bytes, digest
`sha256:0cdd509849bb826f350818329ea58495afb966d0081530aac652a969ad7bed71`.
These are API facts, not a downloaded/inspected ELF claim. Release metadata explicitly
requires driver r580+ and glibc 2.38+ and declares static CUDA 13.2. This does not
require a host CUDA toolkit. The standard servers have sm86/sm89/sm120 plus the
sm120a PF4 object; the distinct `q27-server-12g` serving image is sm86 only.
`Makefile:324–332` binds its W_MAX=8/PF_T=256 build; `README.md:187–223` and
`tools/repack.py` bind the Bonsai 2 T2/T3 slim route. This is not a VRAM upper bound.

### Serving semantics and intentionally unsupported additions

The current `server.cu` argument parser, profile defaults and `parse_sample`, together
with `engine.cuh`, `api_common.h`, `conductor.h`, `depthctl.h`, `prefill.cu` and prefix-cache
owners retain the existing typed controls: context/slots, fast head, thinking and
request thinking/budget, numeric request seed and samplers, KV modes, MTP/suffix limits,
prefix-cache limits, the cc/ref profile, batching/graphs/GEMM/pooling/arena, adaptive
and checkpoint policies, prefill/decode controls and tool parser/dialect/error/size
policies. Existing mappings and precedence are preserved, not replaced by installer
presets. The cc profile's actual batch graph capacity is still 64 (not its stale
512 comment); absent seed remains zero unless forced-temperature sampling assigns a
counter seed. `Q27_SEED=random` is not promoted into the numeric request seed contract.
Bonsai metadata does not expand the existing Qwen v2 trained-template effort grant.

The current native tool masks still depend on registered tools/openers and constrain
tool bodies, not arbitrary no-tools JSON. No `response_format`/JSON-schema serving
route was found in `server.cu`; StructuredOutput and Grep constrained finalization
remain unavailable. Historical goldens above were not rerun.

`Q27_DRAFT_VOCAB` remains unsupported. `engine.cuh:394–420` needs a physical MTP
projection and an output head of Q4_G64, Q8_G128 or T2_G128; its reduced head is solo
only, does not apply to DFlash2, and needs `Q27_BATCH=0`. Scala's bounded metadata
inspection cannot deterministically prove that head inventory/dtype or physical MTP
projection. Enforcing slots/batch alone would be insufficient, so neither this control
nor `Q27_DRAFT_VOCAB_CTX` receives a typed row. Both are dynamically scrubbed along
with all unconfigured `Q27_*` variables. DFlash2, fixed-stack memory estimates,
metrics, logging, dump/probe/internal kernel controls and new environment presets are
not promoted in this audit. Only synthetic Scala adapter tests were added.
