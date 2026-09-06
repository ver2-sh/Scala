# Private per-profile benchmarks

Norted Quick Bench v3 is an original, bundled, offline task pack. The running
server executes it through its normal managed inference path. The TUI and CLI
use authenticated private control; no benchmark endpoints are added to the
public OpenAI-compatible API. No judge model, downloads, shell tools, Python
service or external evaluation service are involved.

Select a Model Profile and choose **Run benchmark** (`b`), or open
`/benchmarks` through normal navigation. Each row represents one profile and
one coherent result. `h` opens its history, Enter opens a result, Space marks a
comparison baseline, and `c` compares the marked result with the selected one.
The mark survives returning to the profile list, so it also supports comparing
different profiles. Comparisons include raw task outcomes, counts, settings,
runtime and hardware differences. They do not declare a winner from a single
changed answer.

The equivalent commands use the same running server:

```console
norted-server benchmarks start PROFILE_ID
norted-server benchmarks status --json
norted-server benchmarks cancel
norted-server benchmarks history PROFILE_ID --json
norted-server benchmarks result RUN_ID --json > result.json
norted-server benchmarks compare LEFT_RUN_ID RIGHT_RUN_ID --json
```

With `--json`, result inspection contains a computed summary and the immutable
record. Human-readable inspection leads with scores, physical units and coverage. Run IDs identify records,
not global multi-model sessions. Two profiles using the same artifact have
separate histories. Starting a profile never clears another profile's results.
There is one admitted job, no queue, and no automatic benchmark trigger.

## Suite and scoring

The complete inputs and answer keys are compiled into the binary from
[`intelligence.json`](../crates/norted-engine/src/benchmark/intelligence.json)
and [`suite.rs`](../crates/norted-engine/src/benchmark/suite.rs). Each record
includes the manifest, its SHA-256, rubric/methodology versions, inputs, request
limits, responses, native usage, timing observations and bounded tool evidence.
Answer keys and grading state are never passed into model messages or tools.

**Norted Quick Intelligence** uses 24 equally weighted tasks: six each in
logical/numerical reasoning, grounded context understanding, code reasoning and
instruction following. Each uses the binary `exact-json-v1` rubric: parse one
complete JSON scalar or array and compare the whole value to its answer key.
Markdown, commentary, multiple answers, wrong types, ordering or extra items
fail. JSON whitespace is harmless. There is no substring matching or partial
credit. Category pass counts and six-task coverage accompany each score.

```
Intelligence = 100 × mean(four category means)
```

The grounded questions include aggregation, policy application, distractors,
configuration precedence, conditional rates and insufficient causal evidence.
Code tasks cover tracing, aliasing, recursion and concrete bug repairs. This is
code reasoning, not full repository engineering or unrestricted code-generation
correctness. It measures this small task pack under the declared limits; it is
not IQ, AA, MMLU or an estimate of performance on a full public benchmark.

Agentic capability uses eight single-turn native tool cases and four multi-step
fixture tasks. Single-turn success requires the correct number of calls, tool
name and complete, correctly typed arguments, or the correct answer without a
call when a tool is unnecessary. Duplicate argument keys, extra arguments,
invalid lists, wrong revisions and unknown keys are rejected.

The production fixture is an in-memory virtual string/revision store with
`read`, `read_many`, `search`, `update` and `verify`. Keys are never OS paths.
There is no host filesystem, shell, arbitrary evaluation or network access.
Multi-step tasks require inspection before mutation and verification of the
correct final state. They cover using discovered keys/values, recovering from a
forced revision conflict, and discovering a service before enabling it. Calls
in a response execute in listed order. Each task allows at most six model
turns and eight calls; alternative valid sequences are accepted. Saying “done”
without the required state change and verification fails.

```
Agentic = 100 × (0.5 × passed single-turn cases / 8
              + 0.5 × solved multi-step tasks / 4)
```

The native tool contract is checked against the actual loaded adapter/schema.
When unavailable, the whole agentic section is unavailable with a reason;
weights are never redistributed. Invalid model-generated arguments fail the
rubric. These are short tool-use tasks, not evidence of long-horizon autonomy.

## Timing and execution limits

One monotonic 600-second deadline starts at admission, before runtime/model
preparation or loading. The budget ceilings are frozen:

| Phase | Ceiling |
|---|---:|
| Preparation, integrity checks and compatible loading | 60 s |
| One separate unscored warm-up | 10 s |
| Four streaming speed/latency requests | 4 × 20 s |
| Intelligence | 24 × 10 s |
| Single-turn native tools | 8 × 8 s |
| Multi-step fixtures | 4 × 30 s |
| Task work ceilings including preparation | 574 s |
| Shared cancellation, bookkeeping and finalization headroom | 26 s |
| Total maximum | 600 s |

