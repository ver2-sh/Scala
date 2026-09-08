# Norted Quick Bench v4

A manual, private benchmark of one Model Profile, using bundled original tasks,
normal managed inference, deterministic oracles, and a hard **600-second maximum**
including preparation, cancellation and finalization. It needs no judge model,
external evaluation service or downloaded benchmark data. Benchmark control remains
private and authenticated; no public OpenAI-compatible benchmark route is added.

## Run and inspect

```console
norted-server benchmarks start PROFILE_ID
norted-server benchmarks plan PROFILE_ID --json
norted-server benchmarks status
norted-server benchmarks cancel
norted-server benchmarks history PROFILE_ID
norted-server benchmarks result RUN_ID
norted-server benchmarks result RUN_ID --verbose
norted-server benchmarks result RUN_ID --json
norted-server benchmarks compare BASELINE_RUN_ID SELECTED_RUN_ID
```

Every run uses the full fixed pack. There is no mode selector, capability
configuration or context ladder. Retrieval is part of the same fixed pack.
On `/benchmarks`, **b Benchmark** starts a run, **h History** opens history,
**d Details** opens technical analysis, Space marks a baseline, **c Compare**
compares, **e Evidence** opens raw evidence, and **x Cancel** cancels.

Default CLI and the selected-result pane show seven independent headline metrics:
Intelligence /100, Agentic /100, Coding /100, Retrieval /100, Output TPS, Prefill TPS, and Latency.
Higher is better except Latency. There is **no overall/composite score**.
Unavailable values are `—`. Status, duration and last benchmark follow the scores.
The responsive table drops secondary metadata first, then Prefill, then latency
and output speed; the selected pane always includes all seven metrics.
Details/`--verbose` include task outcomes, category scores, coverage and missing
reasons, runtime/profile configuration, diagnostics and raw evidence. `--json`
retains the complete machine-readable response.

## Frozen methodology and budget

Suite: **`norted-quick-bench/4`**. Methodology:
**`json-fixture-executable-native-prefill/4`**. Performance summary method:
**`independent-native-prefill/4`**.

| Phase | Ceiling |
|---|---:|
| Preparation, integrity checks and loading | 55 s |
| Unscored warm-up | 9 s |
| Performance probes (2 short, 2 medium) | 4 × 14 s = 56 s |
| Intelligence | 24 × 7 s = 168 s |
| Single-turn tools | 8 × 6 s = 48 s |
| Multi-step Agentic fixtures | 4 × 18 s = 72 s |
| Executable Coding | 6 × (7 s inference + 1 s evaluator) = 48 s |
| Retrieval | 4 × 15 s = 60 s |
| **Declared work** | **516 s** |
| Stop confirmations (50 tasks plus warm-up) | 51 × 1 s = 51 s |
| Execution bookkeeping | 15 s |
| Cleanup/finalization | 16 s |
| Unallocated margin | 2 s |
| **Hard maximum** | **600 s** |

There are **50 scored/performance tasks**, excluding warm-up. Plan construction
checks the entire sum. One monotonic deadline starts at admission. Execution
stops at 584 seconds; cleanup ends by 599 seconds and immutable finalization by
600 seconds. Cancellation confirmation, checkpoints, progress and persistence
retain explicit allowances. No retries, deadline restarts, budget redistribution
or extensions. Unused allowances do not enlarge later tasks. Arbitrarily slow
host storage is not a guarantee of successful finalization; the deadline remains
fixed and unfinished work stays incomplete.

The manifest binds selected IDs, exact prompts, contracts, hidden answer/test
vectors, rubrics, evaluator version and security limits, fixtures, output limits,
and timing policy. Its digest is the pack hash. Hidden oracle state is never
included in model requests; only task prompts/contracts and delivered fixture tool results are sent.

## Intelligence and Agentic

Intelligence retains its 24 exact-JSON questions: six each for logic, context,
**code reasoning**, and instruction following. The four category percentages are
equally weighted; the complete denominator is required. Code reasoning remains
part of Intelligence and is not the executable Coding result.

Agentic retains eight single-turn native-tool cases and four multi-step virtual
fixtures, with equal weight for the single/multi percentages. Tools operate only
on bundled virtual state. Correct tool names/arguments, observed revisions,
dependent reads, conflict recovery and post-update verification are checked
without a judge. Multi-step limits remain six turns/eight calls and 384 output
tokens per turn. Unsupported native tools produce unavailable evidence, not a
fabricated zero or redistributed weight.

