# Settings and defaults policy

Norted Server has two separate settings domains.

Server Settings configure the application and control plane, such as download concurrency. They are
server-wide, but they are not inference defaults and never participate in model settings provenance.

Inference settings have exactly four precedence layers:

```text
Runtime default → Settings runtime override → Model-profile override → Load/inference override
```

Runtime defaults are read-only, version/variant/context-specific baselines. Settings stores independent
explicit overrides for llama.cpp, q27 and NInfer. No engine inherits another engine's values.
Profiles inherit from their engine's Settings, and store only their own explicit overrides.
Load overrides last for the session; request overrides last for one request and win over load values
where request-time capability exists. Neither is persisted or changes an already-loaded process-only setting.

Persisted settings retain schema 3 and the existing `server_settings` and `runtime_defaults` fields.
The `runtime_defaults` map contains **Settings overrides**, not upstream defaults. Its existing name
and state are retained without a migration or reset. Resolved provenance labels these values
`settings_override`; true baselines retain `runtime_default`.

Inheritance is absence: Inherit removes a key. Explicit false, zero, automatic policies, and values
equal to the parent remain explicit until removed. Each setting resolves independently.

Every setting identity belongs to exactly one domain:

```text
Server Settings            server.*
llama.cpp Runtime Settings llama.cpp.*
q27 Runtime Settings       q27.*
NInfer Runtime Settings    ninfer.*
```

Context length, temperature, sampling, reasoning, prompts, structured output, overflow policy, and
other same-named human concepts are independent settings. Engine-neutral Rust constructors may
share type/label shape, but no ID, persisted value, default, inheritance, or semantic ownership is
shared. An unqualified runtime ID is invalid in Settings overrides, Model Profiles, and invocation
patches.

## Values, sources and editing

Rows show effective values and Inherited/Overridden state. Unknown before startup is shown as
`Default not yet known`; it is distinct from unsupported and no runtime installed/selected. Genuine
runtime policies such as `auto` and `random` remain values. Never manufacture a scalar or an `auto`
policy just to populate the display. Default-source annotations distinguish runtime, model-dependent,
derived, startup, and server-owned execution policy evidence.

Settings Inherit removes a Settings override and returns control to the runtime. Profile Inherit
removes only the profile override and reveals Settings, which may itself inherit from the runtime.
The selected-setting pane shows the read-only runtime baseline, immediate parent, local value,
effective source, description, constraints and field diagnostics. Running observations are separate
from next-load configuration, and changed startup values retain `requested_value` in provenance.

Both editors support `/` search, `o` overrides-only, Enter to edit, Escape to cancel, Delete to
inherit, and `R` to reset all overrides in the selected scope after typing `RESET`. Search, filter,
reset, value and Inherit actions also support mouse clicks. Use `i` to expand/close details and `[` and `]` (or the mouse wheel over details) to scroll.
Opening any value editor, including a toggle or choice, does not write an override. Wide terminals
use a scope/profile sidebar and a separate detail column; narrower terminals put details below rows.
No persistent load/request override editor exists.

Runtime/model/host compatibility is evaluated before configuration validation. An invalid field
keeps the selected runtime and schema visible. Unsupported and obsolete keys can be removed without
a successful configuration validation. Exact adapter validation still blocks unsupported execution,
invalid ranges, conflicting options and unproved model/template capabilities at load.

llama.cpp runtime-default previews require evidence from the exact selected runtime, inspected model
metadata, an exact immutable reviewed contract, or a Norted-owned value that Norted actually applies.
Common but unversioned upstream defaults are not treated as authoritative; when exact evidence cannot
establish an omitted default, schema resolution reports that absence instead of inventing a value.
Generic llama.cpp definitions therefore carry type/category/semantic information but no runtime scalar
copied from an audited snapshot. Exact executable help supplies advertised defaults, inspected GGUF metadata
supplies model-derived values, and genuinely Norted-owned omission policies remain explicit. Exact-help
availability gating is applied before a definition can enter the concrete runtime/model schema.

