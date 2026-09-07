# Private capability-aware benchmarks

Norted Quick Bench v4 is an original, bundled, offline benchmark for private
comparison of your own local Model Profiles. The server runs normal managed
inference through authenticated private control. There is no judge model,
downloaded task suite, public benchmark API, automatic run, shell, host-filesystem
tool, network service, or global multi-model session. Each admitted run belongs
to one profile. Terminal records are immutable. New v4 records use record format version 2;
v3 record format version 1 remains readable.

## Declare intent on the Model Profile

Profiles store an explicit typed set: `reasoning`, `coding`, `tool_use`,
`retrieval`, `long_context`. These are semantic intentions, independent of
architecture, filenames, family names, model size and runtime features.
`retrieval` requires `tool_use`. An empty set intentionally selects operational
probes only and has no Profile Quality score. No capability is inferred.

```console
norted-server model-profiles create coding --model MODEL_ID --engine ENGINE_ID --capabilities reasoning,coding,tool_use,long_context
norted-server model-profiles set-capabilities coding reasoning,coding,tool_use,long_context
norted-server model-profiles set-capabilities context-finder retrieval,tool_use,long_context
```

In the existing Model Profiles editor, **C** opens the comma-separated capability
editor; an empty entry clears the set. Newly created profiles start with no
quality capabilities until explicitly edited. Duplication copies the set.
Capabilities participate in `ModelProfile.content_hash()`; display-name changes
still do not. The profile state format is **version 3**. Unsupported state
versions fail clearly. There is no profile migration or legacy default-capability
pathway. Benchmark history has its own narrowly scoped v3 record reader.

## Standard and Quick

```console
norted-server benchmarks start PROFILE_ID --mode standard
norted-server benchmarks start PROFILE_ID --mode quick
norted-server benchmarks plan PROFILE_ID --mode quick --json
norted-server benchmarks status --json
norted-server benchmarks cancel
norted-server benchmarks history PROFILE_ID --json
norted-server benchmarks result RUN_ID
norted-server benchmarks result RUN_ID --verbose
norted-server benchmarks result RUN_ID --json
norted-server benchmarks compare BASELINE_RUN_ID SELECTED_RUN_ID --json
```

The default is **Standard**, the authoritative full local evaluation, with a
**600-second hard maximum including setup/loading and finalization**. Quick is
a smaller confidence check aimed at about two minutes, with a **180-second hard
maximum including setup/loading and finalization**. A plan inspection performs
no load or inference. Quick is a separate frozen selection, not Standard stopped
early. Neither mode retries or redistributes unused task time.

The `/benchmarks` page uses **b Standard**, **q Quick**, **h History**, Enter or
**d Details**, Space to mark a baseline, **c Compare**, **e Evidence**, and
**x Cancel**. On this page with content focus, q runs Quick; it does not quit.
Profile-page b still starts Standard. Overview prefers a current finished
Standard result with the expected Standard pack and configuration key. A newer
Quick does not hide it: Latest Quick, Latest attempt and independent history
entries remain visible. If there is no finished Standard, Quick is explicitly
labelled. Failed/cancelled attempts do not displace a finished scorecard.

The compact table shows Profile, Profile Quality and Mode, adding warm TPS,
latency, a relevant capability score, state/signature-key prefix and timestamp
as width permits. The selected scorecard shows
only declared quality categories plus operational measurements. Details and
verbose CLI include sample coverage, uncertainty, raw retrieval metrics, context
rungs, missing reasons, signature, configuration and hardware/runtime provenance.
Evidence/JSON retain the underlying immutable record.

## Frozen plan and time proof

Suite: **`norted-quick-bench/4`**. Method:
**`capability-json-fixture-retrieval-context/4`**.
The manifest binds selected IDs, inputs, hidden answer keys, tool definitions,
fixtures, rubrics, context payloads, output caps and phase ceilings. Answer keys
and grading state are never included in model messages or tool definitions.
The selected `BenchmarkPlan` and its digest are in the manifest; the resulting
manifest digest remains `pack_hash`. Plan identity is repeated in the signature
for historical comparison, not used as a replacement for `configuration_key`.

Worst case, with **every capability** declared:

