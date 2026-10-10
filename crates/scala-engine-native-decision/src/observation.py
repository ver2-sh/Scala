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


source = cfg["source"]
root = Path(source["path"])
for member in source["files"]:
    watch(root / member, root)
for directory, _, _ in os.walk(root):
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
