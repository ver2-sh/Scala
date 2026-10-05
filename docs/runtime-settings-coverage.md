# Exact runtime configuration coverage

This is the auditable classification ledger for operator-facing runtime controls. `FIRST_CLASS_CONFIGURABLE`
means a typed, engine-qualified setting with adapter-owned translation and collision detection.
`SCALA_MANAGED` means the process contract owns the value. `NOT_APPLICABLE / UNSUPPORTED_BY_SCALA`
means the option is deliberately unavailable and includes the reason. Native arguments/environment are not
counted as first-class coverage.

## llama.cpp

The adapter probes the installed executable's own `llama-server --help`; every definition is hidden unless
that exact executable advertises its required option and a trustworthy omitted/default value. The
2026-09-04 audit ended at upstream commit `8b4b3558f1459c13e4aa38d5c94d306a00dc6acd`, tree
`a53cf3e02dd97bd5c19a33abbed3c9797c8eb842`. The exact `llama-server --help` was built and inspected;
the final advance during the audit changed CI only, so its option-bearing source and resulting help contract
were unchanged. Generic definitions carry no snapshot-derived runtime scalar;
new nightlies discovered by the provider receive only the settings and defaults their own help/model evidence
proves.

### FIRST_CLASS_CONFIGURABLE

- CPU/execution: `threads`, `threads_batch`, generation/batch CPU masks and ranges, strict placement,
  priorities and polling, `batch_size`, `micro_batch_size`, `performance_timings`, and escape processing.
- Context/KV: `context_length`, `parallel_requests`, prompt tokens to keep, full SWA, Flash Attention,
  K/V types, KV offload/unified/per-slot policy, context checkpoints/minimum spacing, cache RAM/idle slots,
  context shifting, continuous batching, prompt cache/reuse, and warmup.
- RoPE/model loading: scaling method/context scale/frequency base/frequency scale, every current YaRN knob,
  load/lazy/NUMA modes, repacking/host buffers/tensor checks/op offload, GPU layer placement, CPU MoE/dense
  FFN placement, ordered exact-UUID CUDA device set, split mode/tensor split/main GPU, fit policy/target/minimum context, tensor placement
  overrides, model metadata active-expert override, LoRA and control-vector modifiers.
- Sampling/default generation: sampler chain, temperature, top-p/top-k/min-p/top-n-sigma, XTC, typical-p,
  seed, EOS behavior, repeat/presence/frequency penalties and window, DRY and adaptive-p controls, dynamic temperature,
  Mirostat, output limit, stop strings, backend sampling, system prompt, structured output, and overflow.
- Speculation: exact advertised mode, bound draft model identity, draft KV types, draft CPU affinity/thread/
  priority/polling/tensor/MoE controls, draft GPU offload and backend sampling, maximum/minimum draft length,
  split/minimum acceptance probabilities, and all current simple/map-k/map-k4v/modified N-gram controls.
- Prompt/reasoning: built-in or content-bound chat templates, Jinja, template arguments, reasoning policy,
  effort/budget/budget message/format/preservation, chat parsing, assistant prefill, and slot similarity.
- Private-server operations compatible with Scala: timeout, SSE ping, HTTP workers, metrics, slots endpoint,
  slot-save path, idle sleep, logging controls and prompt diagnostics.

### SCALA_MANAGED

- Primary model/source selectors (`--model`, model URL, Docker/Hugging Face selectors and token), alias/public
  model identity, bind host/port/reuse/API prefix, and API keys/TLS are owned by acquisition, routing, private
  transport, and authentication. Raw `--device`/`LLAMA_ARG_DEVICE` are process-contract controls: the typed
  `llama.cpp.devices` setting selects ordered physical UUID identities, then launch constrains
  `CUDA_VISIBLE_DEVICES` to that ordered UUID list and emits the corresponding runtime-local
  `--device CUDA0,CUDA1,...`. With no explicit set, Scala preserves automatic single-compatible-GPU binding.
- `--props` is kept off because mutable backend-global properties bypass resolved settings and provenance.
- Help/version/list/completion/cache-list are probe or one-shot commands, not launch configuration.

### NOT_APPLICABLE / UNSUPPORTED_BY_SCALA

- Embedding, reranking, pooling, multimodal-projector/media/video, router/multi-model, Web UI/static/CORS,
  agent/built-in-tool/MCP, and download presets change the endpoint, trust, or single-model backend contract.
