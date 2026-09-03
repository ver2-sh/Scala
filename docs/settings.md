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

Persisted settings use schema 2 with separate `server_settings` and `runtime_defaults` fields. The
obsolete Global schema is rejected; Norted does not read or migrate it.

Common semantics such as context length, temperature, sampling, reasoning, prompts, structured
output, and overflow policy are defined once in engine-neutral code. Each adapter binds the shared
definitions into its own categories and supplies exact-runtime, model-derived, host-derived, or
deliberately Norted-owned concrete values. Shared definitions never imply shared persisted defaults.

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
while retaining the original winning source. `SettingDefaultSource::Norted` is reserved for
execution behavior Norted intentionally owns for a product/runtime reason, not presentation needs.

Clearing a Model Profile override immediately reveals `VALUE (runtime default)`. Boot overrides are
ephemeral and display as `VALUE (boot inference)`; they are not persisted unless the user explicitly
saves the value into a runtime or Model Profile.

Unsupported is distinct from unknown. A setting may be shown as `Unsupported` with an exact reason
when the selected runtime/model does not implement it. A supported setting may not use an ambiguous
placeholder.

## Future bulk editing

An `Apply to all runtimes` feature must be a bulk copy. For example, applying temperature `0.7`
writes independent llama.cpp, q27, and NInfer runtime-default values. The runtimes remain independent;
the feature must never recreate a Global or shared-parent inference layer.