| Phase | Quick | Standard |
|---|---:|---:|
| Preparation, integrity and actual loading | 30 s | 60 s |
| Unscored warm-up | 3 s | 5 s |
| Speed probes | 2 × 11 s | 4 × 17 s |
| Reasoning JSON tasks | 6 × 3 s | 18 × 5 s |
| Coding JSON tasks | 2 × 3 s | 6 × 5 s |
| Single-turn native tools | 3 × 3 s | 8 × 4 s |
| Multi-step Agentic fixtures | 1 × 9 s | 4 × 16 s |
| Retrieval tasks | 3 × 10 s | 8 × 12 s |
| Context rungs | 2 × 5 s | 5 × 14 s |
| Phase work total | **137 s** | **515 s** |
| Stop confirmations (one second per task, plus warm-up) | **20 s** | **54 s** |
| Execution bookkeeping allowance | **7 s** | **15 s** |
| Cleanup/finalization allowance | **16 s** | **16 s** |
| Unallocated margin | **0 s** | **0 s** |
| Hard maximum | **180 s** | **600 s** |

There are at most 19 Quick or 53 Standard tasks, excluding warm-up. Progress
uses the selected plan, including explicit unavailable tasks. For subsets of
capabilities, work ceilings only decrease. Plan construction verifies that work
plus explicit stop-confirmation, execution bookkeeping and cleanup/finalization allowances fits the hard maximum.
Standard: 60 + 5 + 68 + 120 + 32 + 64 + 96 + 70 = 515;
515 + 54 × 1 + 15 + 16 = 600 ≤ 600 seconds.
Quick: 30 + 3 + 22 + 24 + 9 + 9 + 30 + 10 = 137;
137 + 20 × 1 + 7 + 16 = 180 ≤ 180 seconds.
The fixed bookkeeping allowance covers repeated pinned-configuration verification,
checkpoint serialization, Store writes/fsync and progress updates outside task
ceilings. The execution deadline is hard maximum minus the 16-second cleanup
reservation: 584 seconds Standard and 164 seconds Quick, accommodating phase
work, stop confirmations and bookkeeping. This is a conservative frozen allowance,
not a guarantee against arbitrarily slow host storage.
Each probe, question, single tool task, retrieval task and admitted context rung
can add at most one stop confirmation. Agent candidate confirmation is inside
the task ceiling; if that ceiling expires, only its outer confirmation adds
time beyond the ceiling. Unavailable tasks reserve their allowance conservatively.
Unused confirmation allowance is never redistributed into inference. The manifest
exposes phase work, confirmation allowance, execution bookkeeping, cleanup allowance
and hard total.
Standard retains all original JSON and Agentic cases with tighter per-task
ceilings to admit the additional packs even for all-capability profiles. These
are frozen limits, not measured model-duration or calibration claims.

Quick selects JSON IDs ending in `-1` and `-4` in each relevant group, native
single cases 1/6/8, Agentic case 3 (conflict recovery), retrieval cases 1/4/7,
short-1 and medium-1 speed probes, and workload targets 4096/16384.
Standard selects all relevant bundled cases and context targets
4096/16384/32768/65536/131072. Above-limit rungs are recorded as unavailable.

Execution stops no later than admission + hard maximum − 16 seconds. Error
cleanup ends by hard maximum − 1 second and each immediate stop has at most
four seconds; durable finalization is bounded by the hard maximum. The terminal
writer checks the monotonic deadline before publishing. OS/storage stalls can
prevent durable finalization or stop confirmation; they cannot extend the
successful-run contract. Recoverable active evidence remains on disk.

## Quality scores

All quality scores use 0–100. Irrelevant categories are absent, never zero.
Missing declared categories are unavailable/incomplete, not reweighted.

- **Intelligence / Reasoning**: 100 × mean of scored `logic`, `context` and
  `instruction` binary outcomes (18 Standard / 6 Quick). All required outcomes
  must be scored. Each selected group has equal sample count.
- **Coding**: 100 × mean of scored `code` outcomes (6 Standard / 2 Quick).
  This measures short code tracing, reasoning and debugging, not full repository
  engineering or unrestricted code-generation correctness.
- **Agentic**: 100 × (0.5 × single-turn pass fraction + 0.5 × multi-step solve
  fraction). Fixed denominators are 8/4 Standard and 3/1 Quick. Correct typed
  arguments, inspection, delivered observations, conflict recovery, mutation and
  later verification are preserved. Multi-step cases still allow six turns and
  eight calls in the existing in-memory revision store. The loaded adapter and
  exact schema must support native tools. Otherwise Agentic and Retrieval are
  unavailable with a precise reason, and Profile Quality is unavailable.
- **Retrieval**: 100 × mean(0.5 × file F0.5 + 0.5 × line F0.5), described below.
- **Context**: 100 × mean of context-rung binary outcomes only when every
  frozen planned rung is scored. An unavailable rung makes the headline Context
  and Profile Quality unavailable, without scoring that rung as zero. Partial
  observed quality, attempted/scored/required coverage and useful context remain.
  An admitted context-limit failure still counts as a model-attributable failure.
  For a five-rung plan with only 4K admitted and passing, coverage is 1/5 and
  useful context is 4K; neither Context nor Profile Quality has a headline score.

