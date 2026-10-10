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

PROTOCOL = "scala-native-decision-v1"
BACKENDS = {"vllm-labels", "torch-readout"}


def sha(path):
    with Path(path).open("rb") as f:
        # Most wheel members are tiny. Read them without allocating/zeroing a
        # large bytearray per member; reuse a buffer for the remaining bytes of
        # larger files. This hashes the same complete bytes on every platform.
        first = f.read(64 * 1024)
        digest = hashlib.sha256(first)
        if len(first) < 64 * 1024:
            return digest.hexdigest()
        if hasattr(hashlib, "file_digest"):
            return hashlib.file_digest(f, lambda: digest).hexdigest()
        # Preserve Python 3.10 runtime compatibility.
        buffer = bytearray(256 * 1024)
        view = memoryview(buffer)
        while size := f.readinto(buffer):
            digest.update(view[:size])
        return digest.hexdigest()


def digests(paths):
    if os.name == "nt" and len(paths) >= 128:
        # Cold Windows opens of large header/DLL inventories can serialize on
        # filesystem filters. Bound the I/O concurrency, preserving the exact
        # ordered complete hashes and exceptions. This is metadata verification,
        # never tensor execution or a discovery-time replacement for it.
        from concurrent.futures import ThreadPoolExecutor
        with ThreadPoolExecutor(max_workers=8) as pool:
            return list(pool.map(sha, paths))
    return [sha(path) for path in paths]


def read_json(path):
    with Path(path).open("rb") as f:
        data = f.read(1024 * 1024 + 1)
    if len(data) > 1024 * 1024:
        raise ValueError("JSON exceeds inspection bound")
    return json.loads(data)


def regular(path):
    path = Path(path)
    if not path.is_absolute() or not canonical_equal(path.resolve(strict=True), path) or not path.is_file():
        raise ValueError("Expected a canonical regular file")
    return path


def canonical_equal(left, right):
    # Rust canonical paths include the extended-length prefix. Compare only
    # these two equivalent spellings; aliases/symlinks still fail resolution.
    def spelling(path):
        value = str(path)
        if os.name == "nt":
            if value.startswith("\\\\?\\UNC\\"):
                value = "\\\\" + value[8:]
            elif value.startswith("\\\\?\\"):
                value = value[4:]
        return value
    return spelling(left) == spelling(right)


def inventory(root, files, verify=True):
    root = Path(root)
    if not root.is_absolute() or not canonical_equal(root.resolve(strict=True), root) or not root.is_dir() or not files:
        raise ValueError("Invalid source root/inventory")
    for relative, item in files.items():
        p = Path(relative)
        if p.is_absolute() or not p.parts or any(x in {".", ".."} for x in p.parts):
            raise ValueError("Unsafe inventory member")
        full = regular(root / p)
        if full.stat().st_size != item["size_bytes"] or (verify and sha(full) != item["sha256"]):
            raise ValueError(f"Source member changed: {relative}")
    return root



def original_wheel_attestation(name, version, info):
    """Bind a known vendor RECORD defect to an exact upstream archive, never a rewritten RECORD.

    Only specifically enumerated mismatches may use the archive's verified
    original bytes. All other installed files must pass their normal RECORD
    hashes, and the exception must be necessary and exactly reproduced.
    """
    if set(info) != {"path", "sha256", "mismatches"} or not isinstance(info["mismatches"], dict) or not info["mismatches"]:
        raise ValueError("Invalid original wheel attestation")
    archive = regular(Path(info["path"]))
    if archive.suffix != ".whl" or len(info["sha256"]) != 64 or sha(archive) != info["sha256"]:
        raise ValueError("Original wheel archive SHA256 does not match attestation")
    with zipfile.ZipFile(archive) as wheel:
        rows = [f for f in wheel.namelist() if f.endswith(".dist-info/RECORD")]
        expected = name.lower().replace("-", "_").replace(".", "_") + "-" + version + ".dist-info/RECORD"
        if rows != [expected]:
            raise ValueError("Original wheel RECORD does not match package and version")
        source_records = {r[0]: (r[1], r[2]) for r in csv.reader(io.StringIO(wheel.read(expected).decode("utf-8")))}
        for member, expected_digest in info["mismatches"].items():
            p = Path(member)
            if (p.is_absolute() or not p.parts or any(x in {".", ".."} for x in p.parts)
                    or len(expected_digest) != 64 or any(c not in "0123456789abcdef" for c in expected_digest)
                    or member not in source_records or member not in wheel.namelist()):
                raise ValueError("Invalid declared wheel RECORD mismatch")
            if hashlib.sha256(wheel.read(member)).hexdigest() != expected_digest:
                raise ValueError("Original wheel member SHA256 does not match attestation")
    return source_records


