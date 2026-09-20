# Scala agent instructions

Use Scala for the application, CLI, service, crates, API headers, storage and Link.
Norted denotes only the independent model builder and its authoritative artifact
formats/lineage. Never rename builder schemas or historical evidence as branding.

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
- `SettingDefaultSource::Scala` is reserved for execution behavior Scala intentionally owns, never for satisfying presentation requirements.


## NInfer integration boundary

Scala owns native runtime qualification, bounded artifact capabilities,
settings precedence, launch/request behavior and startup observations. Norted owns
model/master lineage, preparation and optional companion assembly/publication.
Bare native artifacts and equivalent manifest-bound artifacts receive identical
inference controls regardless of origin. Schema-7 NInfer manifests bind only
present targets, actual embedded resource hashes and optional draft provenance;
there is no mandatory Sharp sidecar or sidecar-granted inference capability.

DFlash2 admission requires the complete native 66-tensor inventory and qualified
runtime source owners; native dispatch ID alone proves neither draft presence nor
HF lineage. Keep MTP 1–5, DFlash/DFlash2 1–15 and the old DFlash 35B-A3B restriction.
Do not infer benchmark defaults, run inference for development validation, modify
experimental evidence, or activate a production profile/service. No GitHub Actions
or hosted validation is authorized. Use existing local contracts and workspace
checks; keep tests synthetic and cheap. See [native integration](docs/ninfer.md).
