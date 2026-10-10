# Native Windows Decision provisioning

This is a code-only continuation of audited Scala `eda7bd4a`. Development and
validation run on Norted (Linux). No Windows host was contacted, no Windows
provisioning was executed, and no model, GPU qualification, inference, training,
optimization or benchmark was run. Historical execution evidence is retained
separately in [the qualification record](native-decision-windows-qualification.md).

## Implemented support and admission

The shared native launcher, Job Object ownership, integrity verification,
metadata discovery, observation/invalidation and health/readout/nonce
qualification remain implemented. Linux's ordinary `vllm-labels` contract
requires vLLM 0.30.0. Windows has an explicit compatibility exception for exactly
SystemPanic vLLM 0.29.0+cu132, torch 2.11.0+cu130 and triton-windows
3.6.0.post26, with the pinned wheel, source revision, private SDK and environment
bound by `windows_vllm`. This exception already existed in `eda7bd4a`; the old
statement that no exception was admitted was inaccurate.

Installed dependency verification establishes an immutable compatibility
candidate. It does **not** establish execution-qualified Decision. The unchanged
native health/readout/nonce gate must pass after native startup. H2O has no
recorded successful Windows/GPU execution qualification. No GPU numerical
equivalence claim follows from the source review or synthetic tests.

## Reproducible H2O inputs

[`provision-h2o.ps1`](../crates/scala-engine-native-decision/runtime/windows/provision-h2o.ps1)
and its Python helper replace the private investigation procedure. Supply
canonical absolute runtime, model and evidence roots. They acquire dependencies,
create a dedicated venv and auxiliary wheel site, assemble a private SDK, emit
`<name>-windows-vllm.json`, and invoke `prepare.py runtime`. Nothing changes
Scala configuration, profiles, runtime selections, live services, system Python,
PATH, registry, driver or execution policy.

Prerequisites for a later native host: Windows x86-64, existing `uv` and native
`tar.exe`, network access to the sources below, enough disk for the complete
locked closure, and the original staged H2O snapshot. Later execution additionally
requires a CUDA 13-capable NVIDIA driver, a GPU supported by the published wheel
(Ampere/Ada/Blackwell), adequate VRAM, and Windows compiler/runtime prerequisites
for upstream runtime JIT. Provisioning itself never invokes JIT, imports tensor
packages, builds CUDA/vLLM, or loads a model. It does not install or modify drivers
or Visual Studio. The upstream Windows JIT paths use the compiler bundled with
triton-windows; an existing Visual Studio 2019+ x64 C++ toolchain is needed if an
upstream path requires MSVC. That execution dependency is not synthesized by the
metadata probe.

Acquisition pins:

| Input | Source and binding |
| --- | --- |
| CPython 3.12.12, x64 MSVC | [python-build-standalone release 20260211](https://github.com/astral-sh/python-build-standalone/releases/tag/20260211), `cpython-3.12.12+20260211-x86_64-pc-windows-msvc-install_only_stripped.tar.gz`; SHA-256 `93bf8e8c05ede0077b197a29c99ebdaf253497f27190097494265150b4e70ba8` |
| vLLM 0.29.0+cu132 | [SystemPanic v0.29.0](https://github.com/SystemPanic/vllm-windows/releases/tag/v0.29.0), `vllm-0.29.0+cu132-cp312-cp312-win_amd64.whl`; SHA-256 `736a53ff6cbb976735afb73b998c151c2afe69b08ab1546e1e5954d6e4454beb`; source `13e844c86da90c1f96bc516d161ea2a14901ab04` |
| FlashInfer 0.6.11.post3, source `713358284345314df4f40ddc352f4e981f5bb03e` | [SystemPanic release](https://github.com/SystemPanic/flashinfer-windows/releases/tag/v0.6.11.post3), `flashinfer_python-0.6.11.post3-py3-none-any.whl`; SHA-256 `71ef4742ad2d4a9d3ea8aaabd13390fb022389c6e78e6ac86e26ba43a9f27cad` |
| Humming 0.1.15 | [SystemPanic source](https://github.com/SystemPanic/humming-windows/tree/a12df475f241a99256ada01412efc11e79f60e1a), GitHub API tarball at revision `a12df475f241a99256ada01412efc11e79f60e1a`; SHA-256 `dfa70cc28ce10f69c6c71788a22df24f34270901950aae2e16b5b9c8f6305abf` |
| torch 2.11.0+cu130 family | `https://download.pytorch.org/whl/cu130`; exact wheel hashes in the serving lock |
| triton-windows 3.6.0.post26 | PyPI `triton_windows-3.6.0.post26-cp312-cp312-win_amd64.whl`; SHA-256 `189d8c57911aa9d2ff983a715e5c967b325f576307db60924cab22b501a36515` |
| Remaining standard wheels | `https://pypi.org/simple`; exact versions and hashes in the serving lock |

One concrete source-provenance limitation remains: the triton-windows post26
PyPI metadata publishes the exact wheel but no source commit or provenance
attestation, and the referenced repository has no corresponding post26 tag.
The recipe pins the existing audited wheel/version/hash; it cannot honestly
assert its source revision or reproduce that binary from source. The vLLM,
FlashInfer and Humming source revisions above are resolved exact tag commits.
This limitation does not require private files to install the published wheel.

All serving wheel versions/hashes are committed in
[`h2o-requirements.lock`](../crates/scala-engine-native-decision/runtime/windows/h2o-requirements.lock).
The recipe uses binary-only, hash-required, no-dependency installation of that
complete closure; it does not dynamically resolve new dependencies.

The Humming release has no downloadable Windows wheel. The old lock's
`9d0a67ae...` hash described private packaging. The recipe now packages the exact
upstream source with its standard setuptools backend, version 0.1.15, using
[`h2o-packaging.lock`](../crates/scala-engine-native-decision/runtime/windows/h2o-packaging.lock).
This builder environment is separate from serving and is removed on completion.
No `humming/build.py`, `tools/build_native.py`, tensor import or native compiler
runs. Generated metadata line endings and ZIP timestamps/attributes are
normalized; RECORD is generated for this new archive before installation.
Upstream package sources are preserved. The canonical wheel
`humming_kernels-0.1.15-py3-none-any.whl` has SHA-256
`dd2e460632c27b05dac5bcb17a66e2101c8d906526009f88e2e10561483ea646`.
Packaging drift fails before installation. This new acquisition hash replaces
only the privately packaged Humming hash; source and serving versions stay pinned.
It is not a RECORD exception or a rewritten installed package.

`torch-c-dlpack-ext==0.1.5` is installed directly into
`Lib\site-packages\scala-dlpack-wheel`, with one explicit bound `.pth` file.
`apache-tvm-ffi==0.1.14.post1` remains at the ordinary site root. The original
`build_backend.py` members therefore never overwrite each other. Both original
wheel RECORD closures and the import controls participate in runtime identity.
Existing narrowly scoped wheel-attestation support remains unchanged; this
Windows recipe grants no new RECORD exceptions.

The private CUDA SDK is assembled from NVIDIA's original CUDA 13.3.0
redistribution ZIPs, preserving original headers and notices. The
[redistribution manifest](https://developer.download.nvidia.com/compute/cuda/redist/redistrib_13.3.0.json)
has SHA-256 `507eddaab1360336bc0fe17b77552e0b7dfe1e74da888671c3a2f5fad7775db1`.
The helper pins the required component archive hashes directly:

| Component | Version | SHA-256 |
| --- | --- | --- |
| cuda_nvcc | 13.3.33 | `8fed1ab69ed4e637ad76baff572579630674df9ff02570777800782ee5bdfbc5` |
| cuda_cudart | 13.3.29 | `1feb7dd266813ffe8dbc24e115183a5ac35a4795c8d34aca0df85ab616b64d9c` |
| cuda_crt | 13.3.33 | `752c528281a06a0ddf89237d760ffd6acde1b9cd59efc35803c2591127ef55f0` |

Archive URLs are `https://developer.download.nvidia.com/compute/cuda/redist/<component>/windows-x86_64/<component>-windows-x86_64-<version>-archive.zip`.
CUDA_HOME points exclusively at this private SDK. Every assembled SDK file is
inventoried and hashed; added, missing or changed files invalidate verification.
CUDA 13.3 avoids the earlier MSVC alignment patch; no vendor header is patched.
The complete bound environment contains CUDA_HOME, TORCHINDUCTOR_CACHE_DIR,
TRITON_CACHE_DIR, VLLM_CACHE_ROOT, HF_HOME, TEMP, TMP, USERPROFILE,
FLASHINFER_WORKSPACE_BASE, USERNAME and VLLM_USE_FLASHINFER_SAMPLER=0.
All directory values live under the supplied runtime root. No ambient SDK/PATH
or private D-drive investigation JSON is required.

The interpreter, venv, installed dependency closure, shim, SDK, environment and
options contribute to immutable runtime identity. A successful rerun only
probes and compares the full original receipt. An interrupted or failed named
environment is preserved for diagnosis and cannot be synced or overwritten;
retry with a new name. `prepare.py` removes its failed output/launcher normally.
Acquisition or packaging hash failures must be investigated, never relaxed.

## H2O semantics reviewed

The exact model revision is `193ad740925b176a3b70a5a13a7cff2f2fadd01e`.
The recipe checks original shim, config and serve-config hashes before acquisition.
It does not edit checkpoint, tokenizer, calibration or model files. The bound
source descriptor retains the full original payload identity.

At the pinned vLLM source revision, `registry.py` dispatches
`Qwen3_5ForCausalLM` to `qwen3_5.py`, which constructs `Qwen3_5Model` and uses
`LogitsProcessor`. `config/model.py` reads `head_dtype` from the HF config.
`LogitsProcessor._apply_head` performs CUDA projection with float32 output for
the original unquantized head; this is an output/accumulation dtype, not a claim
that the tied checkpoint weights were rewritten to float32.

Completion protocol forwards requested token IDs with ordinary top-k disabled;
the sampler gathers raw log probabilities for those IDs and the completion
serializer retains them. The pinned shim sends up to 128 IDs per call with the
same prompt and answer slot. Its 255-label configuration therefore makes two
calls (128+127); every label must be present. Sampled text is ignored. The shim
checks distinct one-token labels at the exact tokenized answer slot, retains the
original prompt and tokenizer behavior, temperature 0.8 and noul floor 0.801,
and formats choice/score/noul answers itself. Scala delegates to this unchanged
shim and clears ambient SHIM_* overrides.

Offline [fixtures](../crates/scala-engine-native-decision/runtime/fixtures/h2o/README.md)
retain original shim/config bytes, licenses, reviewed source hashes and a head
projection excerpt. Synthetic tests exercise chunk completeness, omitted labels,
distinct tokenizer IDs, typed answers/calibration and a symbolic float32 CUDA
projection call. No tensor runtime is imported. No CPU model, quantization,
parsed generation or replacement scorer is involved.

## Later native Windows commands (not executed in this pass)

Use a reviewed Scala checkout and existing original model snapshot. If the
snapshot has not yet been staged, the native host can acquire the exact revision
with its existing Hugging Face CLI:

```powershell
hf download h2oai/h2o-lightning-4b --revision 193ad740925b176a3b70a5a13a7cff2f2fadd01e --local-dir 'D:\Scala\Models\h2o-lightning-4b'
```

Example roots are explicit placeholders, not dependencies on historical files:

```powershell
$recipe = 'D:\src\Scala\crates\scala-engine-native-decision\runtime\windows'
& "$recipe\provision-h2o.ps1" -RuntimeRoot 'D:\Scala\Runtimes\decision' -ModelRoot 'D:\Scala\Models\h2o-lightning-4b' -EvidenceRoot 'D:\Scala\Evidence\decision' -Name h2o
$python = 'D:\Scala\Runtimes\decision\h2o-env\Scripts\python.exe'
& $python -I -B "$recipe\..\prepare.py" bundle --backend vllm-labels --source model 'D:\Scala\Models\h2o-lightning-4b' h2oai/h2o-lightning-4b 193ad740925b176a3b70a5a13a7cff2f2fadd01e --binding shim h2o_lightning_shim.py --binding serve_config serve_config.json --output 'D:\Scala\Bundles\h2o.decisionbundle'
& $python -I -B 'D:\Scala\Runtimes\decision\h2o-runtime\server.py' --scala-probe
```

Use existing PowerShell policy. When reviewed script text is invoked rather than
its file, pass `-RecipeDirectory $recipe`. Preserve an existing source descriptor
instead of recreating it: descriptor paths affect artifact IDs. `hf download`
may add cache metadata excluded by `prepare.py`; it does not select another
revision or change any model configuration.

After the separate normal application installation/configuration review, merge
these values into that instance's existing config, retaining any other entries:

```toml
[models]
paths = ['D:\Scala\Bundles']
[engine.native_decision]
enabled = true
[engine.native_decision.native]
binaries = ['D:\Scala\Runtimes\decision\h2o-env\Scripts\python.exe']
```

Then use ordinary discovery and explicit selection, with IDs returned by that
instance; no selection is made by provisioning:

```powershell
scala models list
scala runtimes list
scala runtimes select --model <discovered-model-id> <exact-windows-runtime-id>
scala model-profiles set-model <profile> <discovered-model-id>
scala model-profiles set-engine <profile> native_decision
scala model-profiles set-role <profile> auxiliary
scala model-profiles compatibility <profile>
```

Leave inference overrides inherited/empty. These are later configuration steps,
not instructions to activate a service during this pass. Native startup and GPU
readout qualification remain a separate execution validation step. Provisioning,
Windows packaging, SDK loading, GPU memory suitability and real numerical behavior
have not been executed on native Windows in this pass.

## Validation performed on Norted

- `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s crates/scala-engine-native-decision/runtime -p 'test_*.py'`: 20 passed (10 existing, 10 added).
- `cargo fmt --all -- --check`: passed.
- `cargo test --workspace --lib --bins --offline`: 367 passed, 0 failed; one existing manual benchmark ignored.
- `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu --offline`: passed with existing unrelated warnings, unchanged.

Source review used pinned upstream source/metadata and small source archives. The
Humming standard Python packaging backend was exercised on Linux using installed
CPython 3.12.14 and 3.13.15 with only the four lightweight packaging tools; both
produced the canonical hash above. No GPU
environment was installed; no CUDA/vLLM/native build or tensor import occurred.
Synthetic normalization checked Windows generated line endings and RECORD
integrity. PowerShell was reviewed as source; Norted has no PowerShell parser.
Windows provisioning, that packaging backend on native Windows, SDK/JIT loading
and native Windows/GPU execution were not rerun. Historical operational results
were preserved, not independently reverified.