## Coding /100

Coding is an **original Norted executable micro-code benchmark**, not HumanEval,
SWE-bench, LiveCodeBench, or a claim about full repository engineering. Six short
Rhai tasks cover clamped prefix balances, duplicate-aware boundary repair,
last-occurrence collection transformation, escaped string fields, explicit
configuration precedence, and interval-union length. Every prompt gives minimal
Rhai syntax and the `fn solve(x)` contract; prior familiarity is not assumed.

**Coding = 100 × passed tasks / 6.** A submission must compile/parse and pass
**every deterministic hidden test** for its task. Otherwise that task receives
zero, including a completed task allowance with no usable response. No partial
credit or subjective grading. A missing/interrupted task or unfinished run cannot
publish Coding. Each task has a 7-second inference deadline and 768-token output
cap, followed by at most one second of bounded local evaluation.

Rubric: **`norted-rhai-all-hidden/1`**; evaluator: **Rhai 1.26.0**, pinned in Cargo.
The evaluator is private to the benchmark. It uses a raw in-process engine with
an explicit arithmetic/logic/string/array/iterator/math package allowlist. It
excludes Rhai's default core package (which includes blocking sleep). No host
functions are registered, no module resolver exists, and module/time/closure/
custom-syntax/floating-point support is disabled at build time. `eval`, output
callbacks and sleep are disabled. Generated source never reaches shell, Python,
Node, rustc/cargo, another OS process, Docker, filesystem, network, environment,
clocks or randomness. Dependencies are normal application build dependencies;
running the benchmark itself is offline.

Limits: 8,192 source bytes, 20,000 operations per hidden case, eight call levels,
16 expression levels, 64 variables, 256 items per array, 32 map items,
and 2,048 bytes per string. Optimization is disabled. Each hidden case
gets a fresh engine/scope; no oracle/expected value is in that scope. Operation,
collection, expression and call bounds terminate hostile programs; a private
elapsed-time callback provides an additional one-second evaluator ceiling.
Rust unwinding failures are contained without shared evaluator state. As with
any in-process dependency, these protections rely on the interpreter's bounds
and memory safety; there is no claim of OS-level fault isolation.

Evidence stores the original response, compile outcome, binary result, bounded
reason, evaluator time/version, request timing/limits and rubric. A failing test
says “hidden test failed”; interpreter internals are not dumped. Inference that
never produces evaluable source records an unavailable compile outcome.

## Retrieval /100

Pack **`norted-private-lease-repository/1`** contains four newly authored queries
against one immutable virtual repository: ten files, 52 lines, including source,
presentation/fixture decoys and retired documentation. Tasks `grep-lease-1` through
`grep-lease-4` cover precise function localization, caller/callee control flow,
configuration-to-use tracing, and export predicates. Gold consists of exact
inclusive file/line ranges; neither gold nor the repository is given to the model
in advance. Tools access only bundled strings, never the host filesystem or a
network. No Grep training examples, teacher trajectories or SWE-bench material
are used. This small retrieval microbenchmark does not establish broad repository
retrieval quality or replace Grep's qualification corpus.

Repository SHA-256 (sorted compact JSON path-to-content map):
`d72dd6d936277c9e70db0057349a0155f83597c622ff1809381f3ac9dc736ebd`.

The system prompt, tool definitions, finalization instruction and final JSON
Schema mirror Norted commit `3098fd5dd62e739c4368ae7a8f97b1353f28f91c`,
`scripts/grep_protocol.py`; generation flow is cross-checked against
`scripts/grep_generation.py`, model ownership against `scripts/grep_model.py`, and
bounds against `config/grep.toml`. The protocol is:

1. System prompt + user query; native `grep`, `glob`, `read`, automatic tool choice,
   parallel calls enabled, explicit text output during search.
2. Accept a valid ranges response immediately, including before any tools, with
   no forced extra turn. Malformed finals end the task as model evidence.
3. Execute at most four rounds of 1–8 independent calls. Preserve call order for
   deterministic results; the read-only in-process calls execute sequentially
   without dependencies or changes to repository state. A round is one assistant
   request, regardless of call count. Calls must have unique nonempty IDs and a
   complete terminal response; mixed prose/tool output is a protocol failure.
