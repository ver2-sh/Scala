# Historical native Windows Decision evidence, 2026-10-10

This record preserves the earlier operational run; it was not independently
reverified in the subsequent Norted code-only pass. Current provisioning and
compatibility admission are described in
[Windows provisioning](native-decision-windows-provisioning.md).

This is operational development evidence for the changes based on Scala
`c816f4d240064f21ec98e4ae90a30faac857a53d`, not evidence for the unchanged installed
Scala 0.1.15 release. No release, application activation, production profile or
runtime selection, service/startup registration, or live Scala Link registration
was changed. All code changes are in Scala. Linux reference runtime records were
read only.

## Environment and isolation

The discovered Wayfinder owner is Eugene-Gaming,
`wfd1_b2993200f9df354843601bacda7d60e6d5318ee53c0ba24745f020c0e8159021`,
Windows `GAMING-LAPTOP`, account Eugene. Execution uses native Windows MSVC
processes, Python and NVIDIA CUDA; there is no WSL, VM, container, remote
inference or CPU fallback. The GPU is an RTX 4080 Laptop, 12,282 MiB reported
capacity, NVIDIA driver 610.62. Its desktop allocations were left alone.

The explicit `isolated_native_decision` example uses a new `AppPaths` tree,
separate profile/settings/runtime stores, loopback ephemeral public/control
ports, and disabled Link. Original descriptors and checkpoint files are read
in place. Profiles have empty inference overrides and auxiliary roles.
Qualification runs one model at a time.

Persistent roots:

- `D:\Scala\Runtimes\native-decision`: isolated interpreters, environments,
  implementation source and dependency/download caches.
- `D:\Scala\Setup\decision-models\windows`: development checkout, MSVC build,
  test logs, failed-attempt records and qualification evidence.
- `D:\Scala\Setup\decision-models\bundles`: original source descriptors.
- `D:\Scala\Models\decision-sources`: original model payloads, unchanged.

The development inventory discovers `imajev-4b-accc9e6bdd04` and
`h2o-lightning-4b-v1-1-13d8ae2cb64f`. These differ from the previously saved live
profile IDs because descriptor paths participate in artifact identity. Profiles
are not execution proof. Live profiles were not rebound.

## Shared implementation

Windows runtimes use a dedicated venv `Scripts/python.exe` and an adjacent
strict, bounded `scala-native-decision.json` binding the interpreter and wrapper.
Scala launches structured arguments directly under isolated interpreter mode.
Canonical drive/UNC extended-length spellings, spaces and Unicode are supported
without admitting source aliases. The cleared runtime environment retains only
validated `SystemRoot`. The fingerprint binds the executable, sidecar, wrapper,
venv configuration, base interpreter DLL/stdlib closure, root Python/path import
controls, implementation source, complete locked wheels and immutable options.
RECORD verification and the original narrowly scoped wheel attestations remain
intact.

The opted-in process supervisor assigns a suspended child to an unnamed,
non-inheritable kill-on-close Windows Job Object before resuming it. Runtime
workers and probe redirector children remain owned; cancellation, timeout,
unload and shutdown clean up only that tree. Other engines retain their launch
policy. POSIX process groups and launcher behavior remain supported.

Metadata observation inventories paths without tensor imports or GPU work.
Warm reads stat those paths, including Windows file ID/change time to detect
NTFS timestamp-tunneled replacements. Public cold discovery retains the shared
eight-second bound and independent variant deadlines. Authoritative Windows
verification has a separate 120-second deadline and hashes the complete closure;
its bounded eight-reader I/O concurrency changes no digests. Failed variants
cannot discard another completed compatible observation.

The exact six-field health attestation, fresh nonce, source closure and exact
native state-required validator proof remain prerequisites for Decision.
`/scala/execution` is an operational private torch observation, not a health
extension. CUDA qualification verifies the trained float32 readout and every
model parameter reside on CUDA.

## Imajev

The original snapshots are retained:

| Component | Revision |
| --- | --- |
| `mohit67890/imajev-4b` adapter | `f8d8234cebc6c99065c07731e59716dc0a6e27ab` |
| `Qwen/Qwen3.5-4B` base | `851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a` |
| Native Imajev implementation | `91729bff7a806187324e287fd0d730839dc32026` |

