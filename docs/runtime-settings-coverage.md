# Exact runtime configuration coverage

This is the auditable classification ledger for operator-facing runtime controls. `FIRST_CLASS_CONFIGURABLE`
means a typed, engine-qualified setting with adapter-owned translation and collision detection.
`NORTED_MANAGED` means the process contract owns the value. `NOT_APPLICABLE / UNSUPPORTED_BY_NORTED`
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
- Private-server operations compatible with Norted: timeout, SSE ping, HTTP workers, metrics, slots endpoint,
  slot-save path, idle sleep, logging controls and prompt diagnostics.

### NORTED_MANAGED

- Primary model/source selectors (`--model`, model URL, Docker/Hugging Face selectors and token), alias/public
  model identity, bind host/port/reuse/API prefix, and API keys/TLS are owned by acquisition, routing, private
  transport, and authentication. Raw `--device`/`LLAMA_ARG_DEVICE` are process-contract controls: the typed
  `llama.cpp.devices` setting selects ordered physical UUID identities, then launch constrains
  `CUDA_VISIBLE_DEVICES` to that ordered UUID list and emits the corresponding runtime-local
  `--device CUDA0,CUDA1,...`. With no explicit set, Norted preserves automatic single-compatible-GPU binding.
- `--props` is kept off because mutable backend-global properties bypass resolved settings and provenance.
- Help/version/list/completion/cache-list are probe or one-shot commands, not launch configuration.

### NOT_APPLICABLE / UNSUPPORTED_BY_NORTED

- Embedding, reranking, pooling, multimodal-projector/media/video, router/multi-model, Web UI/static/CORS,
  agent/built-in-tool/MCP, and download presets change the endpoint, trust, or single-model backend contract.
- Removed/deprecated defrag, mmap/mlock/direct-IO, legacy draft and legacy N-gram switches are not reintroduced;
  their current replacements are used.
- Raw grammar/logit-bias and synthetic benchmark switches remain intentionally unavailable as persistent
  defaults: Norted's public request contract does not yet preserve their semantics or file/content identity.

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

The stable serving contract is only v0.10.0 commit
`4770e053656af9aababdc49c81f280ad21b74986`, tree
`ff712f78fd17b5fe12149679114b6def003f16a6`. The 2026-09-04 upstream audit found HEAD
`da8a2bf698a2bf1a893fa0afbcd49979919f40b3`, tree
`3ef39c28e549b3b1cc339e33e3f47e35751d1150`, but no release newer than v0.10.0, so HEAD-only controls are not
claimed for installable runtimes. All controls below are source-contract gated; external or unreviewed
binaries do not receive them.

### FIRST_CLASS_CONFIGURABLE

- CLI: context/secondary-slot context/slot count, fast head, thinking/request-thinking/budget, constrained
  tools, continuous batching, sampled graphs, KV representation, MTP depth/probability, suffix drafting and
  width, and every disk/RAM prefix-cache limit.
- Reviewed environment: serving profile, forced default sampling, fused batch graphs/capacity/GEMM, KV
  pooling, shared prefill arena, prefill threshold/split-K/kernel/activation group/token tile, draft early
  exit, plain sampling, tool split, GPU interleaving, phase statistics, readiness floor, thinking budget
  fraction, suffix minimum, GEMM threshold, recurrent checkpoint controls, adaptive-depth thresholds,
  FD decode attention, delta-scan mode/split, tool dialect/parser/error/size controls, and bare-system policy.
- Protocol defaults remain `q27.*` identities even though OpenAI request fields are engine-neutral DTOs.

### NORTED_MANAGED

- Positional model/tokenizer, bind host/port, API key, CUDA visibility/device selection, public identity and
  compiled W8/W12/W16 maximum draft width. Compile-time constants/macros are runtime-variant identity, not
  per-launch settings.

### NOT_APPLICABLE / UNSUPPORTED_BY_NORTED

- q27 runtime catalog entries are Linux CUDA only; Metal-only controls are not shown.
- Current unreleased q27 HEAD's `--enable-metrics` is not present in the installable/reviewed v0.10.0
  server and is not claimed until an installable revision receives a new source capability contract.