Inference-affecting file inputs are bound again immediately before launch. LoRA adapters, control vectors,
and NInfer context-cost presets record the canonical launch path and SHA-256 in normalized launch provenance;
scaled LoRA/control-vector identities also retain their numeric scale. This makes changed content at the same
configured path distinguishable. Mutable output/state destinations, including ordinary log and slot-save
paths, are not hashed as if they were immutable inputs.

## Ordered accelerator binding

The common runtime-selection and launch contract carries an ordered accelerator binding, not one optional
device. Each entry is the complete observed `AcceleratorDevice` fact, including its stable physical UUID,
name, VRAM, driver, and compute capability when available; order is part of provenance.

`llama.cpp.devices` is an ordered list of exact NVIDIA `GPU-...` UUIDs. An explicit list must be nonempty,
duplicate-free, currently visible, and compatible with the selected CUDA runtime. Norted preserves the list
order in `CUDA_VISIBLE_DEVICES`, then translates it to llama.cpp's post-isolation local names
`CUDA0,CUDA1,...`. Host numeric GPU indexes are never persisted or correlated. If the setting is omitted,
the effective policy is the concrete `auto` policy: CUDA retains the existing automatic selection of one
compatible GPU, while non-CUDA runtimes retain their ordinary automatic device behavior. `main_gpu` is validated as an index into this ordered set; tensor-split and fit-target lists
are checked against the selected arity while preserving upstream zero/default, broadcast, and partial-list
behavior. `split_mode=none` remains the upstream policy rather than a Norted single-device rewrite.

q27 and NInfer deliberately require bindings of exactly one device. A plural common representation does not
grant either adapter multi-GPU behavior. Running status and launch provenance expose the complete binding,
so load/unload and residency state retain the physical devices associated with each backend.

Raw llama.cpp and q27 native arguments are disabled. Engine-owned ambient controls are fail-closed as well:
llama.cpp dynamically removes inherited llama/MTMD/GGML/LLGuidance/AIP variables, and q27 removes every
inherited `Q27_*` variable, before each adapter adds back only values produced by its typed settings and
Norted-owned process contract. Unrelated process environment continues to be inherited.

Each runtime/model schema contains only settings configurable for that exact combination. Reusable
definition constructors do not create shared setting identity. A stale or
incompatible stored override is reported as unavailable for the selected schema, but it is not shown
as an ordinary editable `Unsupported` row. An exposed setting may not use an ambiguous placeholder.

For the exact reviewed q27 v0.10.0 source contract, `q27.reasoning_effort` is a persistent process
default only when bounded artifact metadata proves a recognized Qwen3.8 v2 tier and the normalized
`general.name` selector used by q27 itself identifies the Qwen3.8 trained template. Its choices are
`low`, `medium`, and `xhigh`, with the actual Qwen3.8 runtime default shown as `xhigh`; Qwen3.6 and
unproven fine-tunes have no row. The adapter translates configured values to a scrubbed, adapter-owned
`Q27_REASONING_EFFORT`. Explicit request effort is separate and ephemeral: `minimal`/`low` maps to
`low`, `medium` to `medium`, `high`/`xhigh`/`max` to `xhigh`, and `none` disables thinking according
to q27 request semantics when `q27.request_thinking=true`; otherwise `none` is rejected because the
upstream process would ignore that engine-level disable. It does not mutate or masquerade as the
process default.

## Future bulk editing

An `Apply to all runtimes` feature must be a bulk copy. For example, applying temperature `0.7`
writes independent llama.cpp, q27, and NInfer runtime-default values. The runtimes remain independent;
the feature must never recreate a Global or shared-parent inference layer.

The exact upstream coverage audit and option classifications are maintained in
[`runtime-settings-coverage.md`](runtime-settings-coverage.md).

For example, set `q27.top_k=40` in Settings and `q27.top_k=20` on a profile.
A request with `top_k: 10` uses 10; the next omitted request returns to 20. Profile Inherit
reveals 40; Settings Inherit leaves the field to q27. Sampler controls can remain configured
while temperature is zero: they are inactive during greedy decoding, not incompatible.
NInfer has its own top-k bounds (the reviewed runtime accepts at most 20); engine support
and ranges are never copied from q27 or llama.cpp.

Exact numeric terminal controls and mandatory artifact semantics are described in
[Stop token IDs](stop-token-ids.md). Text stop strings remain independent.
