#!/usr/bin/env python3
"""vLLM engine-owned runtime. Probe/closure validation never loads weights.

Install in a dedicated runtime directory with an explicit runtime.json and an
absolute-interpreter launcher. This file delegates native scoring and calibration
to upstream; it never reads generated text or substitutes top-k logprobs.
"""
import sys
sys.dont_write_bytecode = True
import argparse
import ast
import base64
import csv
import io
import hashlib
import importlib.metadata
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import zipfile

# Loaded by absolute sibling path in isolated mode; preparation copies this
# internal helper into this runtime and binds its bytes in runtime identity.
def _integrity():
    helper_path = Path(__file__).with_name("python_integrity.py")
    if not helper_path.exists():
        helper_path = Path(__file__).resolve().parents[2] / "scala-engine/runtime/python_integrity.py"
    spec = importlib.util.spec_from_file_location("scala_python_integrity", helper_path)
    helper = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = helper
    spec.loader.exec_module(helper)
    helper.PROTOCOL = PROTOCOL
    helper.ENGINE_ID = ENGINE_ID
    helper.BACKENDS = BACKENDS
    return helper

ENGINE_ID = "vllm"
PROTOCOL = "scala-vllm-v1"
BACKENDS = {"vllm-labels", "vllm-openai"}
integrity = _integrity()
sha, read_json, regular, inventory, module = integrity.sha, integrity.read_json, integrity.regular, integrity.inventory, integrity.module
bundle = integrity.bundle
def runtime_identity(path):
    declared=read_json(path)
    if declared.get("backend")=="vllm-labels" and os.name!="nt":
        expected={"vllm":"0.30.0","torch":"2.13.0","torchvision":"0.28.0","transformers":"5.19.0"}
        if any(declared.get("packages",{}).get(name)!=version for name,version in expected.items()):
            raise ValueError("Unqualified original Linux trained-label dependency tuple")
    return integrity.runtime_identity(path, runner_path=Path(__file__).resolve())
identity_health = integrity.identity_health
def serve_labels(runtime, cfg, revision, args):
    source = cfg["sources"]["model"]
    root = Path(source["path"])
    implementation = runtime["source"]
    artifact_code = source["files"][cfg["bindings"]["shim"]]["sha256"]
    if artifact_code != implementation["files"][implementation["entrypoint"]]["sha256"]:
        raise ValueError("Artifact shim differs from the installed native implementation")
    native = module("scala_native_label_shim", Path(implementation["path"]) / implementation["entrypoint"])
    # Do not permit ambient shim configuration to override trained calibration.
    for key in list(os.environ):
        if key.startswith("SHIM_"):
            del os.environ[key]
    contract = native.Contract(read_json(root / cfg["bindings"]["serve_config"]))
    with socket.socket() as reserve:
        reserve.bind(("127.0.0.1", 0))
        port = reserve.getsockname()[1]
    options = runtime["options"]
    command = [sys.executable, "-m", "vllm.entrypoints.openai.api_server", "--model", str(root), "--served-model-name", contract.model,
               "--host", "127.0.0.1", "--port", str(port), "--max-model-len", str(options["max_model_len"]), "--gpu-memory-utilization", str(options["gpu_memory_utilization"])]
    if "windows_vllm" in runtime:
        port_runtime = module("scala_windows_vllm", Path(__file__).with_name("windows_vllm.py"))
        command = port_runtime.command(sys.executable, root, contract.model, port, runtime)
        os.environ.update(runtime["windows_vllm"]["environment"])
    child = subprocess.Popen(command)
    try:
        def stop(*_):
            raise KeyboardInterrupt
        signal.signal(signal.SIGTERM, stop)
        signal.signal(signal.SIGINT, stop)
        shim = native.Shim(contract, native.VLLM(f"http://127.0.0.1:{port}", contract.model), options["min_context"])
        handler = native.make_handler(shim)
        health = identity_health(revision, cfg, args.bundle, args.launch_nonce)
        class Handler(handler):
            def do_GET(self):
                if self.path == "/health":
                    if child.poll() is not None:
                        return self._send(503, {"error": "Native backend exited"})
                    try:
                        shim.check_backend()  # tokenizer checks only, no forward pass
                    except (native.UpstreamError, native.Unprocessable):
                        return self._send(503, {"error": "Native backend not ready"})
                    return self._send(200, health)
                return self._send(404, {"error": "not found"})
            def do_POST(self):
                if self.path != "/v1/systemone":
                    return self._send(404, {"error": "not found"})
                try:
                    size = int(self.headers.get("Content-Length", "0"))
                    if not 0 <= size <= 16 * 1024 * 1024:
                        raise ValueError("Request exceeds bound")
                    body = json.loads(self.rfile.read(size))
                    if not isinstance(body, dict) or "state" not in body:
                        return self._send(400, {"error": {"type": "invalid_request_error", "message": '"state" must be provided'}})
                    if "images" in body and body["images"] != []:
                        return self._send(422, {"error": "Native label backend does not support images"})
                    return self._send(200, shim.decide(body))
                except native.BadRequest as e:
                    return self._send(400, {"detail": str(e)})
                except (ValueError, native.Unprocessable) as e:
                    return self._send(422, {"detail": str(e)})
                except native.UpstreamError as e:
                    return self._send(e.status, {"detail": str(e)})
        server = native.Server((args.host, args.port), Handler)
        server.serve_forever()
    finally:
        child.terminate()
        try:
            child.wait(timeout=8)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()