def runtime_identity(config_path):
    cfg = read_json(config_path)
    common = {"schema_version", "backend", "packages", "options"}
    required_fields = common | {"source"}
    allowed = required_fields | {"wheel_attestations", "windows_vllm"}
    if not required_fields <= set(cfg) or set(cfg) - allowed or cfg["schema_version"] != 1 or cfg["backend"] not in BACKENDS:
        raise ValueError("Invalid explicit runtime configuration")
    source = cfg["source"]
    required_source_fields = {"path", "revision", "files"} | ({"entrypoint"} if cfg["backend"] == "vllm-labels" else set())
    if set(source) != required_source_fields or len(source["revision"]) != 40:
        raise ValueError("Native implementation needs an exact source revision/inventory")
    root = inventory(source["path"], source["files"])
    options = cfg["options"]
    required = {"max_model_len", "gpu_memory_utilization", "min_context"} if cfg["backend"] == "vllm-labels" else {"rotations", "max_input_tokens", "device"}
    windows_port = None
    if "windows_vllm" in cfg:
        windows_port = module("scala_windows_vllm", Path(__file__).with_name("windows_vllm.py"))
        windows_port.validate(cfg, os.name == "nt")
        required |= windows_port.EXTRA_OPTIONS
    if set(options) != required:
        raise ValueError("Runtime options must be explicit; unsupported overrides are rejected")
    if cfg["backend"] == "vllm-labels":
        if not isinstance(options["max_model_len"], int) or not isinstance(options["min_context"], int) or not 0 < options["min_context"] <= options["max_model_len"] or not 0 < options["gpu_memory_utilization"] < 1:
            raise ValueError("Invalid native context/memory configuration")
        if windows_port is None and cfg["packages"].get("vllm", "").split("+")[0] != "0.30.0":
            raise ValueError("This label readout contract requires vLLM 0.30.0")
        if source["entrypoint"] not in source["files"] or set(source["files"]) != {source["entrypoint"]}:
            raise ValueError("The label shim must be bound as runtime implementation code")
        required_packages = {"vllm", "torch", "transformers", "safetensors"}
    else:
        if options["rotations"] not in {1, 4} or not isinstance(options["max_input_tokens"], int) or options["max_input_tokens"] <= 0 or options["device"] not in {"cuda", "cpu", "mps"}:
            raise ValueError("Invalid native rotation/context/device configuration")
        required_packages = {"torch", "torchvision", "peft", "transformers", "safetensors", "fastapi", "uvicorn", "pydantic", "Pillow"}
        actual = {p.relative_to(root).as_posix() for directory in (root / "src", root / "scripts") for p in directory.rglob("*.py")}
        if actual != set(source["files"]):
            raise ValueError("Native implementation Python closure changed")
        if "scripts/playground/server.py" not in actual or "scripts/torch_decision.py" not in actual:
            raise ValueError("Native readout implementation is missing")
    # Wheel metadata names are case-insensitive (Pillow now records `pillow`).
    normalize = lambda name: name.lower().replace("_", "-").replace(".", "-")
    missing = {normalize(n) for n in required_packages} - {normalize(n) for n in cfg["packages"]}
    if missing:
        raise ValueError("Required serving dependencies are missing from runtime lock: " + ", ".join(sorted(missing)))
    attestations = cfg.get("wheel_attestations", {})
    if not isinstance(attestations, dict) or any(normalize(k) != k for k in attestations):
        raise ValueError("Wheel attestation package names must be canonical")
    if set(attestations) - {normalize(n) for n in cfg["packages"]}:
        raise ValueError("Attestation refers to an unlocked runtime package")
    packages = {}
    # Verify the actual installed wheel file closure against RECORD, without
    # importing torch, allocating a GPU context or evaluating a model.
    for name, version in cfg["packages"].items():
        dist = importlib.metadata.distribution(name)
        if dist.version != version:
            raise ValueError(f"Locked runtime package changed: {name}")
        attestation = attestations.get(normalize(name))
        source_records = original_wheel_attestation(name, version, attestation) if attestation else {}
        mismatches = attestation["mismatches"] if attestation else {}
        matched_mismatches = set()
        records = []
        members = [file for file in dist.files or [] if file.hash is not None]
        paths = [Path(dist.locate_file(file)) for file in members]
        for file in members:
            # Opening and hashing every member below also rejects missing files
            # and directories. Avoid a separate serial Windows stat per member.
            if file.hash.mode != "sha256":
                raise ValueError(f"Unverifiable runtime package file: {file}")
        for file, observed_sha in zip(members, digests(paths)):
            got = base64.urlsafe_b64encode(bytes.fromhex(observed_sha)).decode().rstrip("=")
            member = str(file)
            if got != file.hash.value:
                if (member not in mismatches or observed_sha != mismatches[member]
                        or source_records.get(member, (None,))[0] != "sha256=" + file.hash.value):
                    raise ValueError(f"Runtime package file changed: {file}")
                matched_mismatches.add(member)
            elif member in mismatches:
                raise ValueError(f"Wheel RECORD exception is unnecessary: {member}")
            records.append((member, got))
        if set(mismatches) != matched_mismatches:
            raise ValueError(f"Original wheel attestation not fully consumed: {name}")
        if not records:
            raise ValueError(f"Runtime package has no verifiable wheel closure: {name}")
        packages[name] = {"version": version, "files": sorted(records)}
    # All installed distributions must be locked, including transitive native
    # libraries. Editable/unlocked installs cannot acquire this runtime identity.
    if {normalize(d.metadata["Name"]) for d in importlib.metadata.distributions()} != {normalize(n) for n in cfg["packages"]}:
        raise ValueError("Runtime has unlocked distributions; use a dedicated, fully locked environment")
    identity = {"config": cfg, "packages": packages, "python": sha(Path(sys.executable).resolve()), "runner": sha(Path(__file__).resolve()), "python_version": sys.version}
    if os.name == "nt":
        launcher = regular(Path(sys.executable).with_name("scala-native-decision.json"))
        launch = read_json(launcher)
        if (set(launch) != {"protocol", "interpreter", "runner"} or launch["protocol"] != PROTOCOL
                or sys.prefix == sys.base_prefix
                or not canonical_equal(Path(launch["interpreter"]).resolve(strict=True), Path(sys.executable).resolve(strict=True))
                or not canonical_equal(regular(Path(launch["runner"])), Path(__file__).resolve())):
            raise ValueError("Invalid native Windows launcher/environment binding")
        # Bind the redirector, base interpreter, DLL/stdlib environment and venv
        # configuration, rather than merely hashing Windows' small python.exe.
        base = Path(sys.base_prefix).resolve(strict=True)
        interpreter_files = [p for p in base.rglob("*") if p.is_file()
                             and "site-packages" not in p.parts and "__pycache__" not in p.parts]
        interpreter_files.sort()
        identity["windows_interpreter"] = {str(p.relative_to(base)): digest for p, digest in zip(interpreter_files, digests(interpreter_files))}
        identity["windows_launcher"] = sha(launcher)
        identity["windows_venv"] = sha(regular(Path(sys.prefix) / "pyvenv.cfg"))
        # uv's venv activation hooks and native Python auxiliary wheel sites
        # can affect import selection without belonging to a wheel RECORD.
        # Bind their root-level Python/path controls as well as every wheel.
        sites = {Path(sys.prefix) / "Lib/site-packages"}
        sites.update(Path(d.locate_file("")) for d in importlib.metadata.distributions())
        controls = sorted({p for site in sites if site.is_dir() for p in site.iterdir()
                           if p.is_file() and p.suffix in {".py", ".pth"}})
        identity["windows_site_controls"] = {str(p): digest for p, digest in zip(controls, digests(controls))}
    if windows_port is not None:
        identity["windows_vllm"] = windows_port.verify(cfg, packages, sys.modules[__name__])
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
        actual = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file() and ".cache" not in p.parts and p.suffix in {".json", ".safetensors", ".py", ".jinja", ".txt"}}
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
    with tempfile.TemporaryDirectory(prefix="scala-native-decision-") as temporary:
        descriptor = Path(temporary) / "base.json"
        descriptor.write_text(json.dumps({"path": base["path"], "repo_id": base["repository"], "revision": base["revision"]}))
        backend = native.TorchBackend(bundle=descriptor, adapter=Path(adapter["path"]), device=options["device"], rotations=options["rotations"], max_input_tokens=options["max_input_tokens"], fast=False, merge_lora=False)
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
