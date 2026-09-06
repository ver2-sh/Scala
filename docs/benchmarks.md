# Private per-profile benchmarks

Norted Quick Bench v1 is an original, bundled, offline task pack. The running
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
in a response execute in listed order. Each task allows at most four model
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
| Preparation, integrity checks and compatible loading | 90 s |
| One separate unscored warm-up | 10 s |
| Four streaming speed/latency requests | 4 × 15 s |
| Intelligence | 24 × 10 s |
| Single-turn native tools | 8 × 8 s |
| Multi-step fixtures | 4 × 30 s |
| Global cancellation/finalization reserve | 16 s |
| Total | 600 s |

Each task/preparation ceiling reserves its last **four seconds for managed
termination**. Thus preparation work is capped at 86 s; inference is capped at
6 s for warm-up/intelligence, 11 s per probe, 4 s per single-turn tool case and
26 s for the whole multi-step task. This conservative split covers the native
supervisor's forced-stop and pipe-drain allowance. Unused time is not lent to
other tasks. Work stops immediately on completion. The global execution future
is cancelled by 584 s, retaining the final reserve for stopping and recording
work. Ordinary OS/storage failure can prevent confirmation of cleanup; the
server then quarantines inference instead of claiming successful cancellation
or admitting conflicting requests.

Additional output limits are 32 tokens for warm-up, 512 for probes and 384 for
intelligence/tool turns, capped further by a stricter configured or known
effective profile limit. Sampling, reasoning, templates, system prompts,
quantization and runtime selection are preserved. Unknown runtime defaults are
not invented. The record distinguishes the saved configuration, actual served
requested settings, effective provenance and startup observations. Reusing a
session with different load settings is explicitly identified.

A timeout records an unsuccessful attempted task (zero for a scored task),
stops the managed backend, and ends the suite as incomplete. Remaining tasks
stay unattempted; there are no silent retries or timed reloads. Setup failure,
infrastructure failure, unsupported capabilities, cancellation and interruption
remain distinct. Only full task coverage yields a normal section score. A
partial attempt can never replace a previous completed result or advance its
Last benchmark timestamp.

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

The four probes are two short technical/data prompts plus two distinct medium
contexts: 48 synthetic service records and 48 repository change records. Their
fixed full text, exact UTF-8 byte/Unicode character sizes and output limits are
in the manifest. Each requests 180–240 words. Workloads are never shortened for
a profile. `input_utf8_bytes` and `input_unicode_characters` describe the fixed
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
* Native output tokens divided by whole request duration are labelled
  **native end-to-end output tokens/s**, never decode speed. Native counts
  must be present, nonzero and arithmetically consistent; invalid or
  reasoning-count-inconsistent usage cannot yield a rate. Reasoning-inclusive
  counts are never divided by a visible-only timing interval.
* Headline latency is **time to first visible output in milliseconds**.
  First-text and completion latency remain separate. The shared event contract
  does not distinguish answer text from reasoning, so first-answer latency and
  native decode-only rates are unavailable.

Details retain short and medium groups separately, with medians, counts and
ranges. A combined median requires all four samples for that method; each
context group's median requires both samples. There is no substitution of short
samples for missing medium samples, P95 estimate, mixed-method ranking or
manufactured 0–100 speed/latency score. Empty, explicitly detected refusal,
short and invalid samples retain failure/insufficiency reasons. The text
workloads are not quality-judged; refusal detection is conservative and cannot
recognize every paraphrased refusal.

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

Responses/tool arguments are bounded to 64 KiB per model turn; multi-step
response text, calls and turns are bounded; records have an 8 MiB ceiling.
Summary inspection is cached and bounded to 4096 recent records, with at most
200 entries returned by a profile-history request. Full raw records remain
inspectable/exportable by ID after their artifact/runtime is removed. The TUI
never deserializes all historical transcripts to draw a row.

Selection prefers the latest completed result matching the current semantic
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