def snapshot_fingerprint(config):
    config=Path(config)
    if config.name != "config.json" or not config.is_absolute(): raise ValueError("Expected original local config.json")
    root=config.parent
    cfg=read_json(config)
    if cfg.get("architectures") not in [["LlamaForCausalLM"],["Qwen2ForCausalLM"]] or "auto_map" in cfg or "quantization_config" in cfg:
        raise ValueError("Unqualified vLLM checkpoint architecture/settings")
    tokenizer=read_json(root / "tokenizer_config.json")
    if "auto_map" in tokenizer or ("chat_template" not in tokenizer and not (root / "chat_template.jinja").is_file()):
        raise ValueError("Unqualified tokenizer or missing model-owned chat template")
    files={}
    for directory, children, members in os.walk(root):
        children[:]=[n for n in children if n not in {".git",".cache","__pycache__"}]
        if any((Path(directory)/n).is_symlink() for n in children): raise ValueError("Snapshot contains an unqualified directory alias")
        for name in members:
            file=Path(directory)/name
            relative=file.relative_to(root).as_posix()
            if len(files)>=4096 or len(Path(relative).parts)>4 or file.suffix in {".py",".bin"}:
                raise ValueError("Unqualified or oversized snapshot closure")
            if not file.resolve(strict=True).is_file(): raise ValueError("Non-file snapshot member")
            files[relative]=file
    if "tokenizer.json" not in files or not any(n.endswith(".safetensors") for n in files): raise ValueError("Incomplete local snapshot")
    if "model.safetensors.index.json" in files:
        weights=read_json(files["model.safetensors.index.json"]).get("weight_map",{})
        if not weights or any(not isinstance(v,str) or not v.endswith(".safetensors") or v not in files for v in weights.values()): raise ValueError("Missing/unsafe shard")
    elif "model.safetensors" not in files: raise ValueError("Missing original shard index")
    digest=hashlib.sha256()
    for name,file in sorted(files.items()):
        digest.update(f"{name}\0{file.stat().st_size}\0{sha(file)}\n".encode())
    return root,digest.hexdigest()


def serve_generation(runtime,args):
    root,fingerprint=snapshot_fingerprint(args.model_config)
    if fingerprint != args.model_sha256: raise ValueError("Local snapshot changed before execution")
    options=runtime["options"]
    if "windows_vllm" in runtime:
        # Qualification already bound the exact Windows wheel/SDK/environment.
        # Normal serving lets omitted context/memory remain upstream-owned;
        # H2O retains its separate original, fully explicit port command.
        os.environ.update(runtime["windows_vllm"]["environment"])
    command=[sys.executable,"-I","-m","vllm.entrypoints.cli.main","serve",str(root),"--served-model-name","scala-"+args.launch_nonce,"--host",args.host,"--port",str(args.port)]
    for name in ("max_model_len","gpu_memory_utilization","max_num_seqs","max_num_batched_tokens","cpu_offload_gb"):
        if name in options: command += ["--"+name.replace("_","-"),str(options[name])]
    if options.get("enforce_eager") is True: command += ["--enforce-eager"]
    # No Python HTTP proxy: native vLLM owns streaming/disconnect cancellation,
    # templates, generation_config.json and tokenizer. Scala owns the process tree.
    os.environ.update(HF_HUB_OFFLINE="1",TRANSFORMERS_OFFLINE="1",PYTHONDONTWRITEBYTECODE="1")
    launch_native_server(command, os.name == "nt")


def launch_native_server(command, windows):
    if not windows:
        os.execv(sys.executable,command)
    else:
        # Windows exec creates a replacement process. Keep the supervised
        # parent alive so its Job Object owns the native server and workers.
        child=subprocess.Popen(command)
        try:
            status=child.wait()
            if status: raise RuntimeError(f"vLLM exited with {status}")
        finally:
            if child.poll() is None:
                child.terminate()
                try: child.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    child.kill();child.wait()


