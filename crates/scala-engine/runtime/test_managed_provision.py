"""Synthetic managed provisioning checks; no dependencies are installed."""
import importlib.util
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
import zipfile
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('managed', Path(__file__).parent / 'managed/provision.py')
managed = importlib.util.module_from_spec(spec)
spec.loader.exec_module(managed)


class ManagedProvisionTests(unittest.TestCase):
    def test_dlpack_keeps_original_colliding_members_in_separate_sites(self):
        lock = 'torch==2.13.0 \\\n    --hash=sha256:' + 'a' * 64 + '\ntorch-c-dlpack-ext==0.1.5 \\\n    --hash=sha256:' + 'b' * 64 + '\napache-tvm-ffi==0.1.11 \\\n    --hash=sha256:' + 'c' * 64 + '\n'
        primary, auxiliary = managed.dlpack_locks(lock)
        self.assertNotIn('torch-c-dlpack-ext', primary)
        self.assertIn('apache-tvm-ffi==0.1.11', primary)
        self.assertIn('torch-c-dlpack-ext==0.1.5', auxiliary)
        self.assertNotIn('torch==', auxiliary)
        self.assertIn("--hash=sha256:" + "b" * 64, auxiliary)

    def test_source_archive_cannot_escape_or_alias_an_owned_package(self):
        for member in ['snapshot/../../model.safetensors', 'snapshot/C:/model', 'snapshot/alias\\file']:
            with self.subTest(member=member), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                archive = root / 'source.zip'
                with zipfile.ZipFile(archive, 'w') as source:
                    source.writestr(member, b'synthetic source')
                with self.assertRaisesRegex(ValueError, 'Unsafe'):
                    managed.extract_source(archive, root / 'owned')
                self.assertFalse((root / 'model.safetensors').exists())

    def test_original_vendor_attestation_has_exact_reviewed_members(self):
        root = Path('/synthetic-owned-runtime')
        payload = b'fake wheel; no native libraries'
        class Sink(io.BytesIO):
            def __exit__(self, *_): return False
        with patch.object(managed.urllib.request, 'urlopen', return_value=io.BytesIO(payload)), patch.object(Path, 'open', return_value=Sink()), patch.object(managed, 'sha', return_value='b59cec7a1a3f78fad13fead78cad8b6d9686827f9ff4477080245457675a01d0'):
            args = managed.vendor_attestation(root)
        self.assertEqual(args[:2], ['--wheel-attestation', 'pynvvideocodec'])
        members = json.loads(args[4])
        self.assertEqual(len(members), 2)
        self.assertEqual(members['PyNvVideoCodec_130.cpython-312-x86_64-linux-gnu.so'], '14f12a7977c2f681fb01693e41434308bfb5cf0e2c31ed2c29d1176337c86462')

    def test_linux_label_recipe_preserves_reviewed_options_and_private_tools(self):
        if managed.os.name == 'nt':
            self.skipTest('Linux recipe assembled independently of Windows')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / '.scala-python-pending').write_text('synthetic owner')
            (root / 'work').mkdir()
            recipe = root / 'recipes/crates/scala-engine-vllm/runtime/linux'
            recipe.mkdir(parents=True)
            lock = recipe / 'h2o-requirements.lock'
            lock.write_text('vllm==0.30.0 \\\n    --hash=sha256:' + 'a' * 64 + '\n')
            (root / 'recipe.json').write_text(json.dumps({'backend': 'vllm-labels',
                'dependency_lock_sha256': hashlib.sha256(lock.read_bytes()).hexdigest(),
                'source_revision': '193ad740925b176a3b70a5a13a7cff2f2fadd01e'}))
            with patch.object(managed.sys, 'argv', ['provision.py', '--root', tmp, '--uv', '/private/uv']), patch.object(managed, 'run') as run, patch.object(managed, 'vendor_attestation', return_value=[]):
                managed.main()
            commands = [[str(arg) for arg in call.args] for call in run.call_args_list]
            install = next(cmd for cmd in commands if 'install' in cmd)
            self.assertEqual(install[0], '/private/uv')
            for flag in ['--no-deps', '--require-hashes', '--only-binary', '--link-mode']:
                self.assertIn(flag, install)
            prepare = commands[-1]
            for option, value in [('max_model_len', '40960'), ('min_context', '4096'), ('gpu_memory_utilization', '0.90')]:
                self.assertEqual(prepare[prepare.index(option) + 1], value)
            self.assertIn('--managed-root', prepare)

    def test_managed_helper_refuses_non_owned_roots_before_any_command(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(managed.subprocess, 'run') as run, patch.object(managed.sys, 'argv', ['provision.py', '--root', tmp, '--uv', '/private/uv']):
            with self.assertRaisesRegex(ValueError, 'owned pending'):
                managed.main()
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
