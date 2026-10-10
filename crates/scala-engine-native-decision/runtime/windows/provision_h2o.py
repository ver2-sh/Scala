"""Native Windows provisioning; pure plan/packaging helpers also run on Linux.

Never import tensor packages, evaluate models, compile CUDA, or alter Scala state.
"""
import argparse
import base64
import csv
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile

HERE = Path(__file__).resolve().parent
MODEL_REVISION = "193ad740925b176a3b70a5a13a7cff2f2fadd01e"
MODEL_FILES = {
    "h2o_lightning_shim.py": "355791c3f7b50a5dc9fa759dfa1b861874678d6e04c70dbf8b86fddccd3c0932",
    "config.json": "6240a991871a574ff04525c90d8f75a2a11bad2f7012ab0774cbc69aca745859",
    "serve_config.json": "dfe5fd08b5b89cfd1be2797baf7a570c32f2883c6520d2b047cd5528591cee7a",
}
HUMMING_REVISION = "a12df475f241a99256ada01412efc11e79f60e1a"
HUMMING_URL = "https://api.github.com/repos/SystemPanic/humming-windows/tarball/" + HUMMING_REVISION
HUMMING_SOURCE_SHA256 = "dfa70cc28ce10f69c6c71788a22df24f34270901950aae2e16b5b9c8f6305abf"
HUMMING_WHEEL_SHA256 = "dd2e460632c27b05dac5bcb17a66e2101c8d906526009f88e2e10561483ea646"
VLLM_URL = "https://github.com/SystemPanic/vllm-windows/releases/download/v0.29.0/vllm-0.29.0%2Bcu132-cp312-cp312-win_amd64.whl"
CUDA_URL = "https://developer.download.nvidia.com/compute/cuda/redist/"
CUDA_COMPONENTS = {
    "cuda_nvcc": ("13.3.33", "8fed1ab69ed4e637ad76baff572579630674df9ff02570777800782ee5bdfbc5"),
    "cuda_cudart": ("13.3.29", "1feb7dd266813ffe8dbc24e115183a5ac35a4795c8d34aca0df85ab616b64d9c"),
    "cuda_crt": ("13.3.33", "752c528281a06a0ddf89237d760ffd6acde1b9cd59efc35803c2591127ef55f0"),
}


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def download(url, path, digest):
    if not path.exists():
        partial = path.with_suffix(path.suffix + ".partial")
        try:
            with urllib.request.urlopen(url, timeout=60) as src, partial.open("wb") as dst:
                shutil.copyfileobj(src, dst)
            if sha(partial) != digest:
                raise ValueError(f"Acquisition SHA256 mismatch: {url}")
            partial.replace(path)
        finally:
            partial.unlink(missing_ok=True)
    if sha(path) != digest:
        raise ValueError(f"Cached acquisition changed: {path}; remove it explicitly before retrying")
    return path


def relative(name):
    path = PurePosixPath(name)
    if (not name or path.is_absolute() or "\\" in name or ":" in name
            or any(p in {"", ".", ".."} for p in name.split("/"))):
        raise ValueError(f"Unsafe archive member: {name}")
    return path


def unpack_source(archive, output):
    with tarfile.open(archive) as source:
        for member in source.getmembers():
            if member.isdir():
                continue
            path = relative(member.name)
            if not member.isfile() or len(path.parts) < 2:
                raise ValueError("Source archive must contain only regular files")
            # No CI workflows are read or executed by this recipe.
            if path.parts[1] == ".github":
                continue
            target = output.joinpath(*path.parts[1:])
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(source.extractfile(member).read())


def normalize_wheel(source, output):
    """Canonical packaging, preserving upstream payload and regenerating RECORD.

    setuptools creates platform-dependent line endings in generated metadata.
    Fixed ZIP_STORED bytes avoid platform/zlib timestamps and compression variance.
    This is a new reproducible wheel, never an attestation for an edited install.
    """
    with zipfile.ZipFile(source) as wheel:
        payload = {}
        for name in wheel.namelist():
            relative(name)
            if name in payload:
                raise ValueError("Duplicate wheel member")
            data = wheel.read(name)
            if ".dist-info/" in name or name == "humming/_version.py":
                data = data.replace(b"\r\n", b"\n")
            payload[name] = data
    records = [n for n in payload if n.endswith(".dist-info/RECORD")]
    if len(records) != 1:
        raise ValueError("Expected one wheel RECORD")
    record = records[0]
    text = io.StringIO(newline="")
    writer = csv.writer(text, lineterminator="\n")
    for name, data in sorted(payload.items()):
        if name != record:
            digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).decode().rstrip("=")
            writer.writerow((name, "sha256=" + digest, len(data)))
    writer.writerow((record, "", ""))
    payload[record] = text.getvalue().encode()
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_STORED) as wheel:
        for name, data in sorted(payload.items()):
            info = zipfile.ZipInfo(name, (2023, 11, 14, 22, 13, 20))
            info.create_system = 0
            info.external_attr = 0o600 << 16
            wheel.writestr(info, data)