def apply_settings(runtime, overrides):
    allowed={"context_length", "gpu_memory_utilization"} if runtime["backend"] == "vllm-labels" else {"context_length","gpu_memory_utilization","enforce_eager","parallel_requests","temperature","top_p","top_k","min_p","seed","repeat_penalty","presence_penalty","frequency_penalty","max_output_tokens","stop_strings"}
    if set(overrides)-allowed or ("windows_vllm" in runtime and ((runtime["backend"] == "vllm-labels" and overrides) or "enforce_eager" in overrides)):
        raise ValueError("Unsupported vLLM tuple override")
    result={**runtime,"options":dict(runtime["options"])}
    mapping={"context_length":"max_model_len","parallel_requests":"max_num_seqs"}
    for key,value in overrides.items():
        result["options"][mapping.get(key,key)]=value
    return result

def runtime_defaults(runtime):
    mapping={"max_model_len":"context_length","gpu_memory_utilization":"gpu_memory_utilization","enforce_eager":"enforce_eager","max_num_seqs":"parallel_requests"}
    defaults={mapping[k]:v for k,v in runtime["options"].items() if k in mapping}
    if runtime["backend"] == "vllm-openai":
        # Observe declarative defaults in the exact installed, verified upstream
        # protocol. Never import tensor code or substitute Scala scalar defaults.
        dist=importlib.metadata.distribution("vllm")
        path=Path(dist.locate_file("vllm/entrypoints/openai/chat_completion/protocol.py"))
        if path.stat().st_size > 1024*1024: raise ValueError("Protocol source exceeds bound")
        tree=ast.parse(path.read_text(encoding="utf-8"))
        for cls in tree.body:
            if isinstance(cls,ast.ClassDef) and cls.name == "ChatCompletionRequest":
                for node in cls.body:
                    if isinstance(node,ast.AnnAssign) and isinstance(node.target,ast.Name) and node.target.id == "_DEFAULT_SAMPLING_PARAMS":
                        observed=ast.literal_eval(node.value)
                        for name in ("temperature","top_p","top_k","min_p","repetition_penalty"):
                            if name in observed: defaults["repeat_penalty" if name=="repetition_penalty" else name]=observed[name]
        for filename,classname,field,setting in [("cache.py","CacheConfig","gpu_memory_utilization","gpu_memory_utilization"),("model.py","ModelConfig","enforce_eager","enforce_eager")]:
            source=Path(dist.locate_file("vllm/config/"+filename))
            if source.stat().st_size>1024*1024:raise ValueError("Config source exceeds bound")
            tree=ast.parse(source.read_text(encoding="utf-8"))
            for cls in tree.body:
                if isinstance(cls,ast.ClassDef) and cls.name==classname:
                    for node in cls.body:
                        if isinstance(node,ast.AnnAssign) and isinstance(node.target,ast.Name) and node.target.id==field:
                            value=node.value
                            if isinstance(value,ast.Call):value=next((k.value for k in value.keywords if k.arg=="default"),None)
                            if value is not None:
                                try: observed=ast.literal_eval(value)
                                except (ValueError,TypeError):continue
                                if isinstance(observed,(int,float,bool)):defaults.setdefault(setting,observed)
    return defaults

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--scala-probe", action="store_true")
    parser.add_argument("--settings", default="{}")
    parser.add_argument("--model-config")
    parser.add_argument("--model-sha256")
    parser.add_argument("--bundle")
    parser.add_argument("--host")
    parser.add_argument("--port", type=int)
    parser.add_argument("--launch-nonce")
    parser.add_argument("--runtime-revision")
    args = parser.parse_args()
    runtime, revision = runtime_identity(Path(__file__).resolve().with_name("runtime.json"))
    if args.scala_probe:
        print(json.dumps({"protocol": PROTOCOL, "engine_id": ENGINE_ID, "version": runtime["packages"]["vllm"], "revision": revision, "backend": runtime["backend"], "defaults": runtime_defaults(runtime), "runtime_options": runtime["options"], "model_code_sha256": runtime["source"]["files"][runtime["source"]["entrypoint"]]["sha256"] if runtime["backend"] == "vllm-labels" else None}))
        return
    if revision != args.runtime_revision or not args.launch_nonce or args.host not in {"127.0.0.1", "::1"} or not args.port:
        raise ValueError("Unverified launch identity/address")
    overrides=json.loads(args.settings)
    if not isinstance(overrides,dict): raise ValueError("Invalid settings overrides")
    runtime = apply_settings(runtime, overrides)
    if runtime["backend"] == "vllm-openai":
        serve_generation(runtime,args)
        return
    cfg = bundle(args.bundle)
    if cfg["backend"] != runtime["backend"]:
        raise ValueError("Source/runtime backend mismatch")
    os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", PYTHONDONTWRITEBYTECODE="1")
    serve = serve_labels
    serve(runtime, cfg, revision, args)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
