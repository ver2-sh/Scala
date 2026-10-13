# Historical Decision source evidence

These dated observations describe the former native_decision integration.
They are provenance, not current configuration or an activation procedure.
Use [vLLM and PyTorch engines](python-engines.md) for installation and migration.

## Norted static qualification, 2026-10-10

The original downloads remain in `/srv/norted/models/decision-sources/`.
Descriptors were prepared under `/srv/norted/scratch/native-decision-integration/`:

| Requested auxiliary profile | Descriptor | Native snapshot | Status |
| --- | --- | --- | --- |
| `h2o-lightning-4b-v1-1-decision` | `h2o-lightning-4b-v1-1.decisionbundle` | `h2oai/h2o-lightning-4b @ 193ad740925b176a3b70a5a13a7cff2f2fadd01e` | **Blocked; not registered** |
| `imajev-4b-decision` | `imajev-4b.decisionbundle` | `mohit67890/imajev-4b @ f8d8234cebc6c99065c07731e59716dc0a6e27ab` | **Blocked; not registered** |

Imajev's base is exactly `Qwen/Qwen3.5-4B @
851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a`. Its adapter metadata records a
training-machine HF snapshot path; the repository/revision encoded there match
this base. No upstream metadata was rewritten. Static readout header inspection
confirmed `weight`, float32, shape `[256, 2560]`.

The host reports an RTX 5090, 32,607 MiB VRAM, NVIDIA driver 595.91.07. System
Python has no vLLM, torch, PEFT, transformers, safetensors, FastAPI or uvicorn
installation. The checked Norted and Swift build virtual environments likewise
have no native serving dependency sets. No qualified external runtime or complete
Imajev runtime source checkout was installed/configured. Existing production Scala
was not replaced or restarted to activate this source change. Both profiles are
therefore unregistered rather than unusable placeholders.

GPU support, available VRAM alongside Swift, native backend startup and operational
model qualification remain unverified. H2O upstream measures H100; that evidence
does not qualify this RTX 5090. There was no benchmark, training, real inference or
model loading for development validation. Runtime provisioning and activation of
this source change are separate operational work; this task changed no production
serving state, Swift profile, runtime selection or Unsloth Studio state.

Coded's existing `decision_candidate` discovery, identity-only selection and native
Decision tool accept the profile/API shapes and preserve extra native observations.
Its existing synthetic discovery and fusion Decision suites pass unchanged; no
Coded contract change was needed.

Sources: [pinned H2O release](https://huggingface.co/h2oai/h2o-lightning-4b/tree/193ad740925b176a3b70a5a13a7cff2f2fadd01e),
[pinned Imajev adapter](https://huggingface.co/mohit67890/imajev-4b/tree/f8d8234cebc6c99065c07731e59716dc0a6e27ab),
[pinned base](https://huggingface.co/Qwen/Qwen3.5-4B/tree/851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a).

Final local validation: `cargo test --workspace --lib --bins --offline` passed
**360 tests**, with the existing manual hash-throughput benchmark left ignored.
The native engine's six tests exercise both backend tuples, plural external
runtime discovery, changed/unsupported identities, native usage and response
preservation, and runtime identity retention under invalid inference configuration. Seven Python wrapper tests use mocked tensor/native servers; the
pinned H2O shim's ten tests use its fake vLLM implementation. Coded's existing
discovery and fusion Decision suites passed **151 tests**. Workspace checking,
formatting, diff checks and native-engine Clippy with warnings denied passed.
Scala's read-only inspector resolved both staged descriptors as real source
artifacts and reported neither execution-qualified. Scratch reports/logs are in
`/srv/norted/scratch/native-decision-integration/qualification-report.json` and its
sibling validation files. The production profile store's before/after SHA-256 is
`49a6b7f253161d8d410e81a7476fc00025dae4fac034dcfc3d9b1e885da1c8e4`.

## Audit follow-up staging, 2026-10-10

Stable staging is `/srv/norted/runtimes/native-decision/staging-2026-10-10/`;
`evidence/qualification-report.json` records the exact wheels, source identities,
descriptor bindings, runtime resolution, validation and remaining release work.
Both descriptors retain the original verified checkpoints. Moving a descriptor
changes its path-derived artifact ID; use the staged profile bindings, not the
earlier scratch IDs.

Imajev's isolated Python 3.12 runtime passes the complete RECORD/source probe and
resolves to `torch-readout`. Its options are CUDA, four rotations and 4,096 input
tokens, with the original calibration and unmerged adapter/readout. A wrapper
fix recognizes case-insensitive wheel names such as `pillow`; RECORD verification
and missing-dependency rejection remain enforced.

H2O provisioning is blocked: stock vLLM 0.30.0 and the pinned README's
`0.30.0+cu129` build require `PyNvVideoCodec==2.0.4`. SHA-verified original Linux
x86-64 wheels for the installed Python versions 3.10, 3.12, 3.13 and 3.14 contain
two native libraries with incorrect RECORD hashes. No qualified H2O launcher or
profile is installed. The separate DLPack wheel site inside the attempted H2O
environment preserves original wheel contents and resolves its shared
`build_backend.py` collision; it does not bypass the PyNvVideoCodec failure.

The exact auxiliary profile bindings are detached staging files. Only Imajev has
an installed-runtime binding; H2O's binding is explicitly blocked. Neither has
execution proof or production registration. Read-only checks reject cross-variant
fallback. Coded's Decision deadline now covers Scala's 300-second startup plus
600-second native request allowance, retaining caller cancellation and fail-closed
handling. Targeted synthetic/static checks perform no model loading or inference.

The RTX 5090 reports only 1,307 MiB free with Swift resident. Original tensors alone
occupy about 8,022 MiB for H2O and 8,888 MiB for Imajev's base, plus its adapter,
readout and runtime/context overhead. Neither can coexist with the current Swift
allocation. No service, live store, Swift configuration or Unsloth setting changed.

## Pinned wheel archive attestation for defective vendor RECORDs

Some otherwise unmodified upstream binary wheels ship with bad internal RECORD
hashes. NVIDIA PyNvVideoCodec **2.0.4** for CPython 3.12 Linux x86-64 has two
such native members; its original archive SHA-256 matches the published PyPI
checksum, and both mismatches are reproducible directly within that archive.
The same packaging defect occurs in PyNvVideoCodec 2.0.5, so simply upgrading
the dependency would not fix the verifier and would violate vLLM 0.30.0's
explicit 2.0.4 requirement.

The opt-in `prepare.py runtime --wheel-attestation PACKAGE WHEEL SHA256
MISMATCHES_JSON` binds the **original archive bytes**, the full archive digest
and the exact actual SHA-256 of each named member to an immutable runtime
fingerprint. Only those enumerated members may disagree with their original
RECORD: the wrapper requires the expected broken RECORD entry, verifies the
matching original ZIP member bytes, hashes the installed bytes, and fails on
additional, missing or unnecessary exceptions. All other installed files still
require valid RECORD hashes. There are no patched packages, rewritten wheel
metadata, upstream version substitutions or global trust exemptions. Other
engine/runtime implementations are unaffected.

The explicit H2O wheel attestation installed on Norted is:
- PyPI wheel: `pynvvideocodec-2.0.4-cp312-cp312-manylinux_2_28_x86_64.whl`
- Original archive SHA-256: `b59cec7a1a3f78fad13fead78cad8b6d9686827f9ff4477080245457675a01d0`
- `PyNvVideoCodec_121.cpython-312-x86_64-linux-gnu.so`: actual SHA-256
  `2fb85f8bcd33c13e240ef2a8c6277f4d5a0260b629ecf9a242a04f1403f582a8`
- `PyNvVideoCodec_130.cpython-312-x86_64-linux-gnu.so`: actual SHA-256
  `14f12a7977c2f681fb01693e41434308bfb5cf0e2c31ed2c29d1176337c86462`

The stable `h2o-runtime/native-decision-server --scala-probe` now passes with
the complete original vLLM 0.30.0 dependency closure. Scala's independent
runtime/model resolver selects H2O as `vllm-labels` and Imajev as
`torch-readout`; both detached auxiliary Model Profile bindings are retained
in `evidence/`. Neither is execution-qualified or registered with the live
Scala 0.1.13 service. With Swift 1.5 occupying approximately 30.8 GiB VRAM,
H2O still needs a separate available-GPU operating window for real model load,
typed-decision smoke tests and service activation. The vendor RECORD defect is
no longer the provisioning blocker.