Runtime/interpreter: `D:\Scala\Runtimes\native-decision\imajev-runtime` and
`imajev-env\Scripts\python.exe`, using the task-scoped CPython 3.12.12 base
under `python\cpython-3.12.12-windows-x86_64-none`. Executable SHA-256:
`711df14e4ef9f0890c5c84330faba821839f3f6757dbe27cbf69ac3de6852446`.
Current runtime fingerprint:
`380707392d5839c2b5f42e1c49319116c684307fb9ac40be181c465f5dd59d1e`.
Original descriptor SHA-256:
`de43e2a809bea2db599cd832b949cbd9118d1ba9e8cb50c9b52e3beee6a7615e`.

The separately resolved Windows lock has 49 packages, including torch
2.14.1+cu130 (CUDA 13.0), torchvision 0.29.1+cu130, transformers 5.19.0,
PEFT 0.21.2, safetensors 0.8.0 and accelerate 1.15.0. Exact versions and
hashes are in `runtime/windows/imajev-requirements.lock`. No Linux lock or
environment was reused. Provisioning is
[`provision-imajev.ps1`](../crates/scala-engine-native-decision/runtime/windows/provision-imajev.ps1).

The original unmerged adapter, non-fast implementation, float32 trained readout,
tokenizer binding and upstream calibration are used with `device=cuda`, four
rotations and 4096 input tokens. No compilation, optimization, training,
quantization or checkpoint modification was performed.

A password-reset request with explicit instructions for all three questions
returned HTTP 200. The predeclared comparison is exact equality of every typed
answer field; upstream `calibration_version` is recorded separately. No tolerance
was selected after seeing results. Actual representative typed observations:

| Question | Result | Other native observations |
| --- | --- | --- |
| `route` choice | `support`; probabilities `support=0.9976260576754087`, `sales=0.002373942324591405` | confidence `0.9860627340899675`; unknown `0.009233219522081313`; abstained false |
| `urgency` score | `0.15099486833467157`; probabilities `0=0.8490051316653284`, `1=0.15099486833467157`; legend `0=Routine`, `1=Urgent` | confidence `0.679227211965905`; unknown `0.026909420035066394`; abstained false |
| `account_support` noul | `0.9152079429231413` | unknown `0.004934974078122797`; abstained false |

All answers are finite and complete. Native calibration is
`p3-r2-s000291-authored`. Native input usage is 132 tokens with four rotations;
Scala omits public usage because the native response supplies no output count.
CUDA reports 9,675,986,944 allocated bytes and 10,229,907,456 reserved bytes.
Loaded desktop-plus-model GPU usage is approximately 11.5 GiB, dropping to
approximately 1.6 GiB on unload. This small smoke does not prove a maximum-length
4096-token request fits alongside every possible desktop workload.

The final `windows\instance23\qualification.json` reports
`passed_smoke_unload_cancel_reload_shutdown`. Scala and native typed answers match
exactly. Runtime-owned PIDs 142268/148680/148968 exited on unload; startup PIDs
135612/149304 exited on cancellation; subsequent JIT PIDs
147268/148820/148988 exited on loaded-instance shutdown. The new launch nonce
differed from `b3447a54-2440-4279-beba-5076660f30e6`. GPU observations were
11,532 MiB loaded, 1,636 MiB unloaded, 1,570 MiB cancelled and 1,566 MiB after
shutdown. `access.json` was removed by normal isolated application shutdown.

Before execution and after unload the profile has `decision_candidate=true` and
`architecture.output_modalities=["decisions"]`. Only the initialized, validated
loaded backend advertises execution-qualified Decision. No generation, embedding
or streaming claim appears. H2O's unavailable entrypoint was configured first and
did not hide Imajev. Model listings took 0.922/0.844 seconds before load,
3.578/1.094 seconds while loaded, and 1.094 seconds after unload. Warm observation
uses metadata only; five-minute observation expiration can trigger bounded cold
verification and honestly withhold a candidate until complete verification.

`windows\provision-rerun.out` records a successful verification-only rerun with
the same fingerprint. It changed no package or implementation file. Recipe text
may be invoked with an explicit `-RecipeDirectory` under existing PowerShell
policy; no execution policy change is needed.

## H2O independent investigation

The original model remains
`h2oai/h2o-lightning-4b @ 193ad740925b176a3b70a5a13a7cff2f2fadd01e`, with
its original `head_dtype: float32`, tokenizer, shim and serve configuration.
The shim SHA-256 is
`355791c3f7b50a5dc9fa759dfa1b861874678d6e04c70dbf8b86fddccd3c0932`.

