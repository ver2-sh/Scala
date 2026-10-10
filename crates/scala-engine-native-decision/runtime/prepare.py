#!/usr/bin/env python3
"""Prepare explicit source bindings or an isolated external runtime; never load a model.

No profile, Scala Settings, runtime selection or production files are modified.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("native_runtime", HERE / "server.py")
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


def files(root, python_only=False):
    result = {}
    for p in sorted(root.rglob("*.py" if python_only else "*")):
        if not p.is_file() or any(part in {".git", ".cache", "__pycache__"} for part in p.relative_to(root).parts):
            continue
        runtime.regular(p)
        result[p.relative_to(root).as_posix()] = {"size_bytes": p.stat().st_size, "sha256": runtime.sha(p)}
    return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    subs = p.add_subparsers(dest="mode", required=True)
    b = subs.add_parser("bundle")
    b.add_argument("--backend", required=True, choices=sorted(runtime.BACKENDS))
    b.add_argument("--source", action="append", nargs=4, metavar=("ROLE", "PATH", "REPOSITORY", "REVISION"), required=True)
    b.add_argument("--binding", action="append", nargs=2, metavar=("ROLE", "FILE"), required=True)
    b.add_argument("--output", type=Path, required=True)
    r = subs.add_parser("runtime")
    r.add_argument("--backend", required=True, choices=sorted(runtime.BACKENDS))
    r.add_argument("--python", type=Path, required=True)
    r.add_argument("--source", type=Path, required=True)
    r.add_argument("--entrypoint", help="explicit label-shim source filename")
    r.add_argument("--source-revision", required=True)
    r.add_argument("--option", action="append", nargs=2, metavar=("NAME", "JSON_VALUE"), required=True)
    r.add_argument("--wheel-attestation", action="append", nargs=4,
                   metavar=("PACKAGE", "ORIGINAL_WHEEL", "WHEEL_SHA256", "MISMATCHES_JSON"),
                   help="pin an original upstream wheel and exact known bad RECORD members; never alter installed bytes")
    r.add_argument("--windows-vllm", type=Path, help="explicit pinned native Windows compatibility build, SDK and environment binding")
    r.add_argument("--output", type=Path, required=True)
    args = p.parse_args()
    if args.mode == "bundle":
        if args.output.exists():
            raise ValueError("Output exists; source descriptors are never overwritten")
        cfg = {"schema_version": 1, "backend": args.backend, "sources": {}, "bindings": dict(args.binding)}
        for role, path, repo, rev in args.source:
            if role in cfg["sources"] or len(rev) != 40 or any(c not in "0123456789abcdef" for c in rev):
                raise ValueError("Source roles must be unique and revisions exact")
            root = Path(path).resolve(strict=True)
            cfg["sources"][role] = {"path": str(root), "repository": repo, "revision": rev, "files": files(root)}
        if len(cfg["bindings"]) != len(args.binding):
            raise ValueError("Duplicate native binding")
        # Validate before publishing, using a temporary descriptor outside sources.
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            candidate = Path(tmp) / "candidate.decisionbundle"
            candidate.write_text(json.dumps(cfg, indent=2) + "\n")
            runtime.bundle(candidate)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with args.output.open("x") as out:
            json.dump(cfg, out, indent=2)
            out.write("\n")
        print(args.output)
    else:
        if args.output.exists():
            raise ValueError("Output exists; create a separate immutable runtime")
        python = args.python.absolute()
        # Keep the venv launcher path: resolving its symlink would switch environments.
        if not python.is_file() or not python.is_absolute():
            raise ValueError("Explicit runtime interpreter is missing")
        script = 'import importlib.metadata as m,json; print(json.dumps({d.metadata["Name"]:d.version for d in m.distributions()}))'
        packages = json.loads(subprocess.check_output([str(python), "-I", "-c", script], text=True))
        cfg = {"schema_version": 1, "backend": args.backend, "packages": packages, "options": {k: json.loads(v) for k, v in args.option}}
        if len(cfg["options"]) != len(args.option):
            raise ValueError("Duplicate runtime option")
        if args.windows_vllm:
            cfg["windows_vllm"] = runtime.read_json(args.windows_vllm)
        if args.wheel_attestation:
            att = {}
            for package, wheel, wheel_hash, mismatches in args.wheel_attestation:
                canonical = package.lower().replace("_", "-").replace(".", "-")
                if canonical in att:
                    raise ValueError("Duplicate original wheel attestation")
                att[canonical] = {"path": str(Path(wheel).resolve(strict=True)),
                                  "sha256": wheel_hash, "mismatches": json.loads(mismatches)}
            cfg["wheel_attestations"] = att
        if args.backend == "torch-readout":
            if args.source is None or not args.source_revision:
                raise ValueError("Exact native readout source checkout is required")
            root = args.source.resolve(strict=True)
            rev = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
            if rev != args.source_revision or subprocess.check_output(["git", "-C", str(root), "status", "--porcelain"], text=True):
                raise ValueError("Native implementation checkout is dirty or at a different revision")
            inventory = files(root / "src", True)
            inventory = {"src/" + k: v for k, v in inventory.items()}
            inventory.update({"scripts/" + k: v for k, v in files(root / "scripts", True).items()})
            cfg["source"] = {"path": str(root), "revision": rev, "files": inventory}
        if args.backend == "vllm-labels":
            if not args.entrypoint or len(args.source_revision) != 40:
                raise ValueError("Exact native label implementation source is required")
            root = args.source.resolve(strict=True)
            inventory = files(root, True)
            if args.entrypoint not in inventory:
                raise ValueError("Native label shim source is missing")
            cfg["source"] = {"path": str(root), "revision": args.source_revision, "entrypoint": args.entrypoint, "files": {args.entrypoint: inventory[args.entrypoint]}}
        # A Windows runtime launches the actual venv executable directly. Never
        # add a descriptor beside a shared/system interpreter.
        launcher = python.with_name("scala-native-decision.json") if os.name == "nt" else None
        if launcher:
            isolated = json.loads(subprocess.check_output([str(python), "-I", "-c",
                'import sys,json; print(json.dumps(sys.prefix != sys.base_prefix))'], text=True))
            if not isolated or launcher.exists():
                raise ValueError("Windows runtime requires an unbound dedicated virtual environment")
        args.output.mkdir(parents=True)
        launcher_created = False
        try:
            (args.output / "runtime.json").write_text(json.dumps(cfg, indent=2) + "\n")
            shutil.copyfile(HERE / "server.py", args.output / "server.py")
            if args.windows_vllm:
                shutil.copyfile(HERE / "windows_vllm.py", args.output / "windows_vllm.py")
            if launcher:
                with launcher.open("x", encoding="utf-8") as out:
                    launcher_created = True
                    json.dump({"protocol": runtime.PROTOCOL, "interpreter": str(python),
                               "runner": str((args.output / "server.py").resolve(strict=True))}, out)
                command = [str(python), "-I", str((args.output / "server.py").absolute())]
            else:
                entry = args.output / "native-decision-server"
                entry.write_text("#!/bin/sh\nexec " + shlex.quote(str(python)) + " -I " + shlex.quote(str((args.output / "server.py").absolute())) + ' "$@"\n')
                entry.chmod(0o755)
                command = [str(entry.absolute())]
            print(subprocess.check_output(command + ["--scala-probe"], text=True).strip())
        except BaseException:
            if launcher_created:
                launcher.unlink(missing_ok=True)
            shutil.rmtree(args.output)
            raise


if __name__ == "__main__":
    main()
