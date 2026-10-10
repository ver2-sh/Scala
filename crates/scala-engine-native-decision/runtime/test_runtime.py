"""Static/synthetic validation. Never import a real tensor runtime or score a model."""
import copy
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch
import sys
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("native_runtime", Path(__file__).with_name("server.py"))
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


class ClosureTests(unittest.TestCase):
    def interpreter(self, root):
        stack = contextlib.ExitStack()
        if runtime.os.name == "nt":
            base, env = root / "python", root / "env"
            base.mkdir()
            (env / "Scripts").mkdir(parents=True)
            executable = env / "Scripts/python.exe"
            executable.write_bytes(b"synthetic redirector")
            (base / "python.exe").write_bytes(b"synthetic interpreter")
            (base / "python312.dll").write_bytes(b"synthetic DLL")
            (env / "pyvenv.cfg").write_text("synthetic virtual environment")
            (env / "Lib/site-packages").mkdir(parents=True)
            (env / "Lib/site-packages/_virtualenv.py").write_text("# synthetic activation hook\n")
            executable.with_name("scala-native-decision.json").write_text(json.dumps({
                "protocol": runtime.PROTOCOL, "interpreter": str(executable),
                "runner": str(Path(runtime.__file__).resolve())}))
            for name, value in (("executable", str(executable)), ("prefix", str(env)), ("base_prefix", str(base))):
                stack.enter_context(patch.object(runtime.sys, name, value))
        return stack

    def fixture(self, root, backend):
        sources = {}
        roles = {"model": ["config.json", "model.safetensors", "tokenizer.json", "tokenizer_config.json", "shim.py", "serve.json"]} if backend == "vllm-labels" else {"adapter": ["adapter_config.json", "adapter_model.safetensors", "decision_readout.json", "decision_readout.safetensors", "calibration.json"], "base": ["config.json", "model.safetensors"]}
        for role, names in roles.items():
            directory = root / role
            directory.mkdir()
            files = {}
            for name in names:
                content = {"head_dtype": "float32"} if name == "config.json" else {"base_model_name_or_path": "/training/models--fixture--base/snapshots/" + "b" * 40, "revision": None} if name == "adapter_config.json" else {"model": "fixture"} if name == "serve.json" else {}
                (directory / name).write_text(json.dumps(content))
                files[name] = {"size_bytes": (directory / name).stat().st_size, "sha256": runtime.sha(directory / name)}
            sources[role] = {"path": str(directory), "repository": "fixture/base" if role == "base" else "fixture/model", "revision": "b" * 40, "files": files}
        bindings = {"shim": "shim.py", "serve_config": "serve.json"} if backend == "vllm-labels" else {"calibration": "calibration.json"}
        cfg = {"schema_version": 1, "backend": backend, "sources": sources, "bindings": bindings}
        path = root / "fixture.decisionbundle"
        path.write_text(json.dumps(cfg))
        return path, cfg

    def test_source_closure_is_verified_without_tensor_imports(self):
        for backend in runtime.BACKENDS:
            with self.subTest(backend=backend), tempfile.TemporaryDirectory() as tmp:
                path, cfg = self.fixture(Path(tmp), backend)
                self.assertEqual(runtime.bundle(path), cfg)
                role = "model" if backend == "vllm-labels" else "adapter"
                weights = Path(cfg["sources"][role]["path"]) / ("model.safetensors" if backend == "vllm-labels" else "decision_readout.safetensors")
                weights.write_text("[]")  # same size, wrong content
                with self.assertRaisesRegex(ValueError, "changed"):
                    runtime.bundle(path)

    def test_missing_readout_and_unbound_inputs_fail_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, cfg = self.fixture(Path(tmp), "torch-readout")
            del cfg["sources"]["adapter"]["files"]["decision_readout.safetensors"]
            path.write_text(json.dumps(cfg))
            with self.assertRaisesRegex(ValueError, "unbound"):
                runtime.bundle(path)
            (Path(cfg["sources"]["adapter"]["path"]) / "decision_readout.safetensors").unlink()
            with self.assertRaisesRegex(ValueError, "readout"):
                runtime.bundle(path)

    def test_traversal_and_symlinks_cannot_bind_native_inputs(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, cfg = self.fixture(Path(tmp), "vllm-labels")
            source = cfg["sources"]["model"]
            source["files"]["../escape"] = next(iter(source["files"].values()))
            path.write_text(json.dumps(cfg))
            with self.assertRaisesRegex(ValueError, "Unsafe"):
                runtime.bundle(path)
            del source["files"]["../escape"]
            p = Path(source["path"]) / "model.safetensors"
            p.unlink()
            try:
                p.symlink_to(path)
            except OSError as error:
                if os.name != "nt" or error.winerror != 1314:
                    raise
                # Native Windows directory junctions require no symlink
                # privilege. Exercise canonical alias rejection without
                # changing Developer Mode or security policy.
                import subprocess
                p.write_bytes(b"synthetic")
                alias = Path(tmp) / "alias"
                subprocess.run(["cmd.exe", "/d", "/c", "mklink", "/J", str(alias), source["path"]],
                               check=True, stdout=subprocess.DEVNULL)
                source["path"] = str(alias)
                path.write_text(json.dumps(cfg))
                with self.assertRaisesRegex(ValueError, "Invalid source root"):
                    runtime.bundle(path)
                return
            path.write_text(json.dumps(cfg))
            with self.assertRaisesRegex(ValueError, "canonical"):
                runtime.bundle(path)

    def test_fp32_label_head_and_exact_base_binding_are_required(self):
        for backend in runtime.BACKENDS:
            with tempfile.TemporaryDirectory() as tmp:
                path, cfg = self.fixture(Path(tmp), backend)
                role, file = ("model", "config.json") if backend == "vllm-labels" else ("adapter", "adapter_config.json")
                source = cfg["sources"][role]
                p = Path(source["path"]) / file
                p.write_text(json.dumps({"head_dtype": "bfloat16"} if backend == "vllm-labels" else {"base_model_name_or_path": "wrong/model"}))
                source["files"][file] = {"size_bytes": p.stat().st_size, "sha256": runtime.sha(p)}
                path.write_text(json.dumps(cfg))
                with self.assertRaises(ValueError):
                    runtime.bundle(path)

    def test_runtime_missing_dependencies_never_claims_a_protocol(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "runtime.json"
            p.write_text(json.dumps({"schema_version": 1, "backend": "vllm-labels", "packages": {}, "options": {"max_model_len": 40960, "min_context": 4096, "gpu_memory_utilization": 0.90}}))
            with self.assertRaises(ValueError):
                runtime.runtime_identity(p)

    def test_runtime_accepts_canonical_wheel_names_and_still_verifies_records(self):
        import base64
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            members = ["src/fixture.py", "scripts/torch_decision.py", "scripts/playground/server.py"]
            for name in members:
                p = root / name
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text("# synthetic\n")
            wheel_file = root / "wheel.py"
            wheel_file.write_text("# synthetic wheel\n")
            record = types.SimpleNamespace(hash=types.SimpleNamespace(mode="sha256", value=base64.urlsafe_b64encode(bytes.fromhex(runtime.sha(wheel_file))).decode().rstrip("=")))
            packages = {name: "1" for name in ["torch", "peft", "transformers", "safetensors", "fastapi", "uvicorn", "pydantic", "pillow"]}
            cfg = {"schema_version": 1, "backend": "torch-readout", "packages": packages,
                   "options": {"rotations": 4, "max_input_tokens": 4096, "device": "cuda"},
                   "source": {"path": str(root), "revision": "a" * 40, "files": {name: {"size_bytes": (root / name).stat().st_size, "sha256": runtime.sha(root / name)} for name in members}}}
            path = root / "runtime.json"
            path.write_text(json.dumps(cfg))
            distributions = {name: types.SimpleNamespace(version="1", metadata={"Name": name}, files=[record], locate_file=lambda _: wheel_file) for name in packages}
            with self.interpreter(root), patch.object(runtime.importlib.metadata, "distribution", side_effect=distributions.__getitem__), patch.object(runtime.importlib.metadata, "distributions", return_value=list(distributions.values())):
                observed, revision = runtime.runtime_identity(path)
                self.assertEqual(observed, cfg)
                if runtime.os.name == "nt":
                    hook = Path(runtime.sys.prefix) / "Lib/site-packages/_virtualenv.py"
                    hook.write_text("# changed activation hook\n")
                    self.assertNotEqual(runtime.runtime_identity(path)[1], revision)
                wheel_file.write_text("# changed wheel\n")
                with self.assertRaisesRegex(ValueError, "Runtime package file changed"):
                    runtime.runtime_identity(path)

    def test_pinned_original_wheel_can_attest_only_exact_known_bad_record_bytes(self):
        import base64
        import zipfile
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "shim.py"
            source.write_text("# pinned native shim\n")
            wheel_member = "PyNvVideoCodec/native.so"
            member = root / wheel_member
            member.parent.mkdir()
            member.write_bytes(b"original wheel member")
            bogus_record = base64.urlsafe_b64encode(bytes.fromhex(runtime.sha(source))).decode().rstrip("=")
            archive = root / "pynvvideocodec-2.0.4-py3-none-any.whl"
            record_path = "pynvvideocodec-2.0.4.dist-info/RECORD"
            with zipfile.ZipFile(archive, "w") as z:
                z.write(member, wheel_member)
                z.writestr(record_path, f"{wheel_member},sha256={bogus_record},123\n{record_path},,\n")
            packages = {n: "0.30.0" if n == "vllm" else "2.0.4" if n == "pynvvideocodec" else "1"
                        for n in ["vllm", "torch", "transformers", "safetensors", "pynvvideocodec"]}
            dist = {}
            class RecordedFile:
                def __init__(self, name, hash_value):
                    self.name = name
                    self.hash = types.SimpleNamespace(mode="sha256", value=hash_value)
                def __str__(self):
                    return self.name
            correct = base64.urlsafe_b64encode(bytes.fromhex(runtime.sha(source))).decode().rstrip("=")
            for name in packages:
                origin = member if name == "pynvvideocodec" else source
                record = RecordedFile(wheel_member if name == "pynvvideocodec" else "shim.py", bogus_record if name == "pynvvideocodec" else correct)
                dist[name] = types.SimpleNamespace(version=packages[name], metadata={"Name": name},
                     files=[record], locate_file=lambda _file, path=origin: path)
            attestation = {"path": str(archive), "sha256": runtime.sha(archive),
                           "mismatches": {wheel_member: runtime.sha(member)}}
            cfg = {"schema_version": 1, "backend": "vllm-labels", "packages": packages,
                   "options": {"max_model_len": 40960, "min_context": 4096, "gpu_memory_utilization": 0.4},
                   "source": {"path": str(root), "revision": "a" * 40,
                              "entrypoint": "shim.py", "files": {"shim.py": {"size_bytes": source.stat().st_size, "sha256": runtime.sha(source)}}},
                   "wheel_attestations": {"pynvvideocodec": attestation}}
            path = root / "runtime.json"
            with self.interpreter(root), patch.object(runtime.importlib.metadata, "distribution", side_effect=dist.__getitem__), patch.object(runtime.importlib.metadata, "distributions", return_value=list(dist.values())):
                path.write_text(json.dumps(cfg))
                self.assertEqual(runtime.runtime_identity(path)[0], cfg)
                unbound = copy.deepcopy(cfg)
                del unbound["wheel_attestations"]
                path.write_text(json.dumps(unbound))
                with self.assertRaisesRegex(ValueError, "Runtime package file changed"):
                    runtime.runtime_identity(path)
                path.write_text(json.dumps(cfg))
                member.write_bytes(b"untrusted changed bytes")
                with self.assertRaisesRegex(ValueError, "Runtime package file changed"):
                    runtime.runtime_identity(path)
                member.write_bytes(b"original wheel member")
                attestation["sha256"] = "0" * 64
                path.write_text(json.dumps(cfg))
                with self.assertRaisesRegex(ValueError, "archive SHA256"):
                    runtime.runtime_identity(path)

    def test_torch_dispatch_uses_trained_readout_and_native_calibrator(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, cfg = self.fixture(Path(tmp), "torch-readout")
            loaded = []
            cuda = types.SimpleNamespace(is_available=lambda: True, get_device_name=lambda: "synthetic CUDA", memory_allocated=lambda: 1, memory_reserved=lambda: 2)
            torch = types.SimpleNamespace(float32="F32", cuda=cuda)
            weight = types.SimpleNamespace(dtype="F32", device=types.SimpleNamespace(type="cuda"))
            model = types.SimpleNamespace(parameters=lambda: [weight])
            native_backend = types.SimpleNamespace(engine=types.SimpleNamespace(readout=types.SimpleNamespace(weight=weight), model=model), torch=torch)
            class App:
                def middleware(self, *_):
                    return lambda function: function
            native = types.SimpleNamespace(TorchBackend=lambda **kw: loaded.append(("backend", kw)) or native_backend, create_app=lambda backend, **kw: loaded.append(("app", backend, kw)) or App())
            calibrator = types.SimpleNamespace(TemperatureCalibrator=types.SimpleNamespace(load=lambda p: loaded.append(("calibration", p)) or "native-calibrator"))
            fake_modules = {"vision_decision": types.ModuleType("vision_decision"), "vision_decision.calibration": calibrator, "fastapi": types.ModuleType("fastapi"), "fastapi.responses": types.SimpleNamespace(JSONResponse=object), "uvicorn": types.SimpleNamespace(run=lambda *a, **k: None)}
            args = types.SimpleNamespace(bundle=str(path), host="127.0.0.1", port=1, launch_nonce="nonce")
            settings = {"source": {"path": str(Path(tmp))}, "options": {"rotations": 4, "max_input_tokens": 4096, "device": "cuda"}}
            with patch.object(runtime, "module", return_value=native), patch.dict(sys.modules, fake_modules):
                runtime.serve_readout(settings, cfg, "revision", args)
                self.assertEqual(loaded[0][0], "calibration")
                self.assertEqual(loaded[1][1]["adapter"], Path(cfg["sources"]["adapter"]["path"]))
                self.assertEqual(loaded[1][1]["rotations"], 4)
                self.assertFalse(loaded[1][1]["fast"])
                self.assertFalse(loaded[1][1]["merge_lora"])
                self.assertEqual(loaded[2][2]["calibration"], "native-calibrator")
                weight.device.type = "cpu"
                with self.assertRaisesRegex(ValueError, "complete model and readout on GPU"):
                    runtime.serve_readout(settings, cfg, "revision", args)
                weight.device.type = "cuda"
                native_backend.engine.readout = None
                with self.assertRaisesRegex(ValueError, "fallback forbidden"):
                    runtime.serve_readout(settings, cfg, "revision", args)

    def test_h2o_dispatch_delegates_to_native_shim_without_text_parsing(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, cfg = self.fixture(Path(tmp), "vllm-labels")
            request = {"state": [{"record": 7}], "questions": {"x": {"type": "noul", "instructions": ["test"]}}}
            calls = []
            class Handler:
                def _send(self, status, body):
                    calls.append((status, body))
            def server(address, handler):
                def run():
                    instance = handler()
                    instance.path = "/v1/systemone"
                    body = json.dumps(request).encode()
                    instance.rfile = io.BytesIO(body)
                    instance.headers = {"Content-Length": str(len(body))}
                    instance.do_POST()
                return types.SimpleNamespace(serve_forever=run)
            class Error(Exception):
                pass
            native = types.SimpleNamespace(Contract=lambda cfg: types.SimpleNamespace(model=cfg["model"]), VLLM=lambda *a: "native-vllm", Shim=lambda *a: types.SimpleNamespace(decide=lambda b: calls.append(("native", b)) or {"answers": {"x": {"type": "noul", "noul": 0.801}}}), make_handler=lambda shim: Handler, Server=server, UpstreamError=Error, Unprocessable=Error, BadRequest=Error)
            child = types.SimpleNamespace(terminate=lambda: calls.append(("terminate",)), wait=lambda **k: None)
            args = types.SimpleNamespace(bundle=str(path), host="127.0.0.1", port=1, launch_nonce="nonce")
            options = {"source": {"path":cfg["sources"]["model"]["path"],"entrypoint":"shim.py","files":{"shim.py":cfg["sources"]["model"]["files"]["shim.py"]}}, "options": {"max_model_len": 40960, "gpu_memory_utilization": 0.90, "min_context": 4096}}
            with patch.object(runtime, "module", return_value=native), patch.object(runtime.subprocess, "Popen", return_value=child) as spawn, patch.object(runtime.signal, "signal"):
                runtime.serve_labels(options, cfg, "revision", args)
                self.assertEqual(calls[0], ("native", request))
                self.assertEqual(calls[1], (200, {"answers": {"x": {"type": "noul", "noul": 0.801}}}))
                self.assertIn("vllm.entrypoints.openai.api_server", spawn.call_args.args[0])
                self.assertIn(cfg["sources"]["model"]["path"], spawn.call_args.args[0])



class WindowsVllmTests(unittest.TestCase):
    def module(self):
        spec = importlib.util.spec_from_file_location("windows_port_tests", Path(__file__).with_name("windows_vllm.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def config(self, port, root):
        return {"backend": "vllm-labels", "packages": dict(port.VERSIONS),
                "options": {"enforce_eager": True, "max_num_seqs": 4, "max_num_batched_tokens": 2048, "cpu_offload_gb": 0},
                "windows_vllm": {"fork_revision": port.FORK_REVISION,
                    "wheel": {"path": str(root / "original.whl"), "sha256": port.WHEEL_SHA256},
                    "sdk": {"path": str(root), "files": {}},
                    "environment": {**{name: str(root) for name in port.DIRECTORIES}, "USERNAME": "synthetic", "VLLM_USE_FLASHINFER_SAMPLER": "0"}}}

    def test_build_version_platform_options_and_environment_are_strict(self):
        port = self.module()
        with tempfile.TemporaryDirectory() as temp:
            cfg = self.config(port, Path(temp).resolve())
            self.assertEqual(port.validate(cfg, True), cfg["windows_vllm"])
            with self.assertRaises(ValueError): port.validate(cfg, False)
            for mutate in (lambda c: c["packages"].update(vllm="0.30.0"),
                           lambda c: c["windows_vllm"].update(fork_revision="0" * 40),
                           lambda c: c["options"].update(cpu_offload_gb=1),
                           lambda c: c["options"].update(enforce_eager=False),
                           lambda c: c["windows_vllm"]["environment"].update(PATH="unbound"),
                           lambda c: c["windows_vllm"]["wheel"].update(sha256="0" * 64)):
                changed = copy.deepcopy(cfg); mutate(changed)
                with self.assertRaises(ValueError): port.validate(changed, True)
            command = port.command("C:/native python.exe", "D:/model 空", "source/model", 1234,
                                   {**cfg, "options": {**cfg["options"], "max_model_len": 2048, "gpu_memory_utilization": 0.8}})
            self.assertEqual(command[:6], ["C:/native python.exe", "-I", "-m", "vllm.entrypoints.cli.main", "serve", "D:/model 空"])
            self.assertIn("--enforce-eager", command)


if __name__ == "__main__":
    unittest.main()