4. Only after four **executed** rounds, remove all tools, set tool choice to none,
   disable parallel calls, append the exact finalization user instruction, and
   request one strict `JsonSchema` assistant response. No final-answer pseudo-tool.

Each task has one 15-second ceiling across all turns, local work and intermediate
checkpoints, with no retries or per-turn deadline resets. Every turn caps output
at 1,024 tokens (an explicit lower profile limit still wins as in the existing
benchmark). Saved sampling/thinking settings remain profile-owned; the benchmark
never rewrites them to match training defaults. Canonical Grep messages have no
benchmark nonce; cache isolation remains unverified and this difference from the
performance probes is recorded. The global deadline still bounds the whole run.

`path-lines-json-v1` delivers compact sorted JSON
`{"files":{"path":[[1,"text"]]},"truncated":false}`, with `bounded:true` for
read requests extending beyond EOF or 160 lines. Glob matches have empty arrays.
Angle brackets are escaped to prevent repository text from injecting template
framing. Errors are deterministic JSON. The 2,048-byte round output budget is
shared equally as `(2048 - 2 * calls) / calls`; each result is a stable prefix.
Each call returns at most 64 results. The 1,024-byte round call budget uses
canonical JSON names/argument objects without transport IDs. Regex uses bounded
Rust regex, without PCRE2; glob/include/exclude use case-sensitive fnmatch-style
`*`, `?` and character classes, with `*` spanning `/`. Discovery has no ignore
files in this fixture. No external ripgrep executable or general repository
framework is added. Regex compilation has 1 MiB size/DFA bounds; tool strings are
bounded to 2,048 UTF-8 bytes. These local implementation limits are identity-bound.

The exact upstream schema permits up to 4,096 ranges, forbids extra fields and
requires nonempty paths and positive integers. Separate semantic validation
normalizes safe relative POSIX paths, rejects `.git`/escaping paths, checks
start ≤ end ≤ 100,000,000 and actual file bounds, merges overlapping/adjacent
ranges, and caps the union at 1,024 lines. This follows the distinction between
upstream `FINAL_SCHEMA`, `ranges()` and `RepositoryTools.validate_ranges()`;
ordering and filesystem bounds are not falsely claimed as JSON Schema constraints.

For file and line sets, `P = hits / returned`, `R = hits / gold`, and
`F0.5 = 1.25 P R / (0.25 P + R)` (zero when both are zero). Line metrics use
interval-union counts, so overlapping predictions cannot inflate hits.

**Retrieval = 100 × mean over all four tasks of min(file F0.5, line F0.5).**
Rubric **`grep-bottleneck-f05/1`** uses the weaker granularity without subjective
mixing weights. Pollution already lowers line precision/F0.5; no second arbitrary
pollution penalty is applied. Malformed finals, terminal protocol/generation errors, invalid
completion and timeouts give zero task credit. Recoverable tool error payloads
are returned to the model and do not zero a later valid completion. Ordinary incomplete retrieval can
receive objective partial credit. This is a Server benchmark rubric, not a claim
that Norted qualification uses this headline formula. It was selected before any
Q6/Q6K observations.

Raw evidence retains file and line P/R/F0.5, polluting/returned lines, exact
predicted/gold ranges, `target_ranges_grounded`, `grounded_success`,
`clean_success`, terminal `failure` and its reason, malformed final,
tool protocol failure, call/error counts, executed serial rounds, truncation,
timeouts and retrieval wall time. `grounded_success` means direct tool evidence
overlaps every gold region; it does not require every gold line or a correct final.
`clean_success` requires valid completion, perfect F0.5, grounding and no pollution.
A valid imperfect answer receives objective partial credit and `failure: false`.
Semantic tool errors increment `tool_errors` and `malformed_calls`, remain visible
in tool history, and permit recovery; `recovered_tool_errors` records valid
completion after such errors. Invalid envelopes/serialization, excess calls,
duplicate/empty IDs, forbidden prose with calls, and tools during finalization
remain terminal. Refusal, malformed final, invalid completion, generation errors
and timeouts also mean terminal failure; infrastructure faults still abort runs.
Aggregate `failures` counts terminal failures and `failure_rate` divides by
`observed_tasks` (0–1). Malformed-final and tool-protocol-failure counts and rates
use the same denominator. Clean success and tool error counts remain separate.
Aggregate P/R/F0.5 are task macro means; counts/time are sums. Missing raw
observations stay null. Partial raw observations never supply a headline.

