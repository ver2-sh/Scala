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
configuration, retrieval pack, or context ladder.
On `/benchmarks`, **b Benchmark** starts a run, **h History** opens history,
**d Details** opens technical analysis, Space marks a baseline, **c Compare**
compares, **e Evidence** opens raw evidence, and **x Cancel** cancels.

Default CLI and the selected-result pane show six independent headline metrics:
Intelligence /100, Agentic /100, Coding /100, Output TPS, Prefill TPS, and Latency.
Higher is better except Latency. There is **no overall/composite score**.
Unavailable values are `—`. Status, duration and last benchmark follow the scores.
The responsive table drops secondary metadata first, then Prefill, then latency
and output speed; the selected pane always includes all six metrics.
Details/`--verbose` include task outcomes, category scores, coverage and missing
reasons, runtime/profile configuration, diagnostics and raw evidence. `--json`
retains the complete machine-readable response.

## Frozen methodology and budget

Suite: **`norted-quick-bench/4`**. Methodology:
**`json-fixture-executable-native-prefill/4`**. Performance summary method:
**`independent-native-prefill/4`**.

| Phase | Ceiling |
|---|---:|
| Preparation, integrity checks and loading | 60 s |
| Unscored warm-up | 10 s |
| Performance probes (2 short, 2 medium) | 4 × 16 s = 64 s |
| Intelligence | 24 × 8 s = 192 s |
| Single-turn tools | 8 × 7 s = 56 s |
| Multi-step Agentic fixtures | 4 × 21 s = 84 s |
| Executable Coding | 6 × (8 s inference + 1 s evaluator) = 54 s |
| **Declared work** | **520 s** |
| Stop confirmations (46 tasks plus warm-up) | 47 × 1 s = 47 s |
| Execution bookkeeping | 15 s |
| Cleanup/finalization | 16 s |
| Unallocated margin | 2 s |
| **Hard maximum** | **600 s** |

There are **46 scored/performance tasks**, excluding warm-up. Plan construction
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
included in model requests; only the coding prompt and contract are sent.

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
publish Coding. Each task has an 8-second inference deadline and 768-token output
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

Comparisons show baseline → selected and signed deltas for the six headline
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