JSON quality uses complete JSON equality, `exact-json-v1`: no commentary,
markdown, substring matching, fuzzy oracle, judge or partial answers. Incorrect
answers score zero; infrastructure failures are unscored and interrupt the run.
A model task deadline scores zero only with the existing stopped-work safeguards.

**Profile Quality** is the simple mean of all complete scores corresponding to
the declared capability set. Reasoning maps to Intelligence, coding to Coding,
tool_use to Agentic, retrieval to Retrieval, long_context to Context. Missing
any declared score prevents Profile Quality. Empty capability sets have no
Profile Quality. Reliability, speed and efficiency never enter it. A retrieval /
tool / context score of 91 is not equivalent to 91 for reasoning / coding / tool /
context. The capability set accompanies the score and is part of comparability.

Binary categories store passed, attempted, scored, required, task IDs and Wilson
95% intervals. Retrieval stores n, mean, sample standard deviation, standard
error and min/max. Quick explicitly indicates its smaller confidence-check
sample. There is no invented combined confidence interval or claim that tiny
score differences are decisive.

## Retrieval / Fast Context

[`retrieval.rs`](../crates/norted-engine/src/benchmark/retrieval.rs) bundles an
original virtual source repository and eight realistic retrieval requests:
symbol location, API-to-implementation follow-up, current configuration versus
historical documentation, worker versus UI decoys, a cross-file permission
switch, caller/clock follow-up, adjacent export predicates, and authentication
call/callee tracing. Quick uses three fixed representative requests.

Tools are deliberately small and deterministic:

- `grep`: case-sensitive Rust regex (no PCRE2), 1–256 pattern bytes,
  bounded compilation (1 MiB regex and DFA limits), optional file/directory
  `path`, `include` and `exclude` glob filters. Sorted path/line/text hits,
  at most 80 per call; malformed regex returns a bounded tool error.
- `glob`: sorted paths, `*` matches any characters including slash; no other
  wildcard syntax. Patterns are limited to 256 bytes.
- `read(path, start_line, end_line)`: inclusive one-based ranges, at most 80
  lines per call. Unknown paths, invalid starts and reversed ranges are errors.
  End beyond EOF or oversized spans return the available bounded prefix with
  `truncated: true`, preserving useful evidence in that round.

Model-visible results use `path-lines-json-v1`, recorded as retrieval
`result_serialization` in the manifest and thus included in the pack digest and
quality signature. The typed canonical evidence is serialized deterministically
as `{"files":{"path":[[line_number,"text"]]},"truncated":false}` without an
`ok` envelope. Glob uses the same map with empty arrays. Truncated reads add
`"bounded":true`; source strings are JSON escaped, never interpolated into chat
or XML framing. Errors use `{"error":"fixed diagnostic"}`; invalid arguments
produce a fixed message rather than echoing parser input or host errors.

For example, `grep(pattern="authorize|auth", path="src")` returns:

```json
{"files":{"src/api.rs":[[2,"use crate::auth::authorize;"],[4,"    authorize(&req, &cfg)?;"]],"src/auth.rs":[[1,"pub fn authorize(req: &Request, cfg: &Config) -> Result {"]]},"truncated":false}
```

`read(path="src/auth.rs", start_line=1, end_line=80)` returns:

```json
{"files":{"src/auth.rs":[[1,"pub fn authorize(req: &Request, cfg: &Config) -> Result {"],[2,"    if req.token.is_valid() { return Ok(()); }"],[3,"    if cfg.allow_guest && req.method == \"GET\" { return Ok(()); }"],[4,"    Err(Denied)"],[5,"}"]]},"truncated":true,"bounded":true}
```

`glob(pattern="src/*retry.rs")` returns
`{"files":{"src/jobs/retry.rs":[],"src/ui/retry.rs":[]},"truncated":false}`.
`read(path="missing", start_line=1, end_line=80)` returns
`{"error":"unknown virtual path"}`.

Tools operate only on bundled strings, never filesystem, shell or network.
There are at most four serial retrieval rounds, up to eight independent calls
per response (32 total), and 320 returned source lines across the task, including
repeated evidence. Retrieval requests allow up to 1024 output tokens (subject
to the existing resolved runtime/profile cap) to accommodate eight-call responses.
The fixed ten-path repository also bounds glob output.
Calls execute deterministically in response order; all assistant/tool history is
preserved. After four executed retrieval rounds (or an earlier no-call response),
a separate finalization turn has an empty callable tool list and common-interface
`tool_choice: None`. No fifth retrieval action executes, even if emitted.
The final response must be only:

```json
{"ranges":[{"path":"src/example.rs","start_line":3,"end_line":8}]}
```

Manual contract inspection: `grep(pattern="authorize|auth", path="src")`
uses regex alternation; `read(path="src/auth.rs", start_line=1, end_line=80)`
returns its five available lines with truncation indicated. A canonical range
using `path`, `start_line`, `end_line` is accepted by the grader; the old
`file`/`start`/`end` schema is rejected. Canonical targets are passed only to
local grading/evidence and never to model messages. File and line precision,
recall, F0.5, pollution and grounded success retain their existing formulas.

At most 16 ranges, line numbers 1–512, no extra fields or duplicate object keys.
Scoring unions ranges into unique file/line sets. Precision is intersection /
predicted; recall is intersection / target. Empty predictions score zero.
`F0.5 = 1.25 × precision × recall / (0.25 × precision + recall)`; a zero
denominator gives zero. Unknown files and extraneous lines count as pollution.
Final-format failure or timeout is an unsuccessful scored attempt.

Each task retains target/predicted files and ranges; file/line precision, recall
and F0.5; returned and polluting line counts; calls, rounds, malformed calls,
timeouts, completion/final-format status and raw tool evidence. **Grounded
success** requires all canonical target lines both in the answer and actually
located through delivered grep/read evidence. Aggregate grounded success rate,
line precision and pollution remain visible separately from Retrieval score.

## Context quality and useful context

[`context.rs`](../crates/norted-engine/src/benchmark/context.rs) builds structured
repository-style evidence. Reasoning/coding profiles get active/inactive and
region distractors interleaved with facts requiring aggregation, a conditional
count and maximum revision. Retrieval profiles resolve a conditional API call
chain across four implementation files amid archived and UI-only decoys.
This is not a repeated trivial needle-string oracle. Retrieval profiles receive
repository evidence accumulation, represented as one deterministic text payload,
not a claimed replay of an actual production chat or tokenized prior search.

Rungs are labelled by **target workload characters**, not exact tokens. They
record actual payload UTF-8 bytes, Unicode characters, native input tokens only
when supplied by the runtime, completion latency, score and failure/exclusion
reason. Admission uses the observed runtime `resolved_settings` context limit:
payload bytes plus a conservative 2048-token overhead allowance must fit. With
unknown observed limits, rungs remain unavailable. This conservative byte-based
screen can exclude workloads that a tokenizer would in fact admit; it never
reports an untested high rung as useful context. Runtime templates/system
prompts and tokenization are not exactly measured by this preflight allowance.

**Useful context** is the highest successfully completed tested rung scoring
at least 80% of a **positive** low-rung baseline. With this first binary oracle,
a passed baseline and passed rung satisfy the rule; a failed/missing baseline
produces no useful-context claim. The exact rule and workload generation are in
the manifest. Untested rungs are never interpolated. Inspect the full ladder,
including non-monotonic outcomes and differing admitted coverage.

## Reliability and physical measurements

**Reliability = 100 × valid completions / model-attributable task attempts**,
across selected quality tasks. Wrong but well-formed answers can be reliable
completions while failing quality. Timeouts, invalid required output shape,
malformed tool serialization/arguments, refusal and bounded-call/output failures
are recorded as model failure classes. Explicit runtime context-limit failures
within an admitted rung are model failures; ambiguous backend/transport failures
remain diagnostic infrastructure errors. Unsupported/unattempted work and
infrastructure failures do not enter the denominator. Valid/attempted counts and
the failure breakdown remain visible, including when run-level evidence is
incomplete. No infrastructure failure is quietly turned into a quality zero.

One warm-up precedes the fixed passage-copying probes. Standard keeps four
probes, two short and two medium; Quick takes short-1 and medium-1. Each probe
requests at most 2048 output tokens, including reasoning where shared. Quality
turns request at most 384; warm-up requests 32. A stricter configured or known
profile output cap wins. Sampling, thinking, templates, system prompt and saved
settings remain intact. A per-run nonce avoids shared task-prefix reuse;
backend/common-system-prefix caches and external GPU workloads remain unverified.

Raw physical metrics remain primary:

- **Native end-to-end output TPS**: native output tokens / whole-request duration.
  Never called decode-only TPS; never replaced by character speed.
- **Visible delivery chars/s**: excludes first-chunk characters and measures the
  span after first text; requires 400 total characters, 128 subsequent characters
  and 50 ms of delivery.
