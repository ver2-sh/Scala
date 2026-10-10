#!/usr/bin/env python3
"""Native Decision runtime v1. Probe/closure validation never loads weights.

Install in a dedicated runtime directory with an explicit runtime.json and an
absolute-interpreter launcher. This file delegates native scoring and calibration
to upstream; it never reads generated text or substitutes top-k logprobs.
"""
import sys
sys.dont_write_bytecode = True
import argparse
import base64
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

PROTOCOL = "scala-native-decision-v1"
BACKENDS = {"vllm-labels", "torch-readout"}


def sha(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as f:
        for chunk in iter(lambda: f.read(8 * 1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def read_json(path):
    with Path(path).open("rb") as f:
        data = f.read(1024 * 1024 + 1)
    if len(data) > 1024 * 1024:
        raise ValueError("JSON exceeds inspection bound")
    return json.loads(data)


def regular(path):
    path = Path(path)
    if not path.is_absolute() or path.resolve(strict=True) != path or not path.is_file():
        raise ValueError("Expected a canonical regular file")
    return path


def inventory(root, files, verify=True):
    root = Path(root)
    if not root.is_absolute() or root.resolve(strict=True) != root or not root.is_dir() or not files:
        raise ValueError("Invalid source root/inventory")
    for relative, item in files.items():
        p = Path(relative)
        if p.is_absolute() or not p.parts or any(x in {".", ".."} for x in p.parts):
            raise ValueError("Unsafe inventory member")
        full = regular(root / p)
        if full.stat().st_size != item["size_bytes"] or (verify and sha(full) != item["sha256"]):
            raise ValueError(f"Source member changed: {relative}")
    return root


def runtime_identity(config_path):
    cfg = read_json(config_path)
    common = {"schema_version", "backend", "packages", "options"}
    allowed = common | {"source"}
    if set(cfg) != allowed or cfg["schema_version"] != 1 or cfg["backend"] not in BACKENDS:
        raise ValueError("Invalid explicit runtime configuration")
    source = cfg["source"]
    required_source_fields = {"path", "revision", "files"} | ({"entrypoint"} if cfg["backend"] == "vllm-labels" else set())
    if set(source) != required_source_fields or len(source["revision"]) != 40:
        raise ValueError("Native implementation needs an exact source revision/inventory")
    root = inventory(source["path"], source["files"])
    options = cfg["options"]
    required = {"max_model_len", "gpu_memory_utilization", "min_context"} if cfg["backend"] == "vllm-labels" else {"rotations", "max_input_tokens", "device"}
    if set(options) != required:
        raise ValueError("Runtime options must be explicit; unsupported overrides are rejected")
    if cfg["backend"] == "vllm-labels":
        if not isinstance(options["max_model_len"], int) or not isinstance(options["min_context"], int) or not 0 < options["min_context"] <= options["max_model_len"] or not 0 < options["gpu_memory_utilization"] < 1:
            raise ValueError("Invalid native context/memory configuration")
        if cfg["packages"].get("vllm", "").split("+")[0] != "0.30.0":
            raise ValueError("This label readout contract requires vLLM 0.30.0")
        if source["entrypoint"] not in source["files"] or set(source["files"]) != {source["entrypoint"]}:
            raise ValueError("The label shim must be bound as runtime implementation code")
        required_packages = {"vllm", "torch", "transformers", "safetensors"}
    else:
        if options["rotations"] not in {1, 4} or not isinstance(options["max_input_tokens"], int) or options["max_input_tokens"] <= 0 or options["device"] not in {"cuda", "cpu", "mps"}:
            raise ValueError("Invalid native rotation/context/device configuration")
        required_packages = {"torch", "peft", "transformers", "safetensors", "fastapi", "uvicorn", "pydantic", "Pillow"}
        actual = {str(p.relative_to(root)) for directory in (root / "src", root / "scripts") for p in directory.rglob("*.py")}
        if actual != set(source["files"]):
            raise ValueError("Native implementation Python closure changed")
        if "scripts/playground/server.py" not in actual or "scripts/torch_decision.py" not in actual:
            raise ValueError("Native readout implementation is missing")
    # Wheel metadata names are case-insensitive (Pillow now records `pillow`).
    normalize = lambda name: name.lower().replace("_", "-").replace(".", "-")
    if not {normalize(n) for n in required_packages} <= {normalize(n) for n in cfg["packages"]}:
        raise ValueError("Required serving dependencies are missing from runtime lock")
    packages = {}
    # Verify the actual installed wheel file closure against RECORD, without
    # importing torch, allocating a GPU context or evaluating a model.
    for name, version in cfg["packages"].items():
        dist = importlib.metadata.distribution(name)
        if dist.version != version:
            raise ValueError(f"Locked runtime package changed: {name}")
        records = []
        for file in dist.files or []:
            if file.hash is None:
                continue
            path = Path(dist.locate_file(file))
            if not path.is_file() or file.hash.mode != "sha256":
                raise ValueError(f"Unverifiable runtime package file: {file}")
            got = base64.urlsafe_b64encode(bytes.fromhex(sha(path))).decode().rstrip("=")
            if got != file.hash.value:
                raise ValueError(f"Runtime package file changed: {file}")
            records.append((str(file), got))
        if not records:
            raise ValueError(f"Runtime package has no verifiable wheel closure: {name}")
        packages[name] = {"version": version, "files": sorted(records)}
    # All installed distributions must be locked, including transitive native
    # libraries. Editable/unlocked installs cannot acquire this runtime identity.
    if {normalize(d.metadata["Name"]) for d in importlib.metadata.distributions()} != {normalize(n) for n in cfg["packages"]}:
        raise ValueError("Runtime has unlocked distributions; use a dedicated, fully locked environment")
    identity = {"config": cfg, "packages": packages, "python": sha(Path(sys.executable).resolve()), "runner": sha(Path(__file__).resolve()), "python_version": sys.version}
    revision = hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    return cfg, revision


def bundle(path, verify=True):
    cfg = read_json(regular(Path(path)))
    if set(cfg) != {"schema_version", "backend", "sources", "bindings"} or cfg["schema_version"] != 1 or cfg["backend"] not in BACKENDS:
        raise ValueError("Unsupported native bundle")
    for source in cfg["sources"].values():
        if set(source) != {"path", "repository", "revision", "files"} or not source["repository"] or len(source["revision"]) != 40:
            raise ValueError("Source needs its exact upstream identity")
        root = inventory(source["path"], source["files"], verify)
        # Reject unbound files that either native loader might consume.
        actual = {str(p.relative_to(root)) for p in root.rglob("*") if p.is_file() and ".cache" not in p.parts and p.suffix in {".json", ".safetensors", ".py", ".jinja", ".txt"}}
        if actual - set(source["files"]):
            raise ValueError("Source contains unbound native input files")
    sources, bindings = cfg["sources"], cfg["bindings"]
    required_sources = {"model"} if cfg["backend"] == "vllm-labels" else {"base", "adapter"}
    required_bindings = {"shim", "serve_config"} if cfg["backend"] == "vllm-labels" else {"calibration"}
    if set(sources) != required_sources or set(bindings) != required_bindings:
        raise ValueError("Missing/unsupported native source bindings")
    source_key = "model" if cfg["backend"] == "vllm-labels" else "adapter"
    for value in bindings.values():
        if value not in sources[source_key]["files"]:
            raise ValueError("Native binding is not in the verified source closure")
    if cfg["backend"] == "vllm-labels":
        model = sources["model"]
        for file in ("config.json", "model.safetensors", "tokenizer.json", "tokenizer_config.json"):
            if file not in model["files"]:
                raise ValueError("Missing label model weights/tokenizer/config")
        if read_json(Path(model["path"]) / "config.json").get("head_dtype") != "float32":
            raise ValueError("Native fp32 label head is required")
    else:
        adapter, base = sources["adapter"], sources["base"]
        for file in ("adapter_config.json", "adapter_model.safetensors", "decision_readout.json", "decision_readout.safetensors"):
            if file not in adapter["files"]:
                raise ValueError("Missing trained adapter/readout; vocabulary fallback is forbidden")
        spec = read_json(Path(adapter["path"]) / "adapter_config.json")
        bound_base = spec.get("base_model_name_or_path", "")
        snapshot_suffix = "/models--" + base["repository"].replace("/", "--") + "/snapshots/" + base["revision"]
        if (bound_base != base["repository"] and not bound_base.endswith(snapshot_suffix)) or spec.get("revision") not in (None, base["revision"]):
            raise ValueError("Adapter/base checkpoint mismatch")
        # The exact base revision is explicit, rather than a live HF name.
        if "config.json" not in base["files"] or not any(p.endswith(".safetensors") for p in base["files"]):
            raise ValueError("Missing exact native base checkpoint")
    return cfg


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    sys.modules[name] = result
    spec.loader.exec_module(result)
    return result


def identity_health(revision, cfg, path, nonce):
    return {"protocol": PROTOCOL, "runtime_revision": revision, "backend": cfg["backend"], "bundle_sha256": sha(path), "launch_nonce": nonce, "native_readout": True}


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


def serve_readout(runtime, cfg, revision, args):
    root = Path(runtime["source"]["path"])
    sys.path[:0] = [str(root / "src"), str(root / "scripts"), str(root / "scripts/playground")]
    native = module("scala_native_readout_server", root / "scripts/playground/server.py")
    from vision_decision.calibration import TemperatureCalibrator
    base, adapter = cfg["sources"]["base"], cfg["sources"]["adapter"]
    options = runtime["options"]
    calibration = TemperatureCalibrator.load(str(Path(adapter["path"]) / cfg["bindings"]["calibration"]))
    # Native API wants a tiny bundle descriptor. It points at the original
    # checkpoint; no weights or tokenizer are copied or rewritten.
    with tempfile.TemporaryDirectory(prefix="scala-native-decision-") as temporary:
        descriptor = Path(temporary) / "base.json"
        descriptor.write_text(json.dumps({"path": base["path"], "repo_id": base["repository"], "revision": base["revision"]}))
        backend = native.TorchBackend(bundle=descriptor, adapter=Path(adapter["path"]), device=options["device"], rotations=options["rotations"], max_input_tokens=options["max_input_tokens"], fast=False, merge_lora=False)
        if backend.engine.readout is None or backend.engine.readout.weight.dtype != backend.torch.float32:
            raise ValueError("Trained fp32 readout was not loaded; fallback forbidden")
        backend.model = adapter["repository"]
        app = native.create_app(backend, examples=[], static=Path(temporary), calibration=calibration)
        from fastapi.responses import JSONResponse
        health = identity_health(revision, cfg, args.bundle, args.launch_nonce)
        @app.middleware("http")
        async def qualification(request, call_next):
            if request.url.path == "/health":
                return JSONResponse(health)
            if request.url.path != "/v1/systemone" or request.method != "POST":
                return JSONResponse({"error": "not found"}, status_code=404)
            try:
                data = await request.json()
            except ValueError:
                return JSONResponse({"error": "invalid JSON"}, status_code=422)
            if not isinstance(data, dict) or "state" not in data:
                return JSONResponse({"error": {"type": "invalid_request_error", "message": '"state" must be provided'}}, status_code=400)
            if set(data) - {"state", "questions"}:
                return JSONResponse({"error": "unsupported request fields"}, status_code=422)
            # Preserve optional public instructions. Empty instructions are
            # accepted by H2O; Imajev requires a nonempty native instruction.
            # Reject that unsupported request instead of inventing a prompt.
            if any(not q.get("instructions") for q in data.get("questions", {}).values()):
                return JSONResponse({"error": "Native readout requires question instructions"}, status_code=422)
            return await call_next(request)
        import uvicorn
        uvicorn.run(app, host=args.host, port=args.port, log_level="warning")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--scala-probe", action="store_true")
    parser.add_argument("--bundle")
    parser.add_argument("--host")
    parser.add_argument("--port", type=int)
    parser.add_argument("--launch-nonce")
    parser.add_argument("--runtime-revision")
    args = parser.parse_args()
    runtime, revision = runtime_identity(Path(__file__).resolve().with_name("runtime.json"))
    if args.scala_probe:
        print(json.dumps({"protocol": PROTOCOL, "engine_id": "native_decision", "version": "1", "revision": revision, "backend": runtime["backend"], "model_code_sha256": runtime["source"]["files"][runtime["source"]["entrypoint"]]["sha256"] if runtime["backend"] == "vllm-labels" else None}))
        return
    if revision != args.runtime_revision or not args.launch_nonce or args.host not in {"127.0.0.1", "::1"} or not args.port:
        raise ValueError("Unverified launch identity/address")
    cfg = bundle(args.bundle)
    if cfg["backend"] != runtime["backend"]:
        raise ValueError("Source/runtime backend mismatch")
    os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", PYTHONDONTWRITEBYTECODE="1")
    serve = serve_labels if cfg["backend"] == "vllm-labels" else serve_readout
    serve(runtime, cfg, revision, args)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
