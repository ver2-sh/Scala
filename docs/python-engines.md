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

## Runtime installation and discovery

Provision only a new, dedicated environment/root. Existing upstream/native Python
and CUDA package acquisition stays separate from Scala. No general Python package
installer, WSL requirement, system Python/CUDA/driver/PATH edit, or environment
upgrade is introduced. Linux and Windows environments have independent locks and
immutable fingerprints. Explicit `native.binary` and `native.binaries` are mutually
exclusive; each configured entry produces its own runtime inventory/identity.

After installing the exact upstream packages in a dedicated Linux environment,
prepare a new OpenAI runtime without model conversion:

```sh
python3 crates/scala-engine-vllm/runtime/prepare.py runtime \
  --backend vllm-openai --python /absolute/venv/bin/python \
  --output /absolute/new-vllm-runtime
```

Optional `--option NAME JSON_VALUE` pairs bind supported runtime-owned load defaults
(`max_model_len`, `gpu_memory_utilization`, `enforce_eager`, `max_num_seqs`).
Absent defaults remain upstream-owned. Discovery reads locked package metadata and
verifies the installed RECORD closure without importing tensor libraries. It reports
the upstream vLLM/torch version separately from the Scala runner protocol.

H2O preparation uses `crates/scala-engine-vllm/runtime/prepare.py`: the existing
bundle mode records the original model source and `shim` / `serve_config` bindings;
runtime mode binds the original shim source revision/entrypoint and explicit
`max_model_len`, `gpu_memory_utilization` and `min_context` options.
Imajev preparation uses `crates/scala-engine-pytorch/runtime/prepare.py`: bundle
mode binds `adapter`, `base` and `calibration`; runtime mode requires the clean,
exact upstream source checkout and `device`, `rotations`, `max_input_tokens`
options. Optional `fast` is a boolean runtime default. Neither preparation rewrites
model checkpoints, adapters, calibration, source revisions or existing descriptors.

For native Windows, use the independently pinned
[H2O/vLLM recipe](../crates/scala-engine-vllm/runtime/windows/provision-h2o.ps1)
and [Imajev/PyTorch recipe](../crates/scala-engine-pytorch/runtime/windows/provision-imajev.ps1)
in new roots. The H2O port remains vLLM 0.29.0+cu132, torch 2.11.0+cu130 and
triton-windows 3.6.0.post26; Imajev's separate lock uses torch 2.14.1+cu130.
The real dedicated interpreter is the external entrypoint, with an adjacent
`scala-vllm.json` or `scala-pytorch.json` binding the engine-owned runner.
Preparing beside an obsolete `scala-native-decision.json` is rejected. Create a
separate environment rather than editing the original binding or fingerprint.
See [Windows provisioning details](python-engines-windows.md) and
[historical qualification evidence](decision-windows-evidence.md).

Configure ordinary engines using explicit paths:

```toml
[engine.vllm]
enabled = true
[engine.vllm.native]
binaries = ["/absolute/h2o-runtime/vllm-server", "/absolute/text-runtime/vllm-server"]

[engine.pytorch]
enabled = true
[engine.pytorch.native]
binary = "/absolute/imajev-runtime/pytorch-server"
```

On Windows these values name each isolated `Scripts/python.exe`. Runtime defaults
come from the exact installed implementation/options and model-owned generation
configuration. Unknown values stay unknown; they are not persisted as overrides.
Public discovery is bounded and reuses verified observations/in-flight probes;
unchanged listings do not repeatedly hash dependency trees. File and distribution
inventory changes invalidate cached discovery. Unknown launchers remain uncached.
Execution verifies the exact interpreter/runner/dependency identity and complete
model closure independently of discovery. Python model/runtime verification is a
shared internal file utility, not an inference adapter or a cross-engine dispatch layer.

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

1. Back up the existing user state and inspect `scala config show`,
   `scala model-profiles list`, and `scala runtimes list`.
2. Prepare separate `vllm` and/or `pytorch` runtimes under new roots using the
   original dependencies and model sources. Keep the old files/receipts untouched.
   Add the new engine configuration above and inspect `scala runtimes info <ID>`.
3. Explicitly clear any obsolete selected runtime that applies to the model:
   `scala runtimes clear-selection --model <MODEL_ID>`, and, if needed,
   `scala runtimes clear-selection --format decisionbundle`. No command is run
   automatically. Retain old identities/receipts for provenance.
4. Run `scala model-profiles set-engine <H2O_PROFILE> vllm` or
   `scala model-profiles set-engine <IMAJEV_PROFILE> pytorch`. This uses the existing
   preflight operation, retains profile ID/model binding/name/role, and reports
   incompatible local override removal. Engine Settings stores are not transferred.
5. Explicitly select the new runtime if required:
   `scala runtimes select --model <MODEL_ID> <NEW_RUNTIME_ID>`.
   Review `scala model-profiles compatibility <PROFILE> --runtime <NEW_RUNTIME_ID>`.
   Loading/activation is a separate user operation, not part of migration or update.

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