Tasks receive the full stated work ceilings: intelligence gets 10 s, single-turn
tools 8 s and a complete multi-step task 30 s. There is no per-task four-second
termination deduction. Each stopped request has up to 1 s to confirm engine-side
cancellation and health, charged to the same global budget. Preparation receives
60 s; unused time remains global headroom, not extra task retries or inference.
The 574 s phase ceilings leave 26 s of headroom. Cancellation and bookkeeping
share that headroom; the global deadline still interrupts execution if it is
exhausted, rather than extending the benchmark.
Execution ends at 584 s, cleanup is bounded by 599 s, and finalization waits no
later than 600 s. The terminal writer checks the monotonic deadline before
publishing, so a delayed disk operation cannot publish a new successful record
after an expired finalization deadline. OS/storage stalls can prevent durable
finalization or confirmation of process cleanup; checkpoints remain recoverable,
and unconfirmed stopped work quarantines inference.

Additional output limits are 32 tokens for warm-up, 2048 for probes and 384 for
intelligence/tool turns, capped further by a stricter configured or known
effective profile limit. Sampling, reasoning, templates, system prompts,
quantization and runtime selection are preserved. Unknown runtime defaults are
not invented. The record distinguishes the saved configuration, actual served
requested settings, effective provenance and startup observations. Reusing a
session with different load settings is explicitly identified.

A timeout records an unsuccessful attempted task (zero for a scored task).
Candidate output/call-limit violations also fail the task. Continuation requires
proof of stopped work and a healthy, unchanged runtime. The llama.cpp adapter
closes its HTTP stream and polls its inference-queue `/slots` contract for idle
slots, then checks health. This path requires an emitted inference event (proof
of admission) and an already-enabled slots endpoint; it never changes profile
settings. Missing admission proof, disabled/unknown slot contracts, q27/NInfer
without an implemented stop acknowledgement, broken transport or unhealthy
backends retain safe termination and an incomplete run. Merely dropping the
manager's request lease is not treated as proof. There are no reloads or retries.

A finished evaluation with missing speed/native metrics or unsupported agentic
capabilities is `completed_unavailable`, distinct from interruption. Fully
covered scored sections remain usable with their fixed denominators. Both
finished statuses participate in latest-result selection and advance Last
benchmark; failed, cancelled and incomplete attempts preserve previous finished
results and appear separately. Every row comes from one run. Insufficient samples
and aggregate measurements remain unavailable, never zero or synthesized.

Loading and inference use the normal runtime manager. An admission reservation
rejects concurrent inference/load/unload and conflicting runtime mutations;
existing active inference or loading makes a start request busy. Other resident
profiles are not unloaded. The selected benchmark backend is stopped on
cancellation/error to ensure work cannot continue after dropping a stream.
Load-task cancellation protects the supervisor-to-manager process handoff, so
an aborted preparation cannot later publish a successful load. Navigation or
TUI disconnection does not cancel execution. Server shutdown cancels it and
preserves evidence.

## Physical speed and latency measurements

The four probes copy two fixed approximately 200-word passages, once with short
context and once after medium context (48 synthetic service records or 48
repository change records). The requested output population is identical within
each short/medium pair. Their fixed full text, exact UTF-8 byte/Unicode character
sizes and output limits are in the manifest. All profiles receive the same
copy instructions, 2048-token cap and 20-second deadline. The cap includes
reasoning where the runtime uses a shared output budget. No request disables or
caps thinking separately. Workloads are never shortened for a profile. `input_utf8_bytes` and `input_unicode_characters` describe the fixed
task text, not native tokens or the profile's complete rendered prompt. The
record also includes the nonce-prefixed model-visible messages. Native usage,
when supplied, describes the runtime's whole request including templates and
other inputs; absent native counts remain unknown.

One different, small request warms the selected backend. A fresh fixed-length
run nonce precedes benchmark user inputs to prevent reuse of cached task-pack
prefixes across runs. Persistent cache settings are not modified. Common system
prefix caching and backend cache state remain unverified: these are **not
claimed to be cold-cache measurements**.

All timing starts at the **internal managed streaming inference boundary**,
including request preparation/routing overhead, not an external HTTP client.
Loading time is separate. Stored timestamps include request start, first
nonempty visible output, first text, last text and completion. For tools, a
complete validated native call is required before reporting an executable
action. A parseable intermediate argument prefix is insufficient.

* Headline speed is Unicode characters per second delivered **after the first
  text chunk**: `(total characters − first-chunk characters) /
  (last-text time − first-text time)`. The first chunk is excluded from both
  the delivered population and elapsed delivery span. At least 400 total
  characters, 128 subsequent characters and 50 ms of delivery are required.
