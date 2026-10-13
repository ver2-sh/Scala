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
            executable.with_name("scala-vllm.json").write_text(json.dumps({
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
                calls.clear()
                request["images"] = ["data:image/png;base64,YQ=="]
                runtime.serve_labels(options, cfg, "revision", args)
                self.assertEqual(calls[0][0], 422)
                self.assertFalse(any(call[0] == "native" for call in calls))

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


class GenerationTests(unittest.TestCase):
    def snapshot(self, root):
        for name,value in {"config.json":{"architectures":["LlamaForCausalLM"]},"tokenizer_config.json":{"chat_template":"{{ messages }}"},"tokenizer.json":{},"generation_config.json":{"temperature":0.7}}.items():
            (root/name).write_text(json.dumps(value))
        (root/"model.safetensors").write_bytes(b"synthetic weights")
        return root/"config.json"

    def test_native_exec_preserves_snapshot_and_qualified_load_options(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp).resolve();config=self.snapshot(root)
            snapshot,digest=runtime.snapshot_fingerprint(config)
            args=types.SimpleNamespace(model_config=str(config),model_sha256=digest,launch_nonce="nonce",host="127.0.0.1",port=1234)
            cfg={"backend":"vllm-openai","options":{"max_model_len":2048,"gpu_memory_utilization":0.8,"enforce_eager":True}}
            with patch.object(runtime.os,"execv") as execute, patch.dict(runtime.os.environ,clear=True):
                runtime.serve_generation(cfg,args)
                command=execute.call_args.args[1]
                self.assertEqual(command[:5],[sys.executable,"-I","-m","vllm.entrypoints.cli.main","serve"])
                self.assertIn(str(root),command);self.assertIn("scala-nonce",command)
                self.assertIn("--enforce-eager",command)
                self.assertNotIn("--chat-template",command);self.assertNotIn("--generation-config",command)
                self.assertEqual(runtime.os.environ["HF_HUB_OFFLINE"],"1")
            (root/"model.safetensors").write_bytes(b"changed weights!")
            self.assertNotEqual(runtime.snapshot_fingerprint(config)[1],digest)
            with patch.object(runtime.os,"execv") as execute:
                with self.assertRaisesRegex(ValueError,"changed before execution"): runtime.serve_generation(cfg,args)
                execute.assert_not_called()
            (root/"unqualified.py").write_text("# model code")
            with self.assertRaises(ValueError): runtime.snapshot_fingerprint(config)

    def test_native_windows_keeps_owned_parent_and_cleans_failed_child(self):
        calls=[]
        child=types.SimpleNamespace(wait=lambda **kw:calls.append("wait") or 0,poll=lambda:0,terminate=lambda:calls.append("terminate"))
        with patch.object(runtime.subprocess,"Popen",return_value=child) as spawn,patch.object(runtime.os,"execv") as execute:
            runtime.launch_native_server(["C:/env/python.exe","-I","-m","vllm.entrypoints.cli.main"],True)
            execute.assert_not_called();spawn.assert_called_once();self.assertEqual(calls,["wait"])
        calls=[]
        count=[0]
        def wait(**kw):
            count[0]+=1
            if count[0]==1: raise KeyboardInterrupt
            calls.append("joined");return 0
        child=types.SimpleNamespace(wait=wait,poll=lambda:None,terminate=lambda:calls.append("terminate"))
        with patch.object(runtime.subprocess,"Popen",return_value=child):
            with self.assertRaises(KeyboardInterrupt):runtime.launch_native_server(["C:/env/python.exe"],True)
        self.assertEqual(calls,["terminate","joined"])

    def test_load_and_runner_overrides_are_separate_from_trained_scoring(self):
        cfg={"backend":"vllm-labels","options":{"min_context":1024,"max_model_len":2048,"gpu_memory_utilization":0.8}}
        with self.assertRaises(ValueError):runtime.apply_settings(cfg,{"temperature":0.7})
        self.assertEqual(runtime.apply_settings(cfg,{"context_length":4096})["options"]["max_model_len"],4096)
        with self.assertRaises(ValueError):runtime.apply_settings({**cfg,"windows_vllm":{}},{"gpu_memory_utilization":0.7})

if __name__ == "__main__": unittest.main()
