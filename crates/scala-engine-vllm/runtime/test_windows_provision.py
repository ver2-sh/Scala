"""Offline provisioning/contract tests; no tensor imports or model execution."""
import base64
import csv
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import types
import unittest
from unittest.mock import patch
import zipfile
import sys
sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures/h2o"


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


plan = module("h2o_plan", HERE / "windows/provision_h2o.py")
port = module("windows_port", HERE / "windows_vllm.py")
shim = module("original_h2o_shim", FIXTURES / "h2o_lightning_shim.py")


class ProvisionTests(unittest.TestCase):
    def test_lock_split_is_complete_and_conflicting_wheels_never_share_a_site(self):
        text = (HERE / "windows/h2o-requirements.lock").read_text()
        primary, auxiliary = plan.locks(text)
        self.assertNotIn("torch-c-dlpack-ext==", primary)
        self.assertIn("apache-tvm-ffi==0.1.14.post1", primary)
        self.assertIn("torch-c-dlpack-ext==0.1.5", auxiliary)
        self.assertNotIn("apache-tvm-ffi==", auxiliary)
        self.assertIn(plan.HUMMING_WHEEL_SHA256, primary)
        self.assertIn(port.WHEEL_SHA256, primary)
        self.assertNotIn("D:/", text)
        for bad in (text + "vllm==1 --hash=sha256:abc\n", text.replace("vllm==", "vllm>=")):
            with self.assertRaises(ValueError):
                plan.locks(bad)

    def test_canonical_wheel_matches_windows_generated_line_endings_and_record(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            outputs = []
            for i, newline in enumerate((b"\n", b"\r\n")):
                raw = root / f"{i}.whl"
                with zipfile.ZipFile(raw, "w") as z:
                    z.writestr("humming/code.py", b"# original payload\n")
                    z.writestr("humming/_version.py", b"version = '0.1.15'" + newline)
                    z.writestr("humming_kernels-0.1.15.dist-info/METADATA", b"Version: 0.1.15" + newline)
                    z.writestr("humming_kernels-0.1.15.dist-info/RECORD", b"old build-specific record")
                output = root / f"normalized-{i}.whl"
                plan.normalize_wheel(raw, output)
                outputs.append(output.read_bytes())
                with zipfile.ZipFile(output) as z:
                    for name, digest, size in csv.reader(io.StringIO(z.read("humming_kernels-0.1.15.dist-info/RECORD").decode())):
                        if digest:
                            data = z.read(name)
                            self.assertEqual(int(size), len(data))
                            self.assertEqual(digest, "sha256=" + base64.urlsafe_b64encode(hashlib.sha256(data).digest()).decode().rstrip("="))
            self.assertEqual(*outputs)

    def test_download_hash_is_checked_before_publication_and_on_reuse(self):
        with tempfile.TemporaryDirectory() as temp:
            target = Path(temp) / "archive"
            digest = hashlib.sha256(b"pinned").hexdigest()
            with patch.object(plan.urllib.request, "urlopen", return_value=io.BytesIO(b"wrong")):
                with self.assertRaisesRegex(ValueError, "SHA256 mismatch"):
                    plan.download("https://fixture", target, digest)
            self.assertFalse(target.exists())
            self.assertFalse(target.with_suffix(".partial").exists())
            target.write_bytes(b"pinned")
            with patch.object(plan.urllib.request, "urlopen", side_effect=AssertionError("rerun network")):
                self.assertEqual(plan.download("https://fixture", target, digest), target)
                target.write_bytes(b"tampered")
                with self.assertRaisesRegex(ValueError, "Cached acquisition changed"):
                    plan.download("https://fixture", target, digest)

    def test_archives_reject_aliases_traversal_and_sdk_conflicts(self):
        for path in ("../escape", "/absolute", "C:/escape", "a/../escape", "a\\escape"):
            with self.assertRaises(ValueError):
                plan.relative(path)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = root / "source.tar"
            with tarfile.open(archive, "w") as t:
                m = tarfile.TarInfo("root/alias")
                m.type = tarfile.SYMTYPE
                m.linkname = "../escape"
                t.addfile(m)
            with self.assertRaises(ValueError):
                plan.unpack_source(archive, root / "out")
            for i, payload in enumerate((b"sdk", b"sdk", b"conflict")):
                zip_path = root / f"sdk-{i}.zip"
                with zipfile.ZipFile(zip_path, "w") as z:
                    z.writestr("component/include/cuda.h", payload)
                if i == 2:
                    with self.assertRaisesRegex(ValueError, "Conflicting"):
                        plan.sdk_payload(zip_path, "crt", root / "sdk")
                else:
                    plan.sdk_payload(zip_path, "crt", root / "sdk")

    def test_description_binds_every_sdk_member_and_exact_environment(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            sdk = root / "sdk"
            sdk.mkdir()
            (sdk / "cuda.h").write_bytes(b"fixture")
            build = plan.build_description(root, "h2o", root / "vllm.whl", sdk, port)
            cfg = {"backend": "vllm-labels", "packages": port.VERSIONS, "windows_vllm": build,
                   "options": {"enforce_eager": True, "cpu_offload_gb": 0, "max_num_seqs": 4, "max_num_batched_tokens": 2048}}
            self.assertEqual(port.validate(cfg, True), build)
            self.assertEqual(build["sdk"]["files"], plan.inventory(sdk))
            self.assertEqual(set(build["environment"]), port.DIRECTORIES | {"USERNAME", "VLLM_USE_FLASHINFER_SAMPLER"})

    def test_completed_rerun_only_probes_and_incomplete_environment_fails(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp).resolve()
            (root / "h2o-runtime").mkdir()
            receipt = root / "h2o-runtime-probe.json"
            receipt.write_text('{"revision":"fixture"}')
            argv = ["provision", "--runtime-root", str(root), "--model-root", str(root), "--evidence-root", str(root)]
            with patch.object(sys, "argv", argv), patch.object(plan, "native_host", return_value=True), patch.object(plan, "run", return_value='{"revision":"fixture"}') as run:
                plan.main()
                self.assertEqual(run.call_count, 1)
                self.assertEqual(run.call_args.args[-1], "--scala-probe")
                receipt.write_text('{"revision":"different"}')
                with self.assertRaisesRegex(ValueError, "immutable runtime changed"):
                    plan.main()
                (root / "h2o-runtime").rmdir()
                (root / "h2o-env").mkdir()
                with self.assertRaisesRegex(ValueError, "Incomplete named environment"):
                    plan.main()
                (root / "h2o-env").rmdir()
                with self.assertRaisesRegex(ValueError, "Evidence name already exists"):
                    plan.main()


class H2OContractTests(unittest.TestCase):
    def test_exact_original_sources_and_complete_label_chunking(self):
        self.assertEqual(plan.sha(FIXTURES / "h2o_lightning_shim.py"), plan.MODEL_FILES["h2o_lightning_shim.py"])
        self.assertEqual(plan.sha(FIXTURES / "serve_config.json"), plan.MODEL_FILES["serve_config.json"])
        engine = shim.VLLM("http://synthetic", "h2o")
        calls = []
        def post(path, body):
            calls.append((path, body))
            return {"choices": [{"text": "ignored", "logprobs": {"top_logprobs": [{f"token_id:{i}": -float(i) for i in body["logprob_token_ids"]}]}}], "usage": {"prompt_tokens": 17}}
        engine.post = post
        ids = list(range(255))
        observed, usage = engine.label_logprobs("same answer slot", ids)
        self.assertEqual(set(observed), set(ids))
        self.assertEqual([len(b["logprob_token_ids"]) for _, b in calls], [128, 127])
        self.assertEqual(usage, {"prompt_tokens": 17})
        for path, body in calls:
            self.assertEqual(path, "/v1/completions")
            self.assertEqual(body["prompt"], "same answer slot")
            self.assertEqual(body["max_tokens"], 1)
            self.assertFalse(body["add_special_tokens"])
        engine.post = lambda *_: {"choices": [{"logprobs": {"top_logprobs": [{"token_id:0": -1}]}}]}
        with self.assertRaisesRegex(shim.UpstreamError, "omitted label"):
            engine.label_logprobs("slot", [0, 1])

    def test_tokenizer_requires_distinct_single_tokens_at_the_exact_answer_slot(self):
        cfg = json.loads((FIXTURES / "serve_config.json").read_text())
        contract = shim.Contract(cfg, env={})
        for bad in (False, True):
            engine = shim.VLLM("http://synthetic", contract.model)
            engine.call = lambda *_: (200, {"data": [{"id": contract.model, "max_model_len": 2048}]})
            engine.tokenize = lambda text: [1, 2] + ([99 if bad else contract.labels.index(text.rsplit(" ", 1)[-1]) + 3] if text.rsplit(" ", 1)[-1] in contract.labels else [])
            native = shim.Shim(contract, engine, 1024)
            try:
                if bad:
                    with self.assertRaisesRegex(shim.UpstreamError, "one distinct token"):
                        native.check_backend()
                else:
                    native.check_backend()
                    self.assertEqual(len(contract.label_ids), 255)
            finally:
                native.pool.shutdown()

    def test_calibration_typed_answers_and_nonfinite_values(self):
        self.assertEqual(shim.format_answer("noul", ["yes", "no"], ["", ""], [0, 0], 0.8, 0.801), {"type": "noul", "noul": 0.801})
        choice = shim.format_answer("choice", ["a", "b"], ["", ""], [-2, -1], 0.8, 0.801)
        self.assertEqual(choice["choice"], "b")
        self.assertEqual(set(choice["probabilities"]), {"a", "b"})
        score = shim.format_answer("score", ["0", "1"], ["low", "high"], [-2, -1], 0.8, 0.801)
        self.assertEqual(score["score"], choice["probabilities"]["b"])
        self.assertEqual(score["legend"], {"0": "low", "1": "high"})
        for bad in (float("nan"), float("inf")):
            with self.assertRaises(shim.UpstreamError):
                shim.probabilities([0, bad], 0.8)

    def test_pinned_cuda_head_projection_requests_float32_output(self):
        receipt = json.loads((FIXTURES / "source-review.json").read_text())
        excerpt = (FIXTURES / "project_logits.py.txt").read_text()
        self.assertEqual(hashlib.sha256(excerpt.encode()).hexdigest(), receipt["projection_excerpt_sha256"])
        calls = []
        class Tensor:
            dtype = "BF16"
            is_cuda = True
            shape = (1, 2)
            def reshape(self, *shape): return self
            def t(self): return self
        class Method:
            pass
        output = Tensor()
        fake_torch = types.SimpleNamespace(Tensor=Tensor, float32="F32", mm=lambda *a, **kw: calls.append(kw) or output)
        scope = {"torch": fake_torch, "UnquantizedEmbeddingMethod": Method, "UnquantizedLinearMethod": Method,
                 "VocabParallelEmbedding": object, "current_platform": types.SimpleNamespace(is_cuda=lambda: True)}
        exec(excerpt, scope)
        head = types.SimpleNamespace(quant_method=Method(), weight=Tensor())
        scope["_apply_head"](types.SimpleNamespace(head_dtype="F32"), head, Tensor(), None)
        self.assertEqual(calls, [{"out_dtype": "F32"}])


if __name__ == "__main__":
    unittest.main()
