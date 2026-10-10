"""Static/synthetic validation. Never import a real tensor runtime or score a model."""
import copy
import importlib.util
import io
import json
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
            p.symlink_to(path)
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

    def test_torch_dispatch_uses_trained_readout_and_native_calibrator(self):
        with tempfile.TemporaryDirectory() as tmp:
            path, cfg = self.fixture(Path(tmp), "torch-readout")
            loaded = []
            torch = types.SimpleNamespace(float32="F32")
            native_backend = types.SimpleNamespace(engine=types.SimpleNamespace(readout=types.SimpleNamespace(weight=types.SimpleNamespace(dtype="F32"))), torch=torch)
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


if __name__ == "__main__":
    unittest.main()