def locks(text):
    """Split the original lock before installing; conflicting roots never overlap."""
    blocks = {}
    for line in text.splitlines(keepends=True):
        if line.startswith("#") or not line.strip():
            continue
        if not line[0].isspace():
            match = re.match(r"([a-zA-Z0-9_-]+)(==| @ )", line)
            if not match:
                raise ValueError("Expected only exact hashed wheel requirements")
            name = match[1].lower().replace("_", "-")
            if name in blocks:
                raise ValueError("Duplicate locked package")
            blocks[name] = line
        else:
            blocks[name] += line
    if any("--hash=sha256:" not in block for block in blocks.values()):
        raise ValueError("Every dependency must have an acquisition hash")
    for name in ("torch-c-dlpack-ext", "humming-kernels", "vllm", "apache-tvm-ffi"):
        if name not in blocks:
            raise ValueError(f"Required Windows wheel missing: {name}")
    return ("".join(v for k, v in blocks.items() if k != "torch-c-dlpack-ext"),
            blocks["torch-c-dlpack-ext"])


def sdk_payload(archive, name, output):
    with zipfile.ZipFile(archive) as source:
        for member in source.infolist():
            if member.is_dir():
                continue
            path = relative(member.filename)
            parts = path.parts[1:]
            if not parts:
                raise ValueError("Invalid CUDA component archive")
            target = output.joinpath(*parts) if parts[0] in {"bin", "include", "lib", "nvvm"} else output.joinpath("_component_notices", name, *parts)
            data = source.read(member)
            if target.exists() and target.read_bytes() != data:
                raise ValueError(f"Conflicting SDK components: {target}")
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)


def inventory(root):
    result = {}
    for p in sorted(root.rglob("*")):
        if p.is_symlink():
            raise ValueError("Provisioned closure contains an alias")
        if p.is_file():
            result[p.relative_to(root).as_posix()] = {"size_bytes": p.stat().st_size, "sha256": sha(p)}
    return result


def build_description(root, name, wheel, sdk, port):
    dirs = {key: root / (name + "-work") / key.lower() for key in port.DIRECTORIES}
    dirs["CUDA_HOME"] = sdk
    for p in dirs.values():
        p.mkdir(parents=True, exist_ok=True)
    return {"fork_revision": port.FORK_REVISION,
            "wheel": {"path": str(wheel), "sha256": port.WHEEL_SHA256},
            "sdk": {"path": str(sdk), "files": inventory(sdk)},
            "environment": {**{k: str(v) for k, v in dirs.items()},
                            "USERNAME": name, "VLLM_USE_FLASHINFER_SAMPLER": "0"}}


def run(*command, **kwargs):
    return subprocess.check_output([str(x) for x in command], text=True, **kwargs).strip()


