# vLLM and PyTorch engines

Scala registers `llama.cpp`, `q27`, `ninfer` (displayed as NInfer), `vllm`, and
`pytorch`. Decision is a capability of a qualified runtime/model/settings tuple.
It is not an engine. PEFT supplies adapters to a PyTorch runner; it is not a
separate inference technology or engine identity.

An engine is Scala's adapter and compatibility contract. A runtime is one
independently installed executable environment, version and integrity fingerprint.
A model is its original checkpoint, tokenizer, template and calibration artifacts.
Settings and Model Profiles are user-owned overrides/bindings, not runtime packages.
Updating Scala does not rename, replace or activate installed runtimes or profiles.

## Qualified implementations

| Engine / runner | Runtime version | Artifact | Public capability |
| --- | --- | --- | --- |
| vLLM / upstream OpenAI server | Linux vLLM 0.30.0; native Windows pinned 0.29.0+cu132 port | Original local HF safetensors snapshot (`config.json` in model inventory) | Text Chat Completions and Responses, including native streaming and disconnect cancellation |
| vLLM / H2O trained labels | Same independently pinned platform versions | Existing `vllm-labels` `.decisionbundle` | System One text Decision; original trained label scoring and calibration |
| PyTorch / `imajev-readout-v1` | torch 2.14.1 or 2.14.1+cu130, with a completely bound dependency environment | Existing `torch-readout` `.decisionbundle`, original adapter/base/readout/calibration | System One text and image Decision |