* Visible Unicode characters divided by whole request duration are separately
  labelled **text end-to-end chars/s**. A completed one-chunk response can
  provide this rate, but cannot provide delivery speed. Whole-request rates
  require a finite, ordered completion time of at least 50 ms. They are never
  substituted into a delivery-speed comparison.
* Native output tokens divided by whole request duration are labelled
  **native end-to-end output tokens/s**, never decode speed. Native counts
  must be present, with nonzero output and arithmetically consistent totals; invalid or
  reasoning-count-inconsistent usage cannot yield a rate. Reasoning-inclusive
  counts are never divided by a visible-only timing interval.
* Headline latency is **time to first visible output in milliseconds**.
  First-text and completion latency remain separate. The shared event contract
  does not distinguish answer text from reasoning, so first-answer latency and
  native decode-only rates are unavailable. q27 routed chat supplies separate
  `reasoning_content`, which is not visible text; its top-level
  `usage.reasoning_tokens` is preserved as the optional reasoning subset of
  completion tokens. Raw completions without that counter remain unknown.

Metric validity is independent. A short response or absent native usage does
not erase first-visible latency. Insufficient delivery span does not erase a
valid whole-request rate. A later timeout retains the observed first-visible
and last-text timestamps with its unsuccessful outcome; without completion it
has no whole-request rate. No output before a timeout means “no output before
deadline,” never zero. A terminal token limit retains its measured whole-request
rate and latency but is not a successful full performance probe.

Details retain every probe and its outcome, values and per-metric reasons.
Short and medium groups have separate medians, counts and ranges. A full
combined median requires four successful samples for that method; each context
group requires both. Available subsets have a separate `partial_median`, labelled
**partial**, with n/N and short/medium coverage in both TUI and CLI. Unsuccessful
observations remain inspectable individually and do not fill the successful
comparison denominator. Missing native usage does not invalidate the text rate.
No short-only or mixed-method result is presented as a full comparison. There
is no P95 estimate or manufactured 0–100 speed/latency score. Refusal detection
remains conservative; performance text is not quality-judged.

The TUI heading and each result identify the reported suite. Summaries identify
their evidence-derivation algorithm as `independent-observed-metrics/3`; this can
expose previously hidden observations in historical records without rewriting
those records or changing their collection suite/methodology. Version 2 and 3
workloads remain distinct and are not comparable performance measurements.

## Storage, identity and privacy

Records live under the application's **data directory / `benchmarks`**, separate
from `model-profiles.json` and `settings.json`. Run records use UUID filenames,
atomic temporary-file replacement, fsync and a local file lock. Unix directory
permissions are 0700 and record files 0600. Terminal `.json` records are
immutable. `.active` checkpoints retain completed task and multi-turn evidence;
a server holding the history ownership lock converts them to interrupted records
on restart without resuming. Offline CLI runtime probes cannot recover or rewrite
a running server's checkpoints. The restart
observation is identified separately from an unknown actual process-stop time.

Tool results become observation evidence only for the next response. Independent
reads can share a response; discovery must precede the dependent inspection,
an observed revision must precede an update, and verification must follow the
update in a later response. Conflict recovery needs the returned conflict and a
fresh read before correction. The six-turn/eight-call limit accommodates the
five-turn read → conflict → read → update → verify solution. Per-turn evidence
includes the pre-response fixture observations, calls and returned results.

Responses/tool arguments are bounded to 64 KiB per model turn; multi-step
response text, calls and turns are bounded; records have an 8 MiB ceiling.
Summary inspection is cached and bounded to 4096 recent records, with at most
200 entries returned by a profile-history request. Full raw records remain
inspectable/exportable by ID after their artifact/runtime is removed. The TUI
never deserializes all historical transcripts to draw a row.

Selection prefers the latest finished result (including `completed_unavailable`) matching the current semantic
configuration and task-pack hash, never the highest score. Otherwise the latest
completed result is historical, accompanied by configuration-change or
unverified-identity reasons. The latest attempt is shown separately. If no completed result exists, its
evidence is labelled Incomplete/Failed and has no Last benchmark date. Renames do
not invalidate a result. Inherited settings, actual runtime identity/binary,
artifact/auxiliary file observations and relevant host identity participate in
matching. Pure display names and observation timestamps do not. A Current
label requires an observed running runtime and sufficient identity evidence;
unverified fallback next-load selection stays historical. File metadata checks
are explicitly metadata observations, not fresh SHA-256 verification of every
model on each refresh. Existing provenance retains acquisition/build evidence
semantics and canonical Norted lineage.

Configuration matching and comparison methodology are distinct. A comparison
shows whether suite/scorer methods match and whether configuration keys match;
cache/external workload conditions remain unverified even for matching keys.
Raw settings/runtime/hardware differences and task outcomes accompany score
and latency deltas. Never infer statistical significance from this small pack.

