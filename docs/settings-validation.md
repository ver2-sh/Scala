# Runtime settings validation (2026-09-06)

Implementation branch: `fix/runtime-settings-inheritance`. All changes are in Scala.
No test code, state migration, model transformation, artifact modification, or cross-repository framework was added.

## Reproduced regression

An isolated copy of the installed runtime store and configuration was used, with separate XDG data,
config, cache, state, and serving port. The user's serving stack and persisted preferences were not changed.

The pre-change release binary, inspecting profile `qwen38-27b-q27-q6` with only `q27.top_k=20`, failed:

```text
runtime selection failed: no compatible installed runtime is available ...
q27-0-10-0-linux-x86-64-cuda-w12-998d20fd9c973ba9:
q27 seed/top-k/min-p request defaults require a positive temperature default;
startup evidence cannot be collected because pre-launch admission failed
```

Inspection established three connected defects: q27 rejected inactive sampling controls at greedy
temperature; runtime selection treated that configuration error as incompatibility; and settings
schema filtering/validation removed fields or whole schemas. The installed q27 source's
`src/server.cu` parses sampling controls inside its positive-temperature branch and uses process
sampler defaults when request fields are omitted. Greedy operation does not invalidate a saved top-k.

The updated binary and TUI accepted `top_k=20` and retained the installed runtime identity and
unrelated settings. `temperature=0.8` and `min_p=0` were also exercised as ordinary overrides.
A separate blanket q27 schema gate requiring the advanced serving-environment contract was removed;
individual runtime feature checks remain authoritative, including historical runtime capabilities.

## Manual checks

| Area | Observed result |
| --- | --- |
| Settings/profile precedence | Settings top-k 40 + profile 20 resolved to 20; profile Inherit resolved to 40; changing Settings to 60 resolved to 60; Settings Inherit resolved to q27 runtime 0. |
| Sparse persistence | After Settings Inherit, the q27 engine override map was absent. Profile clearing removed keys rather than copying parent values. |
| False/zero | Explicit `q27.thinking=false`, `q27.top_k=0`, and `q27.min_p=0` persisted and retained profile sources. |
| Equal-to-parent override | Profile top-k 20 remained explicit after changing its equal Settings parent from 20 to 60. Unrelated false/zero overrides remained unchanged. |
| Other engines | llama.cpp Settings top-k 40 was inherited after profile removal. NInfer Settings 20, profile 10, then profile Inherit resolved to 20. NInfer rejected 40 with its actual maximum-20 diagnostic. |
| Stale-key recovery | An isolated, manually introduced `q27.obsolete_option` was removable through CLI Unset without a registered definition. |
| Reopen | Separate CLI processes and TUI reopening retained explicit values and source labels. |
| TUI | Settings and Model Profiles were exercised at 80×24, 120×32, and 160×40. Mouse navigation/search, keyboard search, overrides filtering, edit/cancel, scoped reset confirmation, selected details, and profile refresh were exercised. |
| Reset scope | TUI reset removed the selected q27 profile's overrides and revealed Settings top-k 60; engine Settings and other profiles remained intact. A subsequent TUI top-k 20 edit saved and retained runtime identity and selection. |

The small-terminal exercise found and corrected a layout defect that left no room for setting rows.
Small layouts now retain editable rows and provide an expandable detail view; normal and wide layouts
use a profile/scope sidebar, with details below or beside the rows.

## Execution-path verification and limits

The existing resolver still performs sparse, per-setting merges. Settings values now carry their
own `settings_override` source. Manager load/autoload and CLI/TUI validation consume this resolver.
Load snapshots remain session-owned; inference preparation mutates only the individual request.
Request top-k overrides win because request preparation fills only absent values, and q27's payload
builder uses the explicit request before the resolved session value. An omitted following request
therefore returns to the session value. No request path writes Settings or profile stores.

Existing tests exercise adapter launch arguments, environment ownership/conflicts, q27 and llama.cpp
request-default precedence, NInfer sampler omission, sparse resolution, model/template identity,
and startup provenance. q27 request payloads no longer manufacture temperature 0/top-p 1 on omission.
Explicit force-temperature/top-p policies are reflected in generation/default observations and
conflicting default controls are rejected explicitly.

**Live load/inference acceptance remains unperformed.** The installed RTX 5090 had 1,440 MiB free
while the user's existing workload was resident. No workload was unloaded to make room. Consequently,
the exact live `request 10 → next request 20` sequence, new backend startup observations, and concurrent
live session isolation were not exercised. Code tracing and existing adapter tests are evidence for
the execution paths, not a claim of GPU runtime execution or a completed live acceptance matrix.

## Repository checks

- `cargo fmt --all --check`: passed.
- `cargo check --workspace --all-targets`: passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: passed.
- `cargo test --workspace`: 214 passed, 0 failed, 1 existing ignored test.
- `cargo build -p scala`: passed.
- `git diff --check`: passed.

The state format remains unchanged. `settings.json` schema 3 retains the `runtime_defaults` field
name; its contents are explicit, independent Settings overrides. No recovery/reset is required for
existing schema-3 state. The resolved provenance source adds `settings_override` so consumers can
distinguish user configuration from read-only runtime baselines.
