#!/usr/bin/env python3
"""Internal Python runtime and source integrity utilities. Never load weights.

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

PROTOCOL = None  # Set by the engine-owned runner before qualification.
ENGINE_ID = None
BACKENDS = set()


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


def verify_build_toolchain(binding, managed_root):
    """Verify a private durable compiler; this function never changes the environment."""
    import stat
    if os.name == 'nt' or set(binding) != {'schema_version', 'reference', 'environment'} or binding['schema_version'] != 1:
        raise ValueError('Invalid managed Linux compiler binding')
    ref = binding['reference']
    required = {'schema_version', 'recipe_sha256', 'environment_sha256', 'bootstrap_sha256',
                'implementation_version', 'inventory_sha256', 'prefix'}
    if set(ref) != required or ref['schema_version'] != 1 or ref['implementation_version'] != 'scala-linux-dependencies-v1':
        raise ValueError('Unknown managed compiler reference')
    for key in ('recipe_sha256', 'environment_sha256', 'bootstrap_sha256', 'inventory_sha256'):
        value = ref[key]
        if not isinstance(value, str) or len(value) != 64 or any(c not in '0123456789abcdef' for c in value):
            raise ValueError('Invalid compiler reference digest')
    expected = hashlib.sha256((ref['implementation_version']+'\n'+ref['bootstrap_sha256']+'\n'+ref['recipe_sha256']+'\n').encode()).hexdigest()
    if expected != ref['environment_sha256']:
        raise ValueError('Compiler environment identity mismatch')
    owned = Path(managed_root).resolve(strict=True)
    if len(owned.parents) < 5 or owned.parents[3].name != 'runtimes':
        raise ValueError('Compiler binding needs the managed runtime store')
    prefix = Path(ref['prefix'])
    expected_prefix = owned.parents[4] / 'runtime-dependencies/linux' / expected / 'prefix'
    if prefix != expected_prefix or prefix.resolve(strict=True) != prefix or not prefix.is_dir():
        raise ValueError('Compiler prefix escapes its Scala-owned durable store')
    if regular(prefix.parent / '.scala-owner').read_text() != ref['implementation_version']+':'+expected:
        raise ValueError('Compiler environment ownership changed')
    if read_json(regular(prefix.parent / 'ready.json')).get('reference') != ref:
        raise ValueError('Compiler ready receipt changed')
    lock_path = regular(prefix.parent / 'lock.json')
    if sha(lock_path) != ref['recipe_sha256']:
        raise ValueError('Compiler dependency lock changed')
    lock = read_json(lock_path)
    inventory_path = regular(prefix.parent / 'inventory.json')
    if inventory_path.stat().st_size > 128 * 1024 * 1024 or sha(inventory_path) != ref['inventory_sha256']:
        raise ValueError('Compiler inventory changed')
    with inventory_path.open('rb') as stream:
        entries = json.load(stream)
    if not isinstance(entries, dict) or not entries or len(entries) > 500000:
        raise ValueError('Invalid compiler file inventory')
    actual = set()
    for directory, children, members in os.walk(prefix, followlinks=False):
        for name in children + members:
            actual.add((Path(directory) / name).relative_to(prefix).as_posix())
    if actual != set(entries):
        raise ValueError('Compiler payload closure changed')
    for relative, entry in entries.items():
        member = Path(relative)
        if member.is_absolute() or not member.parts or any(p in {'.', '..'} for p in member.parts):
            raise ValueError('Unsafe compiler payload member')
        full = prefix / member
        st = full.lstat()
        if stat.S_IMODE(st.st_mode) != entry['mode']:
            raise ValueError('Compiler payload mode changed')
        kind = entry['kind']
        if kind == 'directory':
            valid = stat.S_ISDIR(st.st_mode) and full.resolve(strict=True).is_relative_to(prefix)
        elif kind == 'symlink':
            target = os.readlink(full)
            valid = stat.S_ISLNK(st.st_mode) and target == entry['target'] and not Path(target).is_absolute() and full.resolve(strict=True).is_relative_to(prefix)
        elif kind == 'file':
            valid = stat.S_ISREG(st.st_mode) and full.resolve(strict=True).is_relative_to(prefix) and st.st_size == entry['size'] and sha(full) == entry['sha256']
        else:
            valid = False
        if not valid:
            raise ValueError('Compiler payload changed: '+relative)
    expected_environment = {
        'CC': str(prefix / 'scala-bin/cc'), 'CXX': str(prefix / 'scala-bin/c++'),
        'PATH': str(prefix / 'scala-bin')+':'+str(prefix / 'bin')+':/usr/bin:/bin',
        'CMAKE_PREFIX_PATH': str(prefix),
        'PKG_CONFIG_LIBDIR': str(prefix / 'lib/pkgconfig')+':'+str(prefix / 'share/pkgconfig'),
    }
    if lock.get('cuda_release') is not None:
        expected_environment.update(CUDA_HOME=str(prefix / 'scala-cuda'), CUDA_PATH=str(prefix / 'scala-cuda'),
                                    CUDACXX=str(prefix / 'bin/nvcc'), NVCC_CCBIN=str(prefix / 'bin/x86_64-conda-linux-gnu-g++'),
                                    CUDAHOSTCXX=str(prefix / 'bin/x86_64-conda-linux-gnu-g++'))
    if binding['environment'] != expected_environment:
        raise ValueError('Unadmitted compiler environment override')
    return ref['inventory_sha256']


def runtime_identity(config_path, runner_path=None):
    cfg = read_json(config_path)
    common = {"schema_version", "backend", "packages", "options"}
    required_fields = common | (set() if cfg.get("backend") == "vllm-openai" else {"source"})
    allowed = required_fields | {"wheel_attestations", "windows_vllm", "managed_root", "build_toolchain"}
    if not required_fields <= set(cfg) or set(cfg) - allowed or cfg["schema_version"] != 1 or cfg["backend"] not in BACKENDS:
        raise ValueError("Invalid explicit runtime configuration")
    if cfg["backend"] == "vllm-openai":
        if cfg["packages"].get("vllm", "").split("+")[0] not in {"0.30.0", "0.29.0"}:
            raise ValueError("Unqualified vLLM OpenAI runtime version")
        if os.name != "nt" and cfg["packages"].get("vllm") != "0.30.0":
            raise ValueError("Linux OpenAI runner requires vLLM 0.30.0")
        if os.name == "nt":
            windows_port = module("scala_windows_vllm", Path(runner_path or __file__).with_name("windows_vllm.py"))
            windows_port.validate(cfg, True)
        else:
            windows_port = None
        required_packages={"vllm", "torch", "transformers", "safetensors"}
        allowed={"max_model_len","gpu_memory_utilization","enforce_eager","max_num_seqs","max_num_batched_tokens","cpu_offload_gb"}
        if set(cfg["options"])-allowed or cfg["options"].get("cpu_offload_gb",0) != 0:
            raise ValueError("Unsupported native vLLM load option")
        for key in {"max_model_len","max_num_seqs","max_num_batched_tokens"} & set(cfg["options"]):
            if type(cfg["options"][key]) is not int or cfg["options"][key]<=0: raise ValueError("Invalid positive vLLM load bound")
        if "gpu_memory_utilization" in cfg["options"] and not 0<cfg["options"]["gpu_memory_utilization"]<=1: raise ValueError("Invalid GPU memory fraction")
        if "enforce_eager" in cfg["options"] and type(cfg["options"]["enforce_eager"]) is not bool: raise ValueError("Invalid eager toggle")
    else:
        source = cfg["source"]
        required_source_fields = {"path", "revision", "files"} | ({"entrypoint"} if cfg["backend"] == "vllm-labels" else set())
        if set(source) != required_source_fields or len(source["revision"]) != 40:
            raise ValueError("Native implementation needs an exact source revision/inventory")
        root = inventory(source["path"], source["files"])
        options = cfg["options"]
        required = {"max_model_len", "gpu_memory_utilization", "min_context"} if cfg["backend"] == "vllm-labels" else {"rotations", "max_input_tokens", "device"}
        windows_port = None
        if "windows_vllm" in cfg:
            windows_port = module("scala_windows_vllm", Path(runner_path or __file__).with_name("windows_vllm.py"))
            windows_port.validate(cfg, os.name == "nt")
            required |= windows_port.EXTRA_OPTIONS
        if cfg["backend"] == "torch-readout" and "fast" in options:
            if type(options["fast"]) is not bool: raise ValueError("fast must be a boolean")
            required |= {"fast"}
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
    identity = {"config": cfg, "packages": packages, "python": sha(Path(sys.executable).resolve()), "runner": sha(runner_path or Path(__file__).resolve()), "integrity": sha(Path(__file__).resolve()), "python_version": sys.version}
    if "managed_root" in cfg:
        owned = Path(cfg["managed_root"]).resolve(strict=True)
        if not config_path.resolve(strict=True).is_relative_to(owned) or not Path(sys.prefix).resolve(strict=True).is_relative_to(owned) or not Path(sys.base_prefix).resolve(strict=True).is_relative_to(owned) or sys.prefix == sys.base_prefix:
            raise ValueError("Managed Python isolation/ownership changed")
        if "source" in cfg and not Path(cfg["source"]["path"]).resolve(strict=True).is_relative_to(owned):
            raise ValueError("Managed source escaped its package")
        base = Path(sys.base_prefix).resolve(strict=True)
        interpreter_files = sorted(p for p in base.rglob("*") if p.is_file() and "site-packages" not in p.parts and "__pycache__" not in p.parts)
        identity["managed_interpreter"] = {str(p.relative_to(base)): digest for p, digest in zip(interpreter_files, digests(interpreter_files))}
        identity["managed_venv"] = sha(regular(Path(sys.prefix) / "pyvenv.cfg"))
        sites = {Path(d.locate_file("")) for d in importlib.metadata.distributions()}
        controls = sorted({p for site in sites for p in site.iterdir() if p.is_file() and p.suffix in {".py", ".pth"}})
        identity["managed_site_controls"] = {str(p): digest for p, digest in zip(controls, digests(controls))}
    if os.name == "nt":
        launcher = regular(Path(sys.executable).with_name(f"scala-{ENGINE_ID}.json"))
        launch = read_json(launcher)
        if (set(launch) != {"protocol", "interpreter", "runner"} or launch["protocol"] != PROTOCOL
                or sys.prefix == sys.base_prefix
                or not canonical_equal(Path(launch["interpreter"]).resolve(strict=True), Path(sys.executable).resolve(strict=True))
                or not canonical_equal(regular(Path(launch["runner"])), (runner_path or Path(__file__).resolve()))):
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
    if "build_toolchain" in cfg:
        if "managed_root" not in cfg or not cfg["backend"].startswith("vllm"):
            raise ValueError("Unowned or unsupported compiler binding")
        identity["build_toolchain"] = verify_build_toolchain(cfg["build_toolchain"], cfg["managed_root"])
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