Before each task executes, the running adapter's `serving_features` must prove
ToolCalling and StructuredOutput for the installed runtime, loaded model and
resolved running settings. Static adapter declarations are not the admission gate.
The admitted tuple must also validate the initial request, parallel tool history,
and exact no-tools final request against effective generation settings and the
loaded settings schema. A proven
capability rejection at inference time invalidates the **whole category**, clears
provisional scores and prevents later retrieval tasks, while retaining raw traces.
Unsupported tasks remain in the fixed denominator with null credit, never zero or
redistributed weight. Genuine infrastructure faults retain the existing run-failure
and stop/quarantine paths. Model malformed output is never capability-unavailable.

Currently **none of the bundled adapters proves the whole contract**: q27 and
NInfer reject structured output, and llama.cpp's current adapter does not advertise
native ToolCalling (nor translate tool requests). Thus Retrieval is unavailable
for these adapters; the executor is ready for a faithfully supported adapter.
The exact q27 blocker and qualified template evidence are in
[q27 Grep contract review](q27-grep-contract.md). No weaker fallback is offered.

The v4 suite/method family remains unchanged; the manifest/pack hash changes.
It binds the new repository hash/content, task IDs/queries/gold, prompts, tools,
serialization, round/call/output/semantic limits, finalization, schema, rubric,
timing and execution/aggregation source digests. Conservative source digests also
invalidate comparisons for nonsemantic edits to those owners. Q6/Q6K and future
students share the same denominator and can compare under identical pack identity;
quality/performance retain existing configuration/deployment comparison gates.
Earlier v4 packs and v1–v3 records remain immutable and cannot compare directly to
this pack. No migration or capability-aware format reader is added.

## Output speed, native Prefill and latency

The existing four copy-workload probes independently measure:

- **Output TPS:** native output tokens / whole-request time. This includes hidden
  reasoning when counted by the runtime and is not decode-only TPS.
- **Prefill TPS:** native prompt tokens actually processed × 1000 / native
  prompt-processing milliseconds. Higher is better. Total input, cache reuse,
  processed prompt tokens and processing duration remain distinct evidence.
- **Latency:** managed request start to first nonempty visible output.
- **Visible delivery chars/s:** Unicode characters after the first text chunk /
  first-to-last text delivery span (at least 128 subsequent characters and 50 ms,
  at least 400 total characters, multiple chunks, successful non-refusal output).
- **Text end-to-end chars/s:** visible Unicode characters / whole-request duration.

Prefill is **never prompt tokens / TTFT**, guessed decode overhead, chars/sec,
model size or hardware estimates. Both native observations must exist; tokens
must be positive and duration finite and positive, with plausible token accounting.
Invalid evidence becomes unavailable, never zero. Cache-reused tokens are not
knowingly counted as newly processed tokens.