- **Visible end-to-end chars/s**: Unicode characters / whole-request duration.
- **First-visible**, **first-text** and **completion latency**: distinct timestamps
  at the internal managed streaming inference boundary. First-answer latency and
  native decode-only speed remain unavailable.

Native usage must be arithmetically consistent and nonzero. Each metric has its
own validity guard and missing reason. Successful full aggregates require all
selected probes (4 Standard / 2 Quick), with separate short/medium coverage.
Partial medians are explicitly partial. Token-limit and timeout evidence stays
inspectable without filling successful-comparison denominators. The physical
summary derivation remains `independent-observed-metrics/3`; this is independent
of the v4 collection methodology.

**Cold/startup load** is measured around actual loading only if the profile was
not resident at admission. An already resident profile reports unavailable:
“profile already resident.” It is never unloaded to manufacture a cold test.
Warm TPS and request latency are separate from load time. No cold-cache claim.

**Efficiency** reuses observed accelerator binding, device identity and total
memory, CPU model/count and host RAM total from existing low-overhead provenance.
Process/model peak VRAM, process RAM and TPS/GiB remain unavailable because the
managed contract supplies no reliable process-scoped peak telemetry. Host total
memory is not model usage. There is no estimated memory from file size or
quantization, polling sampler, or arbitrary 0–100 Speed/Efficiency index.

## Identity, comparison and immutable history

`configuration_key` keeps its existing job: whether a saved/running configuration
matches this result. It retains actual requested versus effective/observed
settings, runtime/artifact/file identity and relevant host facts. Configuration
editors continue to describe next-load settings; runtime observations do not
become persisted overrides. A Current label requires verified observed identity.

The version-1 **benchmark signature** structurally records suite/method/mode,
capabilities, exact plan/IDs, plan/pack identity and rubric versions; profile and
model identity, existing content/native/package/auxiliary provenance and file
observations; format/quantization metadata where available; exact engine/runtime
identity/version/variant; server version/source revision; effective and observed
settings; accelerator binding/class/count/memory and CPU/host observations.
No Norted build internals or external repository changes are required.

- **Quality comparable** requires equal v4 suite/method, mode, capability set and
  exact selected plan/pack/rubrics. Compared model artifacts may differ.
- **Strict performance comparable** additionally requires observed materially
  matching hardware class, ordered accelerator classes, CPU/host class, engine /
  exact runtime, server version/revision and effective/observed settings. Keys
  exclude model artifact identity, profile ID, setting source/profile ownership,
  physical device UUIDs and incidental timestamps. Unknown hardware/runtime
  identity prevents a strict performance key. Full raw identity remains evidence.

Comparisons report the two decisions separately and enumerate changed fields.
Quality deltas are selected minus baseline only for matching methodology. Raw
physical values can still be displayed across different deployments, but strict
performance deltas are suppressed. No winner is declared. Context admitted-rung
coverage and small sample uncertainty must still be considered within a matching
plan; matching keys are not a claim of isolated GPU/cache conditions.

Quick versus Standard and v3 versus v4 are **not directly comparable**.
Historical v3 records and summary sidecars remain readable and labelled **legacy
methodology**. Their original Intelligence/Agentic semantics and task evidence
are retained; no v4 scorecard/signature/capabilities are inferred from them.
Compatibility decoding of missing profile capabilities is confined to v3 benchmark
records, never the profile state store. Original terminal files are not rewritten.

Records remain in the data directory's `benchmarks` folder, independent of
profiles/settings. UUID terminal `.json` files use private permissions, locks,
atomic persistence and fsync; terminal overwrite is rejected. Active checkpoints
preserve completed and multi-turn evidence. Only the owning server recovers
checkpoints as interrupted, never resumes them. History remains bounded to 4096
cached summaries / 200 returned per profile; raw records remain inspectable by
ID. Evidence bounds remain 64 KiB per turn and 8 MiB per record.

One admitted benchmark holds inference/load/runtime reservations. Cancel, shutdown,
process stop acknowledgement, unchanged-runtime checks and quarantine on
unverified stopped work retain the existing runner paths. Navigation or TUI
disconnection does not cancel a run. There is no retry, reload or competing queue.

## Validation and interpretation

Run `./validate.sh` (format, workspace check, all-target clippy, workspace tests).
No new tests or downloaded benchmarks are required. `benchmarks plan` makes
selection and mathematical ceilings inspectable without inference. Real model
score discrimination, stricter v4 task deadlines and repeatability still need
hardware calibration. Small samples, conservative context admission and missing
process memory telemetry are explicit limitations, not inferred measurements.
