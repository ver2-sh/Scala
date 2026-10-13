"""Static/synthetic validation. Never import a real tensor runtime or score a model."""
import copy
import base64
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


class RunnerTests(unittest.TestCase):
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
            executable.with_name("scala-pytorch.json").write_text(json.dumps({
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
                loaded.clear()
                runtime.serve_readout(runtime.apply_settings(settings,{"fast":True}),cfg,"revision",args)
                self.assertTrue(loaded[1][1]["fast"])
                self.assertFalse(loaded[1][1]["merge_lora"])
                self.assertEqual(loaded[1][1]["rotations"],4)
                self.assertEqual(loaded[2][2]["calibration"],"native-calibrator")
                weight.device.type = "cpu"
                with self.assertRaisesRegex(ValueError, "complete model and readout on GPU"):
                    runtime.serve_readout(settings, cfg, "revision", args)
                weight.device.type = "cuda"
                native_backend.engine.readout = None
                with self.assertRaisesRegex(ValueError, "fallback forbidden"):
                    runtime.serve_readout(settings, cfg, "revision", args)

class ImageTransportTests(unittest.IsolatedAsyncioTestCase):
    @staticmethod
    def request(data):
        raw = json.dumps(data).encode()
        async def body():
            return raw
        async def json_body():
            return json.loads(raw)
        return types.SimpleNamespace(url=types.SimpleNamespace(path="/v1/systemone"),
                                     method="POST", body=body, json=json_body)

    async def dispatch(self, data):
        calls = []
        # No FastAPI installation or tensor imports needed for policy tests.
        response = lambda body, status_code=200: types.SimpleNamespace(body=body, status_code=status_code)
        native_response = response({"answers": {"q": {"type": "noul", "noul": 0.73,
                                   "unknown_probability": 0.04, "abstained": False}}})
        request = self.request(data)
        async def next_request(received):
            self.assertIs(received, request)
            calls.append(await received.json())
            return native_response
        with patch.dict(sys.modules, {"fastapi.responses": types.SimpleNamespace(JSONResponse=response)}):
            result = await runtime.qualify_readout_request(request, next_request, {}, {})
        return result, calls, native_response

    async def test_images_and_text_delegate_original_request_and_native_response(self):
        for images in (None, [], ["data:image/png;base64,YQ=="],
                       ["data:image/jpeg;base64,YQ==", "data:image/webp;base64,Yg=="]):
            data = {"state": {"record": 7}, "questions": {"q": {"type": "noul", "instructions": "compare"}}}
            if images is not None:
                data["images"] = images
            result, calls, native = await self.dispatch(data)
            self.assertIs(result, native)
            self.assertEqual(calls, [data])

    async def test_invalid_images_and_generation_fields_never_reach_backend(self):
        for images in (None, "data:image/png;base64,YQ==", [1],
                       ["https://example.com/image.png"], ["data:image/gif;base64,YQ=="],
                       ["data:image/png;base64,!"], ["data:image/png;base64,"],
                       ["data:image/png;base64,YQ=="] * 3,
                       ["data:image/png;base64," + base64.b64encode(b"x" * (runtime.MAX_IMAGE_BYTES + 1)).decode()]):
            data = {"state": "context", "questions": {"q": {"type": "noul", "instructions": "check"}}, "images": images}
            result, calls, _ = await self.dispatch(data)
            self.assertEqual(result.status_code, 422)
            self.assertEqual(calls, [])
        for field in ("thinking", "messages", "temperature"):
            data = {"state": "context", "questions": {"q": {"type": "noul", "instructions": "check"}}, field: True}
            result, calls, _ = await self.dispatch(data)
            self.assertEqual(result.status_code, 422)
            self.assertEqual(calls, [])

    async def test_two_eight_mib_images_fit_without_changing_body_limit(self):
        image = "data:image/png;base64," + base64.b64encode(b"x" * runtime.MAX_IMAGE_BYTES).decode()
        data = {"state": "context", "questions": {"q": {"type": "noul", "instructions": "check"}}, "images": [image, image]}
        self.assertLess(len(json.dumps(data).encode()), 32 * 1024 * 1024)
        result, calls, native = await self.dispatch(data)
        self.assertIs(result, native)
        self.assertEqual(calls, [data])

    async def test_upstream_validation_error_is_preserved(self):
        data = {"state": "context", "questions": {"q": {"type": "noul", "instructions": "check"}},
                "images": ["data:image/png;base64,YQ=="]}
        # Valid transport with invalid raster bytes: the authoritative upstream
        # handler must still receive it and its rejection must survive unchanged.
        error = types.SimpleNamespace(status_code=422, body={"error": "bad_image"})
        async def upstream(request):
            self.assertEqual(await request.json(), data)
            return error
        with patch.dict(sys.modules, {"fastapi.responses": types.SimpleNamespace(JSONResponse=lambda *a, **kw: None)}):
            result = await runtime.qualify_readout_request(self.request(data), upstream, {}, {})
        self.assertIs(result, error)


class RunnerContractTests(unittest.TestCase):
    def test_fast_is_explicit_and_merge_is_unqualified(self):
        cfg={"backend":"torch-readout","options":{"device":"cuda","rotations":4,"max_input_tokens":4096}}
        self.assertEqual(runtime.runtime_defaults(cfg),{"fast":False})
        self.assertTrue(runtime.apply_settings(cfg,{"fast":True})["options"]["fast"])
        self.assertFalse(runtime.apply_settings(cfg,{"fast":False})["options"]["fast"])
        for override in ({"merge_lora":True},{"fast":"true"},{"device":"cpu"},{"calibration":0.5}):
            with self.assertRaises(ValueError):runtime.apply_settings(cfg,override)
        self.assertEqual(cfg["options"]["device"],"cuda")

if __name__ == "__main__": unittest.main()
