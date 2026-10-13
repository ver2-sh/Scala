#!/usr/bin/env python3
"""PyTorch model-specific runner. Probe/closure validation never loads weights.

Install in a dedicated runtime directory with an explicit runtime.json and an
absolute-interpreter launcher. This file delegates native scoring and calibration
to upstream; it never reads generated text or substitutes top-k logprobs.
"""
import sys
sys.dont_write_bytecode = True
import argparse
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

ENGINE_ID = "pytorch"
PROTOCOL = "scala-pytorch-v1"
BACKENDS = {"torch-readout"}
integrity = _integrity()
sha, read_json, regular, inventory, module = integrity.sha, integrity.read_json, integrity.regular, integrity.inventory, integrity.module
bundle = integrity.bundle
def runtime_identity(path):
    declared=read_json(path)
    packages=declared.get("packages",{})
    expected={"torch":"2.14.1+cu130" if os.name=="nt" else "2.14.1", "torchvision":"0.29.1+cu130" if os.name=="nt" else "0.29.1", "peft":"0.21.2", "transformers":"5.19.0"}
    if any(packages.get(name)!=version for name,version in expected.items()):
        raise ValueError("Unqualified PyTorch/PEFT/transformers runner dependency tuple")
    cfg, revision=integrity.runtime_identity(path, runner_path=Path(__file__).resolve())
    if cfg["source"]["revision"] != "91729bff7a806187324e287fd0d730839dc32026":
        raise ValueError("Unqualified Imajev serving runner source revision")
    if cfg["packages"].get("torch") not in {"2.14.1","2.14.1+cu130"}:
        raise ValueError("Unqualified PyTorch version for this Imajev runner")
    return cfg,revision
identity_health = integrity.identity_health
MAX_IMAGE_BYTES = 8 * 1024 * 1024