The OpenAI text runner currently qualifies unquantized `LlamaForCausalLM` and
`Qwen2ForCausalLM` checkpoints with a model-owned chat template and tokenizer.
Missing shards, custom/remote Python model or tokenizer code, pickle weights and
other architectures remain unsupported. This is a reviewed subset of vLLM's
[upstream serving APIs](https://docs.vllm.ai/en/v0.30.0/serving/online_serving/),
not an advertisement of every upstream model, embedding, tool, structured output,
reasoning, audio or vision implementation. A successful native startup must
identify the exact served model and expose both qualified API routes.

Scala preserves the caller's Chat/Responses surface and translates to that native
vLLM API. Foreground Responses use `store=false` and `background=false`. Native
request disconnect handling cancels work. Participant names are preserved on Chat
and rejected on Responses, where no name contract is qualified. `min_p` is a
Chat control; the reviewed Responses protocol does not consume it, so Responses
rejects that override. Native usage is validated; missing counters stay absent.
Scala does not render a substitute chat template or disable `generation_config.json`.

H2O's original shim, serve configuration, tokenizer-slot checks, full label
inventory, float32 trained head and calibrated choice/score/noul answers remain
unchanged. There is no ordinary chat/logprob approximation. The original shim's
label-ID scoring calls are preserved. Windows uses the original exact wheel,
fork revision, private SDK and platform-specific options; Linux retains 0.30.0.

Imajev's first PyTorch runner uses upstream source
[`91729bff7a806187324e287fd0d730839dc32026`](https://github.com/mohit67890/imajev/tree/91729bff7a806187324e287fd0d730839dc32026).
It requires the original `Qwen/Qwen3.5-4B` base revision
`851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a`, the unmodified LoRA, trained float32
readout, tokenizer binding and original calibration. Rotation/context/device
policies are explicit properties of that runtime. The original preprocessing,
unknown probability and abstention remain upstream-owned. CUDA-selected runners
reject incomplete CUDA placement; they never choose CPU silently.

`pytorch.fast` enables the reviewed upstream fast path, preserving unmerged LoRA
and the float32 readout. Its CUDA graph handling remains upstream-owned. This is
an explicit mode, not a performance promise or a numerical equivalence claim from
live testing. Upstream `--merge-lora` rounds merged weights and can move near-tie
decisions and calibration. Scala rejects merged serving until it has an independent
model/calibration qualification; it does not couple merging to fast mode.

## Managed runtime installation and discovery

The normal workflow is **Find compatible runtime → Install → Select runtime →
Load Model Profile**. vLLM and PyTorch use the same Runtime Search, Runtime List
and model runtime picker as Scala's other engines. Installing a runtime never
loads weights, edits a profile, activates a service or upgrades another runtime.
Application updates and runtime updates remain separate explicit operations.

In the TUI, open Runtime Search, search `vllm` or `pytorch`, select a candidate,
review its version, native platform, GPU/driver prerequisites and dependency
recipe, then activate Install and Confirm install. Progress and package errors
use the existing runtime notification area. During an install or update,
Runtime Search shows Cancel installation (F6); it stops the owned process tree
and removes incomplete staging. Close (Escape) only hides the dialog and leaves
the operation running; reopen Runtime Search to cancel it. The installed package appears in Runtime List.
Use the model runtime picker to select it and load a Model Profile separately.

For the model-first workflow, open the existing Model Profile and use Actions →
Install compatible runtime (`s`). This searches against its bound physical
artifact, including the reviewed source/readout contract. Actions → Select runtime
(`v`) uses the same installed runtime picker. The H2O label shim and Imajev trained
readout remain separate variants; arbitrary checkpoints and unqualified upstream
architectures do not become compatible merely because packages were installed.

Equivalent CLI operations:

```sh
scala runtimes search vllm
scala runtimes search pytorch
scala runtimes info <EXACT_RUNTIME_ID>
scala runtimes install <EXACT_RUNTIME_ID>
scala runtimes select --model <MODEL_ID> <EXACT_RUNTIME_ID>
scala model-profiles compatibility <PROFILE> --runtime <EXACT_RUNTIME_ID>
scala model-profiles load <PROFILE>
```

CLI acquisition prints bounded progress on stderr; Ctrl-C cancels it. Explicit
`runtimes check-updates` / `runtimes update <ID>` install qualified newer versions
side-by-side. The old runtime and selection remain intact. Remove only an inactive,
unselected managed runtime with `runtimes remove <ID>`; running/loading leases
also protect it across processes. No runtime is installed automatically.

The application binary embeds the reviewed locks, provisioning helper, runner,
integrity utility, original H2O shim/config and license/source attribution.
A Scala Git checkout, global Python, global uv, manual PowerShell invocation and
TOML edits are unnecessary. Scala verifies and privately extracts CPython 3.12.12
from python-build-standalone 20260211 and uv 0.11.2 from its published wheel.
Every serving wheel has an exact version and acquisition hash. Installation is
binary-only with dependency resolution disabled; the embedded complete lock is
the authority. Windows retains its separate PyTorch CUDA index, compatibility
port and private CUDA SDK recipe. Missing or replaced pinned downloads fail closed.

Linux vLLM uses 0.30.0 / torch 2.13.0 / torchvision 0.28.0 / transformers 5.19.0.
The Linux label recipe retains the reviewed 40,960 context / 4,096 minimum context
and 0.90 GPU memory fraction; Windows retains its independent fixed options.
Linux Imajev uses torch 2.14.1 / torchvision 0.29.1 / PEFT 0.21.2 / transformers
5.19.0 with the exact original Imajev source archive. Shared peripheral pins are
retained from the reviewed locks; new Linux native dependencies follow the pinned
upstream wheel metadata and are frozen with hashes in the Linux locks. No install
resolves floating requirements or picks newer versions. The original CPython 3.12
PyNvVideoCodec 2.0.4 wheel has the sole narrowly reviewed RECORD attestation;
its original archive is retained in the package and only the two previously
audited native members may disagree with their vendor RECORD. The DLPack auxiliary
site preserves both original colliding `build_backend.py` files. No RECORD is
rewritten and no global verification exception is granted.

Currently managed recipes target native Linux glibc x86-64 and Windows x86-64,
with reviewed Ampere/Ada/Blackwell CUDA targets (8.0, 8.6, 8.9, 12.0). Other
platforms, CPU/MPS/ROCm environments and unqualified GPU targets are unavailable.
Linux vLLM's wheel closure requires glibc 2.31+; PyTorch requires glibc 2.28+.
CUDA 13 driver prerequisites are shown per candidate; unknown driver/GPU evidence
blocks acquisition. Linux uses the published CUDA 13.0 driver floor 580.65.06.
Native Windows conservatively requires the reviewed 610.62 host floor from the
preserved evidence, rather than asserting an unreviewed lower Windows/PTX-JIT
configuration. These admission floors do not replace native execution qualification.
See [CUDA driver compatibility](https://docs.nvidia.com/cuda/archive/13.3.0/cuda-toolkit-release-notes/index.html#cuda-driver)
and the [preserved native Windows evidence](decision-windows-evidence.md). The installer reserves at least 32 GiB free runtime storage
for vLLM or 16 GiB for PyTorch. These are conservative acquisition reservations,
not model VRAM or exact installed-size promises. Model-specific memory suitability
and execution qualification remain separate. Linux vLLM acquires a pinned private
C++ compiler environment during Install and retains a verified child-only JIT
binding. Native Windows vLLM reuses compatible Microsoft C++ tools/SDK or invokes
the verified Microsoft installer; Windows approval/restart handling is explicit.
This registered Microsoft installation is the machine-level exception, not a
portable compiler cache. Scala never installs a driver or changes persistent
system PATH or global CUDA selection. See [dependency ownership, reuse and
cleanup](runtime-dependencies.md).

Each package owns its interpreter, venv, copied dependencies, runner, source,
launch/configuration files and any private Windows SDK. Absolute Python bindings
are prepared at the final identity path behind an owned pending marker. A matching
interrupted install can be recovered only while holding its installation lease;
foreign directories or completed immutable runtimes are never overwritten. Atomic
manifest publication after full engine admission is the visibility boundary.
Failed/cancelled acquisitions cannot become selectable. Diagnostics retain the
package/verification failure, while disposable staging is removed.

Private dependency caches are separate from installed environments; wheels use
copy mode, so reclaiming caches cannot invalidate an installed package. Acquisition
leases prevent concurrent cache mutation. Normal uv pruning retains reusable
content; caches over the configured internal bound receive additional pruning,
and verified bootstrap archives are bounded separately. `scala runtimes clean-cache`
explicitly clears only Scala-owned Python dependency/bootstrap cache directories
while preserving diagnostics, installed runtimes, source checkpoints, model weights,
profiles and user files. Removing a runtime deletes its exact owned package only.

Discovery reads metadata without importing tensor libraries or initializing CUDA.
Managed manifests retain a verified-install metadata observation; warm discovery
reuses the engine's observation inventory and invalidates it on changed files or
package topology. Full content/RECORD verification occurs at installation and
execution. Model inventory remains independent of heavy runtime probes.

## Advanced external runtimes

Explicit `native.binary` or `native.binaries` configurations remain supported for
user-owned environments. They appear as external/unmanaged runtimes and Scala does
not remove or upgrade their dependencies. The repository's `prepare.py` utilities
and original native Windows provisioning scripts are advanced preparation tools,
not requirements of the supported managed workflow. Each external environment
must still satisfy the same pinned runner and source/dependency integrity contracts.
On Windows, configure its isolated `Scripts/python.exe` with the adjacent
`scala-vllm.json` or `scala-pytorch.json` launcher descriptor. Never repurpose an
obsolete `scala-native-decision.json` environment; preserve it as historical evidence.

Example advanced configuration:

```toml
[engine.vllm]
enabled = true
[engine.vllm.native]
binaries = ["/absolute/external-runtime/vllm-server"]

[engine.pytorch]
enabled = true
[engine.pytorch.native]
binary = "/absolute/external-runtime/pytorch-server"
```

Scala's supervisor owns POSIX groups and native Windows Job Objects, including
startup children. Windows text serving retains its supervised parent while the
native server runs. Unload, failed startup and crashes clear readiness proof and
clean up the owned tree. A cancelled trained Decision retires that exact backend
because these upstream trained runners cannot safely interrupt a forward in place;
other outstanding Decisions on that backend are cancelled too. The next request
must load and qualify again. No new engine or alternate runtime is substituted.

The historical Linux runtime metadata was inspected read-only to retain its exact
trained-serving dependency tuples: H2O uses vLLM 0.30.0, torch 2.13.0,
torchvision 0.28.0 and transformers 5.19.0; Imajev uses torch 2.14.1,
torchvision 0.29.1, PEFT 0.21.2 and transformers 5.19.0. The trained runners
reject other such tuples before expensive closure verification. Native Windows
keeps its separately pinned locks. Remaining dependencies are fully recorded and
verified against their immutable installed RECORD/source closure. Reading those
metadata files did not execute, update or relabel either installed runtime.

## Settings and runner contract

Precedence is runtime default → engine Settings → Model Profile → load/invocation
→ request, where the native API supports the request field. Only explicit overrides
are stored. Inherit removes the local value. Engine values never transfer between
`vllm.*` and `pytorch.*`. Invalid settings do not erase the independently identified
runtime or remove editors' ability to clear stale keys.

vLLM controls include temperature (0–2), top_p (greater than 0 through 1), top_k,
min_p, seed, repetition/presence/frequency penalties, stop strings and maximum
output tokens. Load controls include context length, GPU memory fraction, eager
mode and maximum sequences. Trained H2O permits only qualified context/memory
load overrides on Linux; its exact pinned Windows options stay read-only.
The Windows OpenAI port also keeps eager mode fixed. Unsupported overrides return
errors instead of being ignored. PyTorch exposes only `pytorch.fast` for this runner.
Readout/calibration, base revision and rotation policies are never generic generation
settings. Additional PyTorch models require their own reviewed runner contract.

A PyTorch runner contract binds: a runner ID and exact upstream source revision;
complete dependency/interpreter/runner hashes and upstream torch version;
model-owned source and trained base/readout/tokenizer/calibration bindings;
explicit allowed controls; supervised startup; health with a fresh launch nonce
and verified artifact digest; strict typed requests/responses; and a cancellation
policy. `imajev-readout-v1` is the sole qualified runner today. Arbitrary PyTorch
checkpoints acquire no serving capability from their extension, publisher or engine.

## Explicit migration from native_decision

Existing configuration, Settings, Model Profiles, descriptors, runtime identities
and selections remain readable. `native_decision` is unregistered and produces an
actionable obsolete-engine error. Scala never renames old runtime records or edits
persistent profiles/selections during an application update.

In the TUI the preserved profile remains visible with an obsolete-engine label.
Open its Engine action (`e`) to review compatible registered engines. Compatibility
comes from the bound artifact, never its profile name or publisher. The review
shows the proposed engine and incompatible overrides to remove; Confirm is the
only action that writes the change. Cancel preserves the entire profile. The
operation retains ID, name, model artifact and role and retains applicable local
overrides. Engine Settings and runtime selections are independent and are never
transferred or rewritten. After reviewing the engine, use Install compatible runtime,
Select runtime and then Load as separate actions.

The equivalent CLI operation first prints a review without mutating legacy state:

```sh
scala model-profiles set-engine <PROFILE> <COMPATIBLE_ENGINE>
scala model-profiles set-engine <PROFILE> <COMPATIBLE_ENGINE> --confirm
```

Incompatible override removal also requires `--confirm`. Clear any obsolete selected
runtime explicitly with `scala runtimes clear-selection --model <MODEL_ID>` (and
`--format decisionbundle` if necessary), then select the exact new managed runtime.
Concurrent profile changes invalidate the reviewed operation. No permanent legacy
runtime or `native_decision` adapter is introduced.

Candidate discovery (`decision_candidate`, `output_modalities: ["decisions"]`)
remains distinct from execution-qualified Decision. `/v1/systemone`, `/v1/decisions`,
Scala Link, activity and cancellation keep their contracts. These source runners
retain System One's native observations. They do not advertise the stricter OpenAI
Decisions contract when complete required distributions/usage are unavailable;
unsupported requests remain errors. Native llama.cpp Decision support is unchanged.

Development validation is synthetic and local. No live model/GPU execution,
benchmark, training, hosted CI or production activation was performed for this
restructuring. Live Linux/Windows model execution and accelerated-mode numerical
qualification remain operational checks against the chosen installed tuple.
