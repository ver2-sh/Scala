# Agent instructions

## Settings and defaults policy

- Inference precedence is `Runtime default → Settings runtime override → Model-profile override → Load/inference override`.
- Runtime defaults are read-only and belong to the selected version/variant and context. Unknown before startup, unsupported, and missing runtime are distinct states; never invent a scalar.
- Persist only explicit per-engine Settings and profile overrides. Inherit removes a local key, including when a value equals its parent. False, zero and runtime automatic policies are valid explicit values.
- Profile Inherit resolves through Settings; Settings Inherit returns control to the runtime. No shared cross-engine inference parent.
- Load overrides are session-local; request overrides are request-local and win over load values where request-time support exists. Neither is persisted.
- Separate compatibility, feature support, and configuration validation. Invalid settings must not erase runtime identity or schemas; removal remains available.
- Server operational settings are separate from inference settings and provenance.
- Configuration editors show next-load configuration. Running observations are separate facts; preserve requested and observed values structurally without changing their winning source.
- Runtime observations and derived execution facts never enter persisted override maps.
- `SettingDefaultSource::Norted` is reserved for execution behavior Norted intentionally owns, never for satisfying presentation requirements.