The [official installation documentation](https://docs.vllm.ai/en/latest/getting_started/installation/gpu/)
points Windows users to the community port. The latest inspected
[SystemPanic Windows release](https://github.com/SystemPanic/vllm-windows/releases/tag/v0.29.0)
is v0.29.0 at `13e844c86da90c1f96bc516d161ea2a14901ab04`. Its published
Python 3.12 Windows wheel is
`vllm-0.29.0+cu132-cp312-cp312-win_amd64.whl`, SHA-256
`736a53ff6cbb976735afb73b998c151c2afe69b08ab1546e1e5954d6e4454beb`.
It advertises CUDA 13 and Ada support. This was not execution proof. The audited
implementation nevertheless already contained an explicit compatibility exception for this exact pinned Windows
build; the ordinary vLLM 0.30.0 requirement remains for other runtimes.

The unadmitted, separately locked `h2o-investigation-env` uses torch
2.11.0+cu130, triton-windows 3.6.0.post26 and the port's pinned
humming-windows source `a12df475f241a99256ada01412efc11e79f60e1a`.
Static imports of its native CUDA platform, compiled kernels and Qwen3.5 class
pass. The wheel contains float32 head projection and requested token-logprob
implementation. These are static findings, not a loaded-model qualification.

Two original dependencies install different `build_backend.py` bytes at the
same site root. They were isolated using a supported auxiliary wheel site:
`torch-c-dlpack-ext` resides under `Lib\site-packages\scala-dlpack-wheel`, selected
by an explicit bound `.pth`; `apache-tvm-ffi` retains its original root member.
Neither package nor RECORD was edited. Fresh verification hashes 46,812 RECORD
members with zero mismatches. The corresponding locks, original wheels, pinned
source and isolation/audit scripts are retained under the D roots above.

Historical native GPU investigation outcome: pending. H2O was unavailable in
the recorded isolated Scala inventory. The later code-only pass commits a
reproducible provisioning route and corrects the admission wording: the exact
Windows compatibility build is implemented, while successful native H2O execution
qualification remains unrecorded. The private packaging/isolation scripts above
are historical evidence, not current provisioning prerequisites.

## Historical validation and remaining findings

Linux `cargo test --workspace --lib --bins --offline`: 367 passed, one existing
manual benchmark ignored. Workspace all-targets Clippy with warnings denied
passes. Windows synthetic validation: Python wrapper 9, native engine 9, API 56,
owned job/process cleanup 4 and file-replacement observation 1 pass. Tests cover
strict launcher paths (including a genuinely long Unicode venv and literal shell metacharacters), tampering, invalidation, startup/timeout/cancellation,
exact native health/validator qualification, failed-first variant isolation and
Windows owner/alias Decision routing. They do not load real models.

The full Windows engine suite has four existing platform fixture failures
(source Makefile case alias and Linux-specific runtime fixture assumptions).
These are outside the change; the affected Windows suites pass. No claim is made
that every existing workspace test supports Windows. Existing upstream fake-shim
socket resource warnings are recorded without changing upstream model files.

Live Scala Link GPU routing is **pending normal approved application activation**.
The synthetic Scala Link contract uses the observed owner inventory/aliases and
exact native Decision routing without registering a competing service. Ownership,
profiles and runtimes remain on Eugene-Gaming; there is no replication,
federation recursion or failover.

## Activation after audit, merge and normal release

Do not activate this development executable as the live service. After normal
audit/merge/tag-only release and approved installation, rerun the checked-in
provisioning recipe with explicit D runtime/evidence roots. Configure the normal
Windows instance's existing `models.paths` to include the original bundle folder
and `engine.native_decision.native.binaries` with the admitted dedicated Python
entrypoints. Retain its existing loopback server binding and Link registration.

Run `scala models list` and `scala runtimes list` on Eugene-Gaming. Confirm the
current artifact and immutable runtime identities; paths can change IDs. Then use
`scala runtimes select --model <discovered-model-id> <exact-runtime-id>` and
`scala model-profiles set-model <profile> <discovered-model-id>`, followed by
`set-engine <profile> native_decision` and `set-role <profile> auxiliary` when
needed. Leave inference overrides inherited/empty. Check each profile with
`scala model-profiles compatibility <profile>` before a normal load or JIT
request. Admit H2O only after its independent native Windows qualification.

No activation command above was performed against the live application.