Native llama.cpp Prefill requires proof for the **exact launched runtime**, not
field-name presence or a reported nightly version. The adapter's small
[`prefill.rs`](../crates/norted-engine-llama-cpp/src/prefill.rs) allowlist pins
b10665 commit **`ca3d5a3e10d53f7ea672cb9b6178faca3e2807bc`** and tree
`ba3e0b166abfd6f481e887e6b8d339bc220ceff9`. The complete immutable revision pins
all semantic owners: `tools/server/server-common.cpp` maps `prompt_n`, `cache_n`
and `prompt_ms`; `server-common.h` defines the counters and duration;
`server-context.cpp` updates processed/cache counts and timestamps; and
`server-task.cpp` places final timings on the OAI usage frame. See the
[reviewed source](https://github.com/ggml-org/llama.cpp/tree/ca3d5a3e10d53f7ea672cb9b6178faca3e2807bc/tools/server).

Currently qualifying identities are:

- Managed-source builds from the canonical Norted source provider whose recorded
  commit/tree exactly match that revision and whose build and installed
  executable hashes agree.
- Official b10665 catalog packages tied to that commit and pinned original GitHub
  asset IDs, names and SHA-256 archive digests: Linux CPU x64/arm64/s390x,
  Linux Vulkan x64/arm64, Windows CPU x64/arm64, Windows Vulkan x64, and Windows
  CUDA 12.4 x64, 13.3 x64, and 13.4 arm64.

Before each launch attempt, the adapter clears any previous endpoint proof and
checks the actual executable hash. Startup activates proof only for the matching
runtime ID and executable hash. Each request snapshots that proof before HTTP;
both streaming and non-streaming parsers gate native prompt timing on it.
The observed `native_prefill_contract` fact is retained separately from inference
setting overrides. Unproved/new/custom runtimes leave both native prompt timing
fields unavailable, show Prefill **—**, and explain “native prefill timing
semantics are unverified for this runtime.” Inference remains available.

No other revision inherits support automatically, even if its fields look the
same; extending the allowlist requires another source review. Configured/external
binaries do not qualify based on version strings. Norted reads co-located final
usage/timing, not progress snapshots, and computes `prompt_n * 1000 / prompt_ms`
itself. Conflicting cache counters invalidate timing. Existing request options
are retained; no runtime or Model Profile setting is changed for this metric.

The supported **q27 routed chat usage** (including its reviewed 0.10.0 reasoning
counter) and **NInfer ChatUsage** contracts expose no native per-request prefill
duration. They leave Prefill unavailable: “native per-request prefill timing
unavailable from this runtime.” No ambiguous logs, process-wide profiling,
reloads or TTFT fallback are used. Future reviewed runtimes can populate the same
optional engine-neutral `InferenceUsage` fields.

Each metric has independent short, medium and combined distributions. A headline
requires **all four successful probes**, including two short and two medium.
Partial coverage has no headline/comparison value; Details reports an explicitly
partial median with n/N, short/medium coverage and per-probe reasons. Raw processed,
cached tokens and duration remain inspectable. Completed runs missing native
Prefill use existing **`completed_unavailable`** status (displayed “Completed”);
other complete scores remain usable. Cancelled/failed/incomplete runs publish no
headline scores.

## Identity, comparison and immutable history

Comparisons show baseline → selected and signed deltas for the seven headline
metrics, followed by method/configuration/runtime/hardware differences. Deltas
require matching suite, methodology, pack and comparison signature; strict
performance deltas additionally require equivalent recorded deployment conditions.
Partial aggregates and unfinished runs never supply performance deltas. No overall
winner is declared. Cache isolation and concurrent host workloads remain unverified.

The configuration key still captures requested versus observed settings, runtime,
artifact/file identity and host facts. Editors describe next-load configuration;
benchmark observations never enter persisted override maps. Runtime defaults and
profile settings are not changed for benchmarking. A Current label requires
verified observed identity. Latest attempt remains separate from the latest
finished result; failed/cancelled attempts do not hide a previous finished run.

**v1/v2/v3 records remain immutable historical evidence**. No migration or rewrite
occurs, and no Coding/Prefill observations are synthesized for them. Their original
Intelligence/Agentic evidence remains intact. They are not directly comparable to
v4. The abandoned intermediate capability-aware v4 format has no compatibility
reader or migration. Existing v1–v3 record readers and private storage are retained.

Records remain in the data directory's `benchmarks` folder, independent of
profiles/settings. UUID terminal JSON files use private permissions, locks,
atomic persistence and fsync; terminal overwrite is rejected. Active checkpoints
preserve evidence. Only the owning server recovers checkpoints as interrupted,
never resumes them. History remains bounded to 4096 cached summaries / 200 returned
per profile; raw records remain inspectable by ID. Existing evidence bounds are
64 KiB per turn and 8 MiB per record (Coding additionally restricts source to 8 KiB).

An admitted run reserves inference/load/runtime operations. Cancellation, shutdown,
stop acknowledgement, pinned-runtime checks and quarantine on unverified stopped
work retain existing ownership paths. Navigation/disconnection does not cancel.

## Validation and interpretation

Run `./validate.sh`: formatting, workspace check, all-target Clippy, workspace tests.
No new application test suite is required. The six hidden oracles are bundled
product benchmark logic. Hand-written solutions and hostile-source probes can
check the evaluator independently without model inference. Real-model score
discrimination and timing calibration still require representative hardware;
small local scores are not broad claims about model capability.
