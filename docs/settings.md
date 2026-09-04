# Settings and defaults policy

Norted Server has two separate settings domains.

Server Settings configure the application and control plane, such as download concurrency. They are
server-wide, but they are not inference defaults and never participate in model settings provenance.

Inference settings have exactly three precedence layers:

```text
Runtime default → Model Profile → Boot/invocation
```

The selected exact runtime is the only base. llama.cpp, q27, and NInfer own independent defaults;
one runtime cannot inherit values from another. A runtime's authoritative baseline and persisted
runtime customization are one user-facing `runtime default` layer.

Persisted settings use schema 3 with separate `server_settings` and `runtime_defaults` fields.
Schema 2 is intentionally rejected rather than migrated because Norted is unreleased and its
unqualified inference IDs had ambiguous ownership.

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
shared. An unqualified runtime ID is invalid in runtime defaults, Model Profiles, and invocation
patches.

## Concrete values are mandatory

Every supported effective-setting presentation must put the actual value in the primary value
column and annotate the winning source:

```text
Temperature       1.0      (runtime default)
Temperature       0.7      (model profile)
Context length    200000   (boot inference)
Parallel requests 16       (runtime default)
```

Derivation may appear as secondary detail, for example `derived from host concurrency`. Source and
derivation supplement the value; they never replace it. `runtime/model default`, `runtime-selected`,
`model/thinking-mode default`, `inherited`, `default`, `automatic`, and similar prose must not stand
in for an unknown effective result. A genuine typed policy such as seed `random` or an editable
`auto` mode remains a real value. Showing a value never authorizes inventing or forcing a scalar:
when the runtime intentionally defers a result until model/host/startup facts exist, the pre-startup
effective value is `auto (runtime default)` with constraints in secondary detail. Once authoritative
startup observation resolves that policy, the running effective value becomes the observed result
while retaining the original winning source. Provenance stores a changed pre-start value separately
as `requested_value`, so an explicit `fp8` remains distinguishable from `auto` that startup resolved
to `fp8`; human-readable detail is supplementary. `SettingDefaultSource::Norted` is reserved for
execution behavior Norted intentionally owns for a product/runtime reason, not presentation needs.

Clearing a Model Profile override immediately reveals `VALUE (runtime default)`. Boot overrides are
ephemeral and display as `VALUE (boot inference)`; they are not persisted unless the user explicitly
saves the value into a runtime or Model Profile.

The Model Profile editor always presents current/next-load resolution as its primary value. If the
same backend is already running with a different authoritative observed value, that value appears
separately as running state and does not overwrite the editable configuration. Adapter baselines are
not presented as the value a cleared profile override will inherit; clearing resolves the actual
persisted runtime-default layer.

llama.cpp runtime-default previews require evidence from the exact selected runtime, inspected model
metadata, an exact immutable reviewed contract, or a Norted-owned value that Norted actually applies.
Common but unversioned upstream defaults are not treated as authoritative; when exact evidence cannot
establish an omitted default, schema resolution reports that absence instead of inventing a value.

Each runtime/model schema contains only settings configurable for that exact combination. Reusable
definition constructors do not create shared setting identity. A stale or
incompatible stored override is reported as unavailable for the selected schema, but it is not shown
as an ordinary editable `Unsupported` row. An exposed setting may not use an ambiguous placeholder.

## Future bulk editing

An `Apply to all runtimes` feature must be a bulk copy. For example, applying temperature `0.7`
writes independent llama.cpp, q27, and NInfer runtime-default values. The runtimes remain independent;
the feature must never recreate a Global or shared-parent inference layer.

The exact upstream coverage audit and option classifications are maintained in
[`runtime-settings-coverage.md`](runtime-settings-coverage.md).
