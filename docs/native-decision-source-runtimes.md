# Native Decision source runtimes

The `native_decision` engine supports explicit `.decisionbundle` source closures
through the `scala-native-decision-v1` runtime protocol. It uses the existing
Model Profile, runtime selection, authenticated `/v1/systemone`, auxiliary JIT,
request lease and Decision qualification paths. It is registered alongside
llama.cpp, q27 and NInfer; their artifact and native serving contracts are unchanged.

Configuration alone cannot make these Hugging Face snapshots executable in the
three older engines. They are neither GGUF Decision artifacts nor Q27/NInfer
containers. The new source-bundle format is a small local descriptor referencing
canonical original files, not a conversion, copied checkpoint, Norted package,
engine, runtime, Settings store or Model Profile. Norted manifest schemas are
unchanged, and this format has no Norted package manifest.

## Algorithms and qualification

Two backend contracts are implemented, independent of repository/profile names:

- `vllm-labels`: stock vLLM **0.30.0** (or the explicitly pinned Windows
  compatibility build described below) plus an installed upstream label
  shim. The original model configuration must retain `head_dtype: float32`.
  The native shim owns trained question rendering, exact `logprob_token_ids`
  reads for **every** label (including chunking beyond vLLM's limit), temperature,
  yes/no commitment floor, probabilities, confidence, ordinal expectation and
  legends. Scala does not generate a prompt, parse output text or estimate missing
  probabilities. The shim is implementation code bound into the **runtime**
  identity. A model's bound shim digest must match that installed implementation;
  arbitrary model-supplied code cannot acquire capability by calling itself a shim.
- `torch-readout`: a pinned native source checkout calls PyTorch/PEFT with the
  original adapter and exact local base checkpoint, unmerged LoRA, no graph
  warm-up, and the original finite float32 linear readout/tokenizer binding.
  Calibration is applied by upstream `TemperatureCalibrator`; rotation count
  and context/device policy are explicit immutable runtime options. Missing
  adapter/readout, vocabulary-row fallback, changed tokenizer binding and base
  mismatch are rejected. No native thought/reasoning path is enabled. Original
  native `unknown_probability` and `abstained` fields are retained as optional
  observations on each answer. Missing observations stay absent.

The bundle binds repository IDs, exact 40-character revisions, canonical source
roots, each consumed file's size/SHA-256, and calibration/serving file selection.
Metadata discovery is bounded; it checks required role/file bindings and current
sizes without reading tensor payloads. It grants **no** execution capability.
Before a runtime can be discovered, the wrapper's metadata-only probe verifies
its implementation source and the fully locked installed wheel closure against
RECORD, without importing tensor libraries or touching a GPU. The runtime
fingerprint includes the interpreter, wrapper, explicit options, implementation
code and actual dependency closure. A changed closure creates a different
immutable runtime identity or fails the probe. External variants are discovered
independently; a failed variant does not make another variant unavailable.

Ordinary inspection reuses the shared engine-neutral verified observations
described in [native Decision discovery](native-decision.md). For existing
`prepare.py` absolute-interpreter launchers, a small isolated, bytecode-disabled
metadata helper inventories the source, interpreter, installed distribution
files/metadata, parent directories and attested wheel archives. Subsequent warm
reads only stat those paths; they do not rerun Python or hash the wheel closure.
File replacement, removal, permission changes and directory additions invalidate
the observation. Unknown launcher shapes or failed metadata inspection remain
uncached. No installed wrapper or runtime fingerprint is changed by this mechanism.
The full original `--scala-probe` and pre-launch checks remain authoritative.

Before each launch Scala clears old proof and verifies the exact runtime,
executable SHA-256 and bundle digest. The wrapper independently repeats runtime
verification and hashes the complete artifact closure before loading. The
private health response must bind the runtime fingerprint, backend, verified
bundle digest and fresh launch nonce, and attest that the native readout is
initialized. Its native System One validator must return the exact state-required
error for `{}`. This validation performs **no decision forward pass**. A mismatch,
failed health observation, failed launch, unload or crash invalidates proof.
The shared `native_decision_supported` gate controls execution and advertising.
The public `decision_candidate` field requires a compatible installed variant,
complete bundle, matching implementation binding and valid resolved configuration.
It remains only an opportunity for JIT qualification.

Both backend handlers receive only the original `state` and typed `questions`;
Scala never sends a model selector to a loaded private backend. Native limits and
missing instructions are rejected by upstream rather than reconstructed. Native
answers must match the requested names/types/candidate inventory, with finite
in-range observations. Scala preserves supplied values rather than normalizing
or recalibrating them. Native token usage is mapped only when both input and
output counts are present; Imajev's current native response lacks an output count,
so Scala omits public usage rather than inventing a zero.

These runtimes advertise Decision only, without chat, Responses, completions,
streaming or embedding support. Generation defaults are optional observations in
the manager; a Decision-only backend neither queries nor persists fictional
sampling defaults. Existing adapters retain their existing observations. The new
runtime opts into POSIX process groups or native Windows Job Objects, including
cleanup of workers on parent failure and immediate termination; existing adapters
keep their previous launch policy. Windows children start suspended, enter a
private non-inheritable kill-on-close job, then resume. Probe redirector children
also belong to owned jobs, so timeout or cancellation cannot strand them.

On Windows the entrypoint is the dedicated virtual environment's real
`Scripts/python.exe`, with an adjacent bounded `scala-native-decision.json`:

```json
{"protocol":"scala-native-decision-v1","interpreter":"D:\\Runtimes\\env\\Scripts\\python.exe","runner":"D:\\Runtimes\\runtime\\server.py"}
```

Scala checks canonical interpreter equality and launches structured `-I`/wrapper
arguments directly. No shell or POSIX compatibility layer is involved. Equivalent
extended-length drive/UNC spellings are accepted without accepting source aliases,
traversal or symlinks. Short executable paths use ordinary Windows spelling;
actually long executable paths retain device syntax. Spaces and Unicode are
preserved. Runtime execution clears the inherited environment and retains only
the validated Windows `SystemRoot` needed by Winsock. The identity additionally
binds the launcher descriptor, venv configuration, base interpreter DLL/stdlib
closure, root-level Python/path import controls, and the existing complete
wrapper/source/dependency/options closure.

Authoritative Windows verification allows 120 seconds for cold DLL reads;
POSIX retains 30 seconds. Public cold discovery retains its shared eight-second
bound with independent per-variant deadlines. Warm observation reads metadata
only, including Windows file attributes, file ID, change time and dependency topology; it does not
rehash dependencies or initialize Python/CUDA on every model listing. Slow or
failed cold verification grants no candidate or execution proof. Windows retains
one owned metadata verification task per entrypoint across bounded waits; a later
query can observe its complete result. Changing its metadata or dropping the
adapter cancels the retained task. It never initializes a model or CUDA. Explicit full
verification is separate from public discovery and always repeats before launch.

The private health attestation and native validator qualification are unchanged.
An operational-only private `/scala/execution` observation reports the actual
CUDA device/allocation for `torch-readout`, bound to the same launch nonce.
CUDA launches require CUDA availability, float32 readout weights on CUDA, and
every model parameter on CUDA. These checks prohibit silent CPU offloading.

## Explicit provisioning boundary

`crates/scala-engine-native-decision/runtime/prepare.py` prepares source descriptors
and isolated runtime directories. It never installs dependencies, creates a Model
Profile, edits Settings, selects a runtime, starts serving, or changes production
configuration. `prepare.py --help`, `prepare.py bundle --help` and
`prepare.py runtime --help` document required explicit bindings/options.

A prepared runtime requires a dedicated interpreter with **every installed
wheel** locked, plus immutable native implementation source. `runtime` copies
only Scala's small wrapper and writes a config/absolute-interpreter launcher. It
probes dependencies only; failed preparation removes that new runtime directory.
For `vllm-labels`, provide the source directory, exact revision and explicit shim
entrypoint; for `torch-readout`, provide a clean Git checkout and its exact commit.
The reviewed Imajev implementation is
[`91729bff7a806187324e287fd0d730839dc32026`](https://github.com/mohit67890/imajev/tree/91729bff7a806187324e287fd0d730839dc32026).
That runtime source commit is separate from the adapter's Hugging Face revision.

After provisioning, ordinary explicit Scala configuration can expose both
variants without a shared cross-engine inference parent:

```toml
[engine.native_decision]
enabled = true
[engine.native_decision.native]
binaries = ["/absolute/label-runtime/native-decision-server", "/absolute/readout-runtime/native-decision-server"]
```

A single `native.binary` is also supported, mutually exclusive with `binaries`.
There is no automatic runtime selection. Runtime options are read-only properties
of that exact runtime, artifact calibration is model-owned, and no Settings,
profile, load or request inference override is currently supported by this engine.
Such overrides fail validation; inherited values remain absent. Configure explicit
runtime/model compatibility first and create **auxiliary** profiles through the
normal Model Profiles store only when their engine and installed runtime resolve.
No runtime-wide role or origin-based privilege is introduced.

Native Windows CUDA provisioning for the pinned Imajev implementation is in
[`provision-imajev.ps1`](../crates/scala-engine-native-decision/runtime/windows/provision-imajev.ps1)
with a separately resolved, hash-locked Windows dependency closure. It scopes
Python installation and package caches to the supplied runtime root, leaves
system Python/PATH/drivers untouched, and verifies an existing immutable runtime
against its original receipt on rerun. Windows configuration uses the interpreter
entrypoint in `native.binaries`; backend/platform builds remain runtime variants
of `native_decision`. The narrowly pinned Windows vLLM 0.29.0+cu132 compatibility
exception and reproducible H2O recipe are documented in
[Windows provisioning](native-decision-windows-provisioning.md). Installed
compatibility remains separate from native execution-qualified Decision.

The [`isolated_native_decision`](../crates/scala-api/examples/isolated_native_decision.rs)
example uses the supported explicit `AppPaths` boundary, a new state tree,
loopback ephemeral ports, isolated profile/runtime stores and disabled Link.
[`qualify.py`](../crates/scala-engine-native-decision/runtime/qualify.py) is explicit
operational qualification, never a routine synthetic test. It exercises typed
choice/score/noul, exact native comparison, discovery/proof changes, owned-tree
unload, startup cancellation, subsequent JIT and loaded-instance shutdown.
See [Windows qualification evidence](native-decision-windows-qualification.md)
for tested behavior, incomplete work and activation steps.

The read-only Rust `inspect` example checks source bundles with Scala's actual
artifact discovery without initializing application/production state:

```sh
cargo run -p scala-engine-native-decision --example inspect --offline -- /path/to/bundles
```

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