- After subtracting the typed and Norted-managed values above, the exact v0.10.0 CUDA serving runtime's
  remaining environment inventory is deliberately unsupported:
  `Q27_ATTN_PF`, `Q27_BATCH_DBG`, `Q27_DRAFT_CEIL`, `Q27_DRAFT_CEIL1`, `Q27_DRIFT_CORPUS`,
  `Q27_DUMP_HIDDENS`, `Q27_FDMMA_NS`, `Q27_FDMMA_STAGES`, `Q27_GC_RECYCLE`,
  `Q27_GEMM_SPLITK_DBG`, `Q27_KV_SCATTER`, `Q27_MPROBE`, `Q27_NJOINT`, `Q27_P0B_T`,
  `Q27_PF4_INSTRUMENT`, `Q27_PF_ARENA_NODRAIN`, `Q27_PF_CPASYNC`, `Q27_PF_FP8MMA`,
  `Q27_PF_NOSERIAL`, `Q27_PF_NTX`, `Q27_PF_NTX_DBG`, `Q27_PF_PV8`, `Q27_PF_SPLIT`,
  `Q27_PRINT_WSUM`, `Q27_PROF_DECODE`, `Q27_REASONING_EFFORT`, `Q27_SUFFIX_DBG`, `Q27_SYSBLK`,
  `Q27_TG_REENGAGE`, `Q27_TG_TRACE`, `Q27_VG_CTA_TARGET`, and `Q27_WSUM_LOCATE`. These are diagnostic,
  A/B, corpus, trace/dump/instrumentation, unstable internal lifecycle, or low-level launch-geometry controls;
  the exact q27 schema intentionally does not promote them into normal serving configuration.

q27 raw native arguments are disabled, including the former alternate paths for fast-head, fp16 KV, and
prefix caching. Configured `Q27_*` variables are rejected, every inherited `Q27_*` name is scrubbed
dynamically (including future names), and only values generated from reviewed typed `q27.*` settings are
added back. Norted's independently owned `CUDA_VISIBLE_DEVICES` binding is applied separately. This makes
the complete pinned v0.10.0 runtime environment inventory enforceable without promoting diagnostics into
ordinary settings.

## NInfer

The reviewed/current upstream capability revision is commit
`863aa8a5f1e866db74f29f8999b83b4021398dee`, tree
`5368f514bafbcab89ce1272df3a13d9d9af55820`. The prior exact revision
`a140e7ae82a11ed2f370a4d8f2cc16268a3790b8`, tree
`1474697c790de8df18ed07a469f560bb33e8f324`, remains an immutable legacy contract. Capability domains are
separately fingerprinted so future source snapshots retain only unchanged reviewed domains.

### FIRST_CLASS_CONFIGURABLE

- Independent logical `ninfer.context_length` (`--max-context`) and physical `ninfer.kv_capacity`
  (`--kv-capacity`), concurrency/pending/timeout/request limits, prefill chunk, statistics interval, KV dtype,
  prefix reuse, device/host state caches and Host KV budget, private/shared continuation/prefix limits.
- Speculative backend, draft tokens, LM-head draft, default output/thinking budgets, thinking and preservation,
  CUDA Graph, Vision/media budgets/threads, response-store limits, CORS, log level, context-cost preset file,
  sampling overrides, seed and greedy mode.
- DFlash remains limited to exact `qwen3.6-35b-a3b`/`groupwise-int` artifacts. MTP and DFlash are one mutually
  exclusive backend selection. Current revision `863aa8a...` admits DFlash with Vision; the legacy reviewed
  revision retains its DFlash+Vision rejection, and exact runtime/model schemas filter the combination.

### NORTED_MANAGED

- Artifact, host/port, API key, public model ID and `--device` are owned by model identity, private transport,
  routing and exact selected-GPU isolation.
- `--request-log-jsonl` is Norted's private authoritative schema-20 startup/request evidence channel. NInfer
  supports only one such sink, so exposing another path would either duplicate the flag or destroy startup
  proof; it is therefore explicitly managed rather than presented as a user log setting.

### NOT_APPLICABLE / UNSUPPORTED_BY_NORTED

- None of the reviewed `ninfer-serve` normal controls are silently unclassified. Build flags and target
  constants belong to the immutable runtime variant rather than a per-run setting.

The capability-domain fingerprints include the target-runtime implementation owners, not only the serving
front end: admission/scheduling and resource search; context cost, KV capacity, materialization, Host/Device
checkpoint state and prefix reuse; target layouts/program/request plans/resource projections/state images;
MTP/DFlash contexts and schedules; Vision/text residency and prefill; and request/log/protocol owners. Commit
and tree identity remain alongside these focused blob sets. `ninfer.context_cost_presets` is structured-only;
its canonical path and SHA-256 are bound immediately before launch.