Only allowlisted CPU/RAM/OS observations and the existing host accelerator
identity/VRAM/driver observations are collected. Existing structured/redacted
runtime provenance is reused; the private backend endpoint is redacted. Control
tokens, authorization headers, transient backend credentials and unredacted
environment dumps are not copied. No builder manifests, model transformations
or Norted-Utils code are modified.

## Validation limits

The bundled oracles, invalid/ambiguous answers, fixture argument validation,
final-state checks, physical-rate guards, per-profile selection, immutable
storage and restart recovery can be validated offline. Live task difficulty,
repeatability, output-length adequacy and score discrimination need calibration
on safely available inference hardware. No calibrated runtime duration or
measured model score is implied by the suite's maximum budget.

## Version 2 correctness review

All 24 intelligence prompts and keys were reviewed. `logic-5` now asks for all
possible culprits, alphabetically: `["Ada", "Cy"]`. Exhaustive truth assignments
show Bo violates the one-true-statement premise, while Ada and Cy both satisfy
it. `context-2` explicitly requests a JSON array, and `code-3` explicitly requests
only the option letter. Other keys retain their results. Full JSON equality
remains the oracle; commentary, substrings and alternative shapes fail.

Suite `norted-quick-bench/2` and methodology
`binary-json-fixture-visible-delivery/2` identify the corrected pack, observation
grading and task-failure policy. The manifest hash includes prompts, answers,
fixture state and frozen limits. Version 1 records remain immutable and are not
regraded or labelled comparable to version 2.

## Version 3 performance correction

The local v2 q27 Q6/Q6K records exhausted all 512 output tokens without visible
text in roughly 3.5–4.7 s. Their exact installed q27 v0.10.0 source
(`4770e053656af9aababdc49c81f280ad21b74986`, w12) uses a shared reasoning/answer
output cap. The recorded profiles enabled thinking with an unbounded thinking
budget. Their runtime chat path separates reasoning from visible content;
Norted previously discarded its native reasoning-token counter. A replay of
the original short probe through the corrected serving path confirmed 512
completion tokens, all 512 reasoning tokens, zero visible characters and a
`length` finish after 4.66 s. Raising the
cap alone did not make the explanatory workload reliable in live validation:
one request still had no visible answer at a 30 s deadline.

Version 3 replaces performance-only reasoning/review prompts with fixed passage
copying and reserves 2048 total output tokens and 20 s per probe. The scored
intelligence and agentic packs, settings precedence, runtime/template selection,
and 600 s ownership/cancellation deadline are unchanged. The existing delivery
guards (400 total characters, 128 after the first chunk, 50 ms) are unchanged.
The q27 SSE parser continues forwarding text incrementally and collecting final
usage after the finish frame. It filters initial raw-template reasoning only
for raw completion text, since routed chat content has already been separated
from reasoning by the runtime. This prevents suppression of an answer when the
optional initial-reasoning filter is selected on the chat route.

## NInfer startup validation

NInfer's reviewed startup log uses different names for different identity
fields: `artifact.target` is the registry dispatch key (for example
`qwen3_8_27b`), while `engine.context_cost.model_id` is the container's native
model ID (`qwen3.8-27b`). Norted validates the explicit registry mapping, both
weights IDs, and the independent public Model Profile ID. Unknown mappings and
mismatches are rejected; punctuation is not normalized speculatively.

The same distinction applies to KV settings: the CLI choices `fp8` and `int8`
are reported as `fp8-e4m3-row256` and `int8-group64`. Validated settings use the
canonical CLI choice, and `kv_cache_format` retains the exact native format as
a startup observation. Sampler arguments are parsed by NInfer as 32-bit floats;
Norted validates the exact value obtained from the same decimal argument at
that precision, including values such as `0.6` reported as
`0.6000000238418579`. This does not change the saved or requested setting.

Previously, these valid representation differences rejected startup, and the
health polling loop treated the rejection as retryable after its startup proof
had already been discarded. It eventually reported a setup timeout instead of
the cause. Startup-proof rejection now returns a permanent configuration error
with the validation reason, so the manager stops the owned process promptly.
Temporary lack of readiness still follows the existing polling contract.
Artifact verification, GPU binding, profile settings and the 60 s setup / 600 s
overall benchmark limits are unchanged.

Live validation on the installed `a140e7ae82a11ed2f370a4d8f2cc16268a3790b8`
NInfer runtime (`ninfer-serve-v2-sm120a`) completed all four performance probes
and both scored sections without preloading the model. Temporary startup-record
replay also checked valid target/KV mappings, float conversion and rejection of
wrong target/model/weights, GPU, public profile, context, speculation, format and
schema values. These checks do not establish compatibility with unreviewed
runtime contracts.
