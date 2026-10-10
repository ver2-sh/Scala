"""List discovery change detectors with the configured isolated interpreter.

This does not qualify a runtime. The original --scala-probe and launch verifier
still hash the source, interpreter, runner, archives and complete wheel closure.
"""
import importlib.metadata
import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
config = Path(sys.argv[1])
with config.open("rb") as stream:
    raw = stream.read(1024 * 1024 + 1)
if len(raw) > 1024 * 1024:
    raise ValueError("Runtime config exceeds inspection bound")
cfg = json.loads(raw)
paths = {config, Path(sys.executable), Path(sys.executable).resolve()}
inventory_paths = set(paths)


def watch(path, root):
    paths.add(path)
    # Parents detect additions, removals and symlink/directory replacements.
    while path != root and path.is_relative_to(root):
        path = path.parent
        inventory_paths.add(path)
        if path in paths:
            break
        paths.add(path)


if os.name == "nt":
    paths.add(Path(sys.executable).with_name("scala-native-decision.json"))
    paths.add(Path(sys.prefix) / "pyvenv.cfg")
    site = Path(sys.prefix) / "Lib/site-packages"
    paths.update(site.glob("*.pth"))
    paths.update(site.glob("*.py"))
    base = Path(sys.base_prefix).resolve(strict=True)
    for directory, children, members in os.walk(base):
        children[:] = [name for name in children if name not in {"site-packages", "__pycache__"}]
        paths.add(Path(directory))
        inventory_paths.add(Path(directory))
        for member in members:
            paths.add(Path(directory) / member)


source = cfg["source"]
root = Path(source["path"])
for member in source["files"]:
    watch(root / member, root)
for directory, _, _ in os.walk(root):
    paths.add(Path(directory))
    inventory_paths.add(Path(directory))
if "windows_vllm" in cfg:
    build = cfg["windows_vllm"]
    paths.add(config.with_name("windows_vllm.py"))
    paths.add(Path(build["wheel"]["path"]))
    sdk = Path(build["sdk"]["path"])
    for member in build["sdk"]["files"]:
        watch(sdk / member, sdk)
    for directory, _, _ in os.walk(sdk):
        paths.add(Path(directory))
        inventory_paths.add(Path(directory))
for attestation in cfg.get("wheel_attestations", {}).values():
    paths.add(Path(attestation["path"]))
for directory in sys.path:
    if directory:
        paths.add(Path(directory))
        inventory_paths.add(Path(directory))
for dist in importlib.metadata.distributions():
    root = Path(dist.locate_file(""))
    if os.name == "nt":
        paths.update(root.glob("*.pth"))
        paths.update(root.glob("*.py"))
    paths.add(root)
    inventory_paths.add(root)
    for member in dist.files or []:
        path = Path(dist.locate_file(member))
        watch(path, root)
        if path.name == "RECORD":
            inventory_paths.add(path)
print(json.dumps({
    "paths": sorted(str(path) for path in paths),
    "inventory_paths": sorted(str(path) for path in inventory_paths),
}))