- Removed/deprecated defrag, mmap/mlock/direct-IO, legacy draft and legacy N-gram switches are not reintroduced;
  their current replacements are used.
- Raw grammar/logit-bias and synthetic benchmark switches remain intentionally unavailable as persistent
  defaults: Scala's public request contract does not yet preserve their semantics or file/content identity.

All aliases in the three classifications are enforced at the raw boundary, and llama.cpp raw native
arguments are disabled entirely. At exact-runtime probe time, every ordinary option header advertised by
that executable must occur in the structured or reviewed managed/unsupported inventories; a new upstream
option makes the runtime inadmissible until this ledger and adapter are updated. First-class CLI/environment
aliases are reserved even when their structured setting is omitted. All inherited `LLAMA_*`, `MTMD_*`,
`GGML_*`, `LLGUIDANCE_*`, and `AIP_*` variables are scrubbed dynamically, after which the adapter supplies
only its reviewed values; unrelated ordinary environment is still inherited. Thus `--rpc`, generic
`--override-kv`, device/list-device, multimodal/media, router, UI/tools/MCP, one-shot, legacy-removed, raw
grammar/logit-bias, synthetic controls, and future unclassified controls cannot change the private
single-model process through `engine."llama.cpp".native` or ambient engine environment.

## q27

The 2026-10-05 static audit adds the exact v0.14.3 source contract: annotated tag
`cd3e2c4faa0dc476c49f73eab7736341689a268b`, commit
`a4d5fc4be1231214e25c578eda7ab659a55689e7`, tree
`db65c9363346935f2f12498d2438418e16e12557`. The independently reviewed v0.10.0 contract
(commit `4770e053656af9aababdc49c81f280ad21b74986`, tree
`ff712f78fd17b5fe12149679114b6def003f16a6`) remains admitted. Source recipes and persisted build
provenance bind these exact trees and file digests; no version range or help text grants semantics.
The v0.14.3 source recipe is offered separately alongside its binary archive. The historical v0.6.2
binary grants remain; newer/custom/external binaries do not inherit source-built capability proof.
See [immutable evidence and serving audit](q27-grep-contract.md#v0143-static-runtime-audit-2026-10-05).

### FIRST_CLASS_CONFIGURABLE

- CLI: context/secondary-slot context/slot count, fast head, thinking/request-thinking/budget, constrained
  tools, continuous batching, sampled graphs, KV representation, MTP depth/probability, suffix drafting and
  width, and every disk/RAM prefix-cache limit.
- Reviewed environment: Qwen3.8 trained-template reasoning effort (`q27.reasoning_effort`: `low`,
  `medium`, or `xhigh`), serving profile, forced default sampling, fused batch graphs/capacity/GEMM, KV
  pooling, shared prefill arena, prefill threshold/split-K/kernel/activation group/token tile, draft early
  exit, plain sampling, tool split, GPU interleaving, phase statistics, readiness floor, thinking budget
  fraction, suffix minimum, GEMM threshold, recurrent checkpoint controls, adaptive-depth thresholds,
  FD decode attention, delta-scan mode/split, tool dialect/parser/error/size controls, and bare-system policy.
- Protocol defaults remain `q27.*` identities even though OpenAI request fields are engine-neutral DTOs.

`q27.reasoning_effort` is admitted only for an exact reviewed source contract together with bounded
artifact metadata proving both a recognized Qwen3.8 v2 tier and q27's own normalized `general.name`
`qwen38` trained-template selector. Qwen3.6 and unknown/mismatched fine-tune identities do not receive the
row. The reviewed Qwen3.8 runtime default is `xhigh`; a persisted/Profile/invocation process value is
translated only by the adapter to `Q27_REASONING_EFFORT`. Persistent choices intentionally exclude q27's
legacy `off` A/B arm and internal aliases. Explicit request `minimal`/`low` becomes q27 `low`, `medium`
remains `medium`, and `high`/`xhigh`/`max` becomes q27 `xhigh`; request `none` retains q27's distinct
disable-thinking behavior only when `q27.request_thinking=true`, and is rejected otherwise because the
upstream process would ignore it. Request overrides are ephemeral and never rewrite the process default
or the separate `q27.thinking` setting.

### SCALA_MANAGED

- Positional model/tokenizer, bind host/port, API key, CUDA visibility/device selection, public identity and
  compiled W8/W12/W16 maximum draft width. The separate v0.14.3 `12g` identity selects
  `q27-server-12g`, W_MAX=8, a 256-row prefill arena and **sm86 only**; it is not a W8/W12 alias.
  Compile-time constants/macros are runtime-variant identity, not per-launch settings.
- The `12g` route admits only bounded metadata for Bonsai 2 `bonsai2-t2-v1`/`t2-slim` (12 GB class)
  or `bonsai2-t3-v1`/`t3-slim` (8 GB class), with the documented Hadamard descriptors and consistent
  64-layer plain/65-layer MTP metadata. Qwen 24/32 GB tiers and unknown/non-slim recipes are rejected.
  Bonsai 2 is not admitted to historical/custom builds. These are metadata admission facts, not full
  tensor-inventory validation or benchmark-derived settings. No VRAM upper limit or install-script preset
  is imposed; MTP-dependent settings remain unavailable without a prediction layer.
- Exact v0.14.3 release metadata declares static CUDA 13.2, NVIDIA r580+ and glibc >=2.38.
  Static CUDA linkage is an advisory, **not a host CUDA toolkit dependency**. Driver compatibility is
  checked; glibc remains an explicit unverified prerequisite because shared host observations contain no
  glibc fact. No such ABI/driver/compute-target floors are extrapolated to unreviewed releases. The separate
  source Make recipe needs CUDA 13.2 to compile its mandatory `pf4.o`; local source builds do not inherit
  the release's host glibc floor.

### NOT_APPLICABLE / UNSUPPORTED_BY_SCALA

- q27 runtime catalog entries are Linux CUDA only; Metal-only controls are not shown.
- v0.14.3 has opt-in `--enable-metrics`, but operational metrics configuration is not promoted into
  inference settings in this bounded audit.
- `Q27_DRAFT_VOCAB` and `Q27_DRAFT_VOCAB_CTX` remain unsupported: the exact source requires solo,
  non-DFlash2 execution, `Q27_BATCH=0`, a physical MTP projection and a Q4/Q8/T2 draft head. Bounded
  metadata does not prove the head dtype/inventory or projection, so slot/batch enforcement alone is
  insufficient. Both names remain covered by dynamic environment scrubbing.
- Additional v0.14.3 getters (DFlash2 and its internal D2 controls, `Q27_FIXED_STACK_GB`,
  `Q27_BONSAI_FUSED`, `Q27_KV_INCREMENTAL`, `Q27_MTP_WARM`, `Q27_PF_FOLDLAST`,
  `Q27_T2_PF_SHADOW`, `Q27_DUMP_PF_LOGITS`, `Q27_ECHO_MODEL`, `Q27_REQ_LOG`, `Q27_WAIT_LOG_MS`
  and `Q27_SEED`) are not normal typed serving settings. No install-script memory preset, random seed
  environment policy, diagnostic or internal kernel switch is promoted.
- After subtracting the typed and Scala-managed values above, the exact v0.10.0 CUDA serving runtime's
  remaining environment inventory is deliberately unsupported:
  `Q27_ATTN_PF`, `Q27_BATCH_DBG`, `Q27_DRAFT_CEIL`, `Q27_DRAFT_CEIL1`, `Q27_DRIFT_CORPUS`,
  `Q27_DUMP_HIDDENS`, `Q27_FDMMA_NS`, `Q27_FDMMA_STAGES`, `Q27_GC_RECYCLE`,
  `Q27_GEMM_SPLITK_DBG`, `Q27_KV_SCATTER`, `Q27_MPROBE`, `Q27_NJOINT`, `Q27_P0B_T`,
  `Q27_PF4_INSTRUMENT`, `Q27_PF_ARENA_NODRAIN`, `Q27_PF_CPASYNC`, `Q27_PF_FP8MMA`,
  `Q27_PF_NOSERIAL`, `Q27_PF_NTX`, `Q27_PF_NTX_DBG`, `Q27_PF_PV8`, `Q27_PF_SPLIT`,
  `Q27_PRINT_WSUM`, `Q27_PROF_DECODE`, `Q27_SUFFIX_DBG`, `Q27_SYSBLK`,
  `Q27_TG_REENGAGE`, `Q27_TG_TRACE`, `Q27_VG_CTA_TARGET`, and `Q27_WSUM_LOCATE`. These are diagnostic,
  A/B, corpus, trace/dump/instrumentation, unstable internal lifecycle, or low-level launch-geometry controls;
  the exact q27 schema intentionally does not promote them into normal serving configuration.

q27 raw native arguments are disabled, including the former alternate paths for fast-head, fp16 KV, and
prefix caching. Configured `Q27_*` variables are rejected, every inherited `Q27_*` name is scrubbed
dynamically (including future names), and only values generated from reviewed typed `q27.*` settings are
added back. Scala's independently owned `CUDA_VISIBLE_DEVICES` binding is applied separately. This makes
the pinned reviewed source-runtime environment boundary enforceable without promoting diagnostics into
ordinary settings.

## NInfer

The reviewed/admitted canonical v2 capability revision is commit
`d49296868dcc17bd478ec185f0d3a801bcc0bf56`, tree
`8e2f0275fc533cf11fe05a4ac3ac85f00eb91c72`. Capability domains are separately fingerprinted so future source snapshots
retain only unchanged reviewed domains.

The 2026-10-05 static audit pinned canonical master at
`68c54356fd490ab329bd1475d48957f886bb7dd1`, tree
`a10f0928844093ef6ee9c2e0fa27e69980539e88`, 94 commits after the reviewed baseline.
Of 97 recorded owner blobs, 17 are identical, 2 narrowly contract-preserving changed,
21 semantic changed and 57 removed/restructured. All 17 capability domains remain
ungranted at that head: no complete successor owner closure is proven. Independently,
the native reader is v3-only and explicitly rejects Scala's admitted v2 containers.
The managed Linux catalog therefore remains pinned to the reviewed v2 snapshot;
runtime/container admission fails closed separately from capability grants. The exact
reviewed Windows package and settings precedence remain unchanged. Full path/domain
and format-owner evidence is in [the NInfer audit](ninfer.md#static-canonical-master-audit--2026-10-05).

### FIRST_CLASS_CONFIGURABLE

- Independent logical `ninfer.context_length` (`--max-context`) and physical `ninfer.kv_capacity`
  (`--kv-capacity`), concurrency/pending/timeout/request limits, prefill chunk, statistics interval, KV dtype,
  prefix reuse, device/host state caches and Host KV budget, private/shared continuation/prefix limits.
- Speculative backend, draft tokens, LM-head draft, default output/thinking budgets, thinking and preservation,
  CUDA Graph, Vision/media budgets/threads, response-store limits, CORS, log level, context-cost preset file,
  sampling overrides, seed and greedy mode.
- DFlash remains limited to exact `qwen3.6-35b-a3b`/`groupwise-int` artifacts.
  DFlash2 requires complete native companion tensors on the registered Qwen3.6/3.8
  27B groupwise/NVFP4 targets. MTP 1–5 and DFlash/DFlash2 1–15 are one exclusive
  backend selection; full/optimized proposal heads and Vision are native controls.
  Absence of the draft removes DFlash2 from artifact settings and rejects it at launch.

### SCALA_MANAGED

- Artifact, host/port, API key, public model ID and `--device` are owned by model identity, private transport,
  routing and exact selected-GPU isolation.
- `--request-log-jsonl` is Scala's private authoritative schema-20 startup/request evidence channel. NInfer
  supports only one such sink, so exposing another path would either duplicate the flag or destroy startup
  proof; it is therefore explicitly managed rather than presented as a user log setting.

### NOT_APPLICABLE / UNSUPPORTED_BY_SCALA

- None of the reviewed `ninfer-serve` normal controls are silently unclassified. Build flags and target
  constants belong to the immutable runtime variant rather than a per-run setting.

The capability-domain fingerprints include the target-runtime implementation owners, not only the serving
front end: admission/scheduling and resource search; context cost, KV capacity, materialization, Host/Device
checkpoint state and prefix reuse; target layouts/program/request plans/resource projections/state images;
MTP/DFlash/DFlash2 contexts and schedules; Vision/text residency and prefill; and request/log/protocol owners. Commit
and tree identity remain alongside these focused blob sets. `ninfer.context_cost_presets` is structured-only;
its canonical path and SHA-256 are bound immediately before launch.
