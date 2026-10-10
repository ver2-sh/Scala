"""Qualified native Windows vLLM build; metadata checks never import tensor code."""
import csv
import io
import os
from pathlib import Path
import zipfile

FORK_REVISION = "13e844c86da90c1f96bc516d161ea2a14901ab04"
WHEEL_SHA256 = "736a53ff6cbb976735afb73b998c151c2afe69b08ab1546e1e5954d6e4454beb"
VERSIONS = {"vllm": "0.29.0+cu132", "torch": "2.11.0+cu130", "triton-windows": "3.6.0.post26"}
DIRECTORIES = {"TORCHINDUCTOR_CACHE_DIR", "TRITON_CACHE_DIR", "VLLM_CACHE_ROOT", "HF_HOME", "TEMP", "TMP", "USERPROFILE", "FLASHINFER_WORKSPACE_BASE", "CUDA_HOME"}
EXTRA_OPTIONS = {"enforce_eager", "max_num_seqs", "max_num_batched_tokens", "cpu_offload_gb"}


def validate(cfg, windows):
    build = cfg["windows_vllm"]
    if (not windows or cfg["backend"] != "vllm-labels"
            or set(build) != {"fork_revision", "wheel", "sdk", "environment"}
            or build["fork_revision"] != FORK_REVISION
            or any(cfg["packages"].get(name) != version for name, version in VERSIONS.items())
            or set(build["wheel"]) != {"path", "sha256"}
            or build["wheel"]["sha256"] != WHEEL_SHA256
            or set(build["sdk"]) != {"path", "files"}):
        raise ValueError("Unqualified native Windows vLLM build")
    options = cfg["options"]
    if (options.get("enforce_eager") is not True or options.get("cpu_offload_gb") != 0
            or any(type(options.get(key)) is not int or options[key] <= 0
                   for key in ("max_num_seqs", "max_num_batched_tokens"))):
        raise ValueError("Invalid qualified Windows vLLM execution options")
    environment = build["environment"]
    if (set(environment) != DIRECTORIES | {"USERNAME", "VLLM_USE_FLASHINFER_SAMPLER"}
            or not all(isinstance(value, str) and value and "\0" not in value for value in environment.values())
            or environment["VLLM_USE_FLASHINFER_SAMPLER"] != "0"
            or environment["CUDA_HOME"] != build["sdk"]["path"]):
        raise ValueError("Invalid qualified Windows vLLM environment")
    return build


def verify(cfg, packages, runtime):
    build = validate(cfg, os.name == "nt")
    for name in DIRECTORIES:
        path = Path(build["environment"][name])
        if not path.is_absolute() or not path.is_dir() or not runtime.canonical_equal(path.resolve(strict=True), path):
            raise ValueError("Windows runtime directory must be canonical and existing")
    archive = runtime.regular(Path(build["wheel"]["path"]))
    if runtime.sha(archive) != WHEEL_SHA256:
        raise ValueError("Qualified Windows vLLM wheel changed")
    # Check the actual installed payload against the exact published archive,
    # in addition to the ordinary complete RECORD checks. No RECORD exception.
    with zipfile.ZipFile(archive) as wheel:
        records = [name for name in wheel.namelist() if name.endswith(".dist-info/RECORD")]
        if len(records) != 1:
            raise ValueError("Qualified Windows vLLM wheel has an invalid RECORD")
        expected = {row[0]: row[1].removeprefix("sha256=")
                    for row in csv.reader(io.StringIO(wheel.read(records[0]).decode("utf-8")))
                    if row[1].startswith("sha256=")}
    actual = {name.replace("\\", "/"): digest for name, digest in packages["vllm"]["files"]}
    if any(actual.get(name) != digest for name, digest in expected.items()):
        raise ValueError("Installed vLLM differs from its qualified Windows wheel")
    sdk = runtime.inventory(build["sdk"]["path"], build["sdk"]["files"])
    if {p.relative_to(sdk).as_posix() for p in sdk.rglob("*") if p.is_file()} != set(build["sdk"]["files"]):
        raise ValueError("Private CUDA SDK closure changed")
    return {"wrapper_sha256": runtime.sha(Path(__file__).resolve()), "wheel_sha256": WHEEL_SHA256}


def command(python, root, model, port, cfg):
    options = cfg["options"]
    return [python, "-I", "-m", "vllm.entrypoints.cli.main", "serve", str(root),
            "--served-model-name", model, "--host", "127.0.0.1", "--port", str(port),
            "--max-model-len", str(options["max_model_len"]),
            "--gpu-memory-utilization", str(options["gpu_memory_utilization"]),
            "--cpu-offload-gb", "0", "--enforce-eager", "--max-num-seqs", str(options["max_num_seqs"]),
            "--max-num-batched-tokens", str(options["max_num_batched_tokens"])]