def native_host():
    return os.name == "nt" and sys.version_info[:3] == (3, 12, 12)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for key in ("runtime-root", "model-root", "evidence-root"):
        parser.add_argument("--" + key, type=Path, required=True)
    parser.add_argument("--name", default="h2o")
    args = parser.parse_args()
    if not native_host():
        raise ValueError("Use provision-h2o.ps1 with native Windows CPython 3.12.12")
    if not re.fullmatch(r"[a-zA-Z0-9_-]+", args.name):
        raise ValueError("Invalid isolated runtime name")
    for path in (args.runtime_root, args.model_root, args.evidence_root):
        if not path.is_absolute() or path.resolve(strict=True) != path:
            raise ValueError("Explicit canonical absolute roots are required")
    root, model, evidence, name = args.runtime_root, args.model_root, args.evidence_root, args.name
    env = root / (name + "-env")
    python = env / "Scripts/python.exe"
    runtime = root / (name + "-runtime")
    receipt = evidence / (name + "-runtime-probe.json")
    if runtime.exists():
        if not receipt.is_file():
            raise ValueError("Existing immutable runtime needs its original receipt; use a new name")
        current = run(python, "-I", "-B", runtime / "server.py", "--scala-probe")
        if json.loads(current) != json.loads(receipt.read_text()):
            raise ValueError("Existing immutable runtime changed; provision a separate name")
        print(current)
        return
    # Fail safely after interruption. Never sync/rebuild an existing environment.
    if env.exists() or python.with_name("scala-native-decision.json").exists():
        raise ValueError("Incomplete named environment exists; preserve evidence and retry with a new name")
    if receipt.exists() or (evidence / (name + "-windows-vllm.json")).exists():
        raise ValueError("Evidence name already exists without this runtime; preserve it and use a new name")
    for member, digest in MODEL_FILES.items():
        if sha(model / member) != digest:
            raise ValueError(f"Expected original H2O {MODEL_REVISION}: {member}")
    spec = importlib.util.spec_from_file_location("windows_port", HERE.parent / "windows_vllm.py")
    port = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(port)
    cache = root / (name + "-acquisition")
    cache.mkdir(exist_ok=True)
    wheel = download(VLLM_URL, cache / "vllm-0.29.0+cu132-cp312-cp312-win_amd64.whl", port.WHEEL_SHA256)
    humming = download(HUMMING_URL, cache / "humming-source.tar.gz", HUMMING_SOURCE_SHA256)
    humming_wheel = cache / "humming_kernels-0.1.15-py3-none-any.whl"
    # Only the standard pure Python setuptools wheel backend is invoked. The
    # upstream humming/build.py and tools/build_native.py are never executed.
    with tempfile.TemporaryDirectory(dir=cache) as temp:
        temp = Path(temp)
        source = temp / "source"
        source.mkdir()
        unpack_source(humming, source)
        builder = temp / "builder"
        run("uv", "venv", "--python", sys.executable, builder)
        build_python = builder / "Scripts/python.exe"
        run("uv", "pip", "install", "--python", build_python, "--no-deps", "--require-hashes", "--only-binary", ":all:", "--index-url", "https://pypi.org/simple", "-r", HERE / "h2o-packaging.lock")
        run(build_python, "-I", "-B", "-c", "from setuptools.build_meta import build_wheel; build_wheel('../dist')", cwd=source,
            env={**os.environ, "SETUPTOOLS_SCM_PRETEND_VERSION": "0.1.15", "SOURCE_DATE_EPOCH": "1700000000"})
        normalize_wheel(temp / "dist" / humming_wheel.name, temp / humming_wheel.name)
        if sha(temp / humming_wheel.name) != HUMMING_WHEEL_SHA256:
            raise ValueError("Humming packaging differs from the reviewed canonical wheel; do not relax its hash")
        if humming_wheel.exists() and sha(humming_wheel) != HUMMING_WHEEL_SHA256:
            raise ValueError("Cached Humming wheel changed")
        shutil.copyfile(temp / humming_wheel.name, humming_wheel)
    primary, auxiliary = locks((HERE / "h2o-requirements.lock").read_text())
    main_lock, aux_lock = cache / "primary.lock", cache / "dlpack.lock"
    main_lock.write_text(primary, encoding="utf-8")
    aux_lock.write_text(auxiliary, encoding="utf-8")
    run("uv", "venv", "--python", sys.executable, env)
    install = ["uv", "pip", "install", "--python", python, "--no-deps", "--require-hashes", "--only-binary", ":all:",
               "--index-url", "https://pypi.org/simple", "--index", "https://download.pytorch.org/whl/cu130",
               "--index-strategy", "unsafe-best-match", "--find-links", cache]
    run(*install, "-r", main_lock)
    site = env / "Lib/site-packages/scala-dlpack-wheel"
    run(*install, "--target", site, "-r", aux_lock)
    (site.parent / "scala-dlpack-wheel.pth").write_text(str(site) + "\n", encoding="utf-8")
    sdk = root / (name + "-cuda-13.3.0")
    if sdk.exists():
        raise ValueError("Incomplete SDK exists; use a new runtime name")
    sdk.mkdir()
    for component, (version, digest) in CUDA_COMPONENTS.items():
        member = f"{component}/windows-x86_64/{component}-windows-x86_64-{version}-archive.zip"
        sdk_payload(download(CUDA_URL + member, cache / Path(member).name, digest), component, sdk)
    build = evidence / (name + "-windows-vllm.json")
    build.write_text(json.dumps(build_description(root, name, wheel, sdk, port), indent=2) + "\n", encoding="utf-8")
    options = {"max_model_len": 2048, "min_context": 1024, "gpu_memory_utilization": 0.80,
               "enforce_eager": True, "max_num_seqs": 4, "max_num_batched_tokens": 2048, "cpu_offload_gb": 0}
    command = [python, "-I", "-B", HERE.parent / "prepare.py", "runtime", "--backend", "vllm-labels", "--python", python,
               "--source", model, "--entrypoint", "h2o_lightning_shim.py", "--source-revision", MODEL_REVISION,
               "--windows-vllm", build, "--output", runtime]
    for key, value in options.items():
        command.extend(("--option", key, json.dumps(value)))
    probe = run(*command)
    receipt.write_text(probe + "\n", encoding="utf-8")
    print(probe)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        detail = error.output[-4000:] if isinstance(error, subprocess.CalledProcessError) and error.output else ""
        sys.exit(f"H2O provisioning failed: {error}\n{detail}")