def validate_image_transport(images):
    """Transport bounds only; upstream owns raster validation/preprocessing."""
    if not isinstance(images, list) or len(images) > 2:
        raise ValueError("images must be an array of at most two data URLs")
    for index, image in enumerate(images):
        if not isinstance(image, str) or "," not in image:
            raise ValueError(f"images[{index}] must be a base64 data URL")
        header, payload = image.split(",", 1)
        if header not in {"data:image/png;base64", "data:image/jpeg;base64", "data:image/webp;base64"}:
            raise ValueError(f"images[{index}] must be a PNG, JPEG, or WebP base64 data URL")
        if len(payload) > ((MAX_IMAGE_BYTES + 2) // 3) * 4:
            raise ValueError(f"images[{index}] exceeds 8 MiB")
        try:
            decoded = base64.b64decode(payload, validate=True)
        except (ValueError, UnicodeEncodeError) as error:
            raise ValueError(f"images[{index}] contains invalid base64") from error
        if not decoded:
            raise ValueError(f"images[{index}] must not be empty")
        if len(decoded) > MAX_IMAGE_BYTES:
            raise ValueError(f"images[{index}] exceeds 8 MiB")


async def qualify_readout_request(request, call_next, health, execution):
    from fastapi.responses import JSONResponse
    if request.url.path == "/health":
        return JSONResponse(health)
    if request.url.path == "/scala/execution" and request.method == "GET":
        return JSONResponse(execution)
    if request.url.path != "/v1/systemone" or request.method != "POST":
        return JSONResponse({"error": "not found"}, status_code=404)
    try:
        if len(await request.body()) > 32 * 1024 * 1024:
            return JSONResponse({"error": "Request exceeds 32 MiB"}, status_code=413)
        data = await request.json()
    except ValueError:
        return JSONResponse({"error": "invalid JSON"}, status_code=422)
    if not isinstance(data, dict) or "state" not in data:
        return JSONResponse({"error": {"type": "invalid_request_error", "message": '"state" must be provided'}}, status_code=400)
    if set(data) - {"state", "questions", "images"}:
        return JSONResponse({"error": "unsupported request fields"}, status_code=422)
    try:
        validate_image_transport(data.get("images", []))
    except ValueError as error:
        return JSONResponse({"error": str(error)}, status_code=422)
    # Preserve optional public instructions. Empty instructions are
    # accepted by H2O; Imajev requires a nonempty native instruction.
    # Reject that unsupported request instead of inventing a prompt.
    if any(not q.get("instructions") for q in data.get("questions", {}).values()):
        return JSONResponse({"error": "Native readout requires question instructions"}, status_code=422)
    return await call_next(request)


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
    with tempfile.TemporaryDirectory(prefix="scala-pytorch-") as temporary:
        descriptor = Path(temporary) / "base.json"
        descriptor.write_text(json.dumps({"path": base["path"], "repo_id": base["repository"], "revision": base["revision"]}))
        backend = native.TorchBackend(bundle=descriptor, adapter=Path(adapter["path"]), device=options["device"], rotations=options["rotations"], max_input_tokens=options["max_input_tokens"], fast=options.get("fast", False), merge_lora=False)
        if backend.engine.readout is None or backend.engine.readout.weight.dtype != backend.torch.float32:
            raise ValueError("Trained fp32 readout was not loaded; fallback forbidden")
        if options["device"] == "cuda":
            if (not backend.torch.cuda.is_available()
                    or backend.engine.readout.weight.device.type != "cuda"
                    or any(p.device.type != "cuda" for p in backend.engine.model.parameters())):
                raise ValueError("Native CUDA execution requires the complete model and readout on GPU")
        backend.model = adapter["repository"]
        app = native.create_app(backend, examples=[], static=Path(temporary), calibration=calibration)
        health = identity_health(revision, cfg, args.bundle, args.launch_nonce)
        execution = {**health, "execution_device": options["device"]}
        if options["device"] == "cuda":
            execution["cuda_device_name"] = backend.torch.cuda.get_device_name()
            execution["cuda_allocated_bytes"] = backend.torch.cuda.memory_allocated()
            execution["cuda_reserved_bytes"] = backend.torch.cuda.memory_reserved()
        @app.middleware("http")
        async def qualification(request, call_next):
            return await qualify_readout_request(request, call_next, health, execution)
        import uvicorn
        uvicorn.run(app, host=args.host, port=args.port, log_level="warning")


def apply_settings(runtime, overrides):
    if set(overrides) - {"fast"} or any(type(v) is not bool for v in overrides.values()):
        raise ValueError("Unsupported PyTorch runner override; merged LoRA lacks calibration qualification")
    result = {**runtime, "options": {**runtime["options"], **overrides}}
    return result

def runtime_defaults(runtime):
    return {"fast":runtime["options"].get("fast",False)}

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--scala-probe", action="store_true")
    parser.add_argument("--settings", default="{}")
    parser.add_argument("--bundle")
    parser.add_argument("--host")
    parser.add_argument("--port", type=int)
    parser.add_argument("--launch-nonce")
    parser.add_argument("--runtime-revision")
    args = parser.parse_args()
    runtime, revision = runtime_identity(Path(__file__).resolve().with_name("runtime.json"))
    if args.scala_probe:
        print(json.dumps({"protocol": PROTOCOL, "engine_id": ENGINE_ID, "runner_id":"imajev-readout-v1", "runner_source_revision":runtime["source"]["revision"], "version": runtime["packages"]["torch"], "revision": revision, "backend": runtime["backend"], "defaults": runtime_defaults(runtime), "runtime_options": runtime["options"], "model_code_sha256": None}))
        return
    if revision != args.runtime_revision or not args.launch_nonce or args.host not in {"127.0.0.1", "::1"} or not args.port:
        raise ValueError("Unverified launch identity/address")
    overrides=json.loads(args.settings)
    if not isinstance(overrides,dict): raise ValueError("Invalid settings overrides")
    runtime = apply_settings(runtime, overrides)
    cfg = bundle(args.bundle)
    if cfg["sources"]["base"]["repository"] != "Qwen/Qwen3.5-4B" or cfg["sources"]["base"]["revision"] != "851bf6e806efd8d0a36b00ddf55e13ccb7b8cd0a":
        raise ValueError("Imajev requires its original trained Qwen base revision")
    if cfg["backend"] != runtime["backend"]:
        raise ValueError("Source/runtime backend mismatch")
    os.environ.update(HF_HUB_OFFLINE="1", TRANSFORMERS_OFFLINE="1", PYTHONDONTWRITEBYTECODE="1")
    serve = serve_readout
    serve(runtime, cfg, revision, args)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
