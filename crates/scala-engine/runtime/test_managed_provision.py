"""Synthetic managed provisioning checks; no dependencies are installed."""
import base64
import csv
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

spec = importlib.util.spec_from_file_location('managed_integrity', Path(__file__).with_name('python_integrity.py'))
integrity = importlib.util.module_from_spec(spec)
spec.loader.exec_module(integrity)


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
        self.assertEqual(args[2], str(root / 'pynvvideocodec-2.0.4-cp312-cp312-manylinux_2_28_x86_64.whl'))
        self.assertEqual(args[3], 'b59cec7a1a3f78fad13fead78cad8b6d9686827f9ff4477080245457675a01d0')
        members = json.loads(args[4])
        self.assertEqual(members, {
            'PyNvVideoCodec/PyNvVideoCodec_121.cpython-312-x86_64-linux-gnu.so': '2fb85f8bcd33c13e240ef2a8c6277f4d5a0260b629ecf9a242a04f1403f582a8',
            'PyNvVideoCodec/PyNvVideoCodec_130.cpython-312-x86_64-linux-gnu.so': '14f12a7977c2f681fb01693e41434308bfb5cf0e2c31ed2c29d1176337c86462'})

    def test_managed_attestation_matches_original_record_and_verifies_exact_files(self):
        if managed.os.name == 'nt':
            self.skipTest('Reviewed vendor wheel attestation is Linux-only')
        # Preserve the qualified member paths and two-defect RECORD shape, with
        # tiny synthetic bytes instead of acquiring or loading native libraries.
        contents = {
            'PyNvVideoCodec/PyNvVideoCodec_121.cpython-312-x86_64-linux-gnu.so': b'synthetic original 121',
            'PyNvVideoCodec/PyNvVideoCodec_130.cpython-312-x86_64-linux-gnu.so': b'synthetic original 130',
            'PyNvVideoCodec/__init__.py': b'# synthetic package\n'}
        record_path = 'pynvvideocodec-2.0.4.dist-info/RECORD'
        defective_members = set(list(contents)[:2])
        def record_hash(content):
            return base64.urlsafe_b64encode(hashlib.sha256(content).digest()).decode().rstrip('=')
        rows = [(name, 'sha256=' + record_hash(b'bad vendor RECORD' if name in defective_members else data), str(len(data)))
                for name, data in contents.items()]
        record = io.StringIO()
        csv.writer(record).writerows(rows + [(record_path, '', '')])
        payload = io.BytesIO()
        with zipfile.ZipFile(payload, 'w') as wheel:
            for name, data in contents.items():
                wheel.writestr(name, data)
            wheel.writestr(record_path, record.getvalue())

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            with patch.object(managed.urllib.request, 'urlopen', return_value=io.BytesIO(payload.getvalue())), patch.object(managed, 'sha', return_value='b59cec7a1a3f78fad13fead78cad8b6d9686827f9ff4477080245457675a01d0'):
                args = managed.vendor_attestation(root)
            archive = Path(args[2])
            declared = json.loads(args[4])
            with zipfile.ZipFile(archive) as wheel:
                original_rows = {row[0]: (row[1], row[2]) for row in csv.reader(io.StringIO(wheel.read(record_path).decode()))}
                self.assertEqual(set(declared), defective_members)
                self.assertTrue(set(declared) <= set(original_rows) & set(wheel.namelist()))
            # Only this synthetic fixture substitutes hashes. The production
            # archive/member digests are asserted independently above; all
            # integrity checks below hash real fixture bytes without mocks.
            attestation = {'path': str(archive), 'sha256': integrity.sha(archive),
                           'mismatches': {name: hashlib.sha256(contents[name]).hexdigest() for name in declared}}
            self.assertEqual(integrity.original_wheel_attestation('pynvvideocodec', '2.0.4', attestation), original_rows)

            site = root / 'site'
            site.mkdir()
            for name, data in contents.items():
                member = site / name
                member.parent.mkdir(parents=True, exist_ok=True)
                member.write_bytes(data)
            metadata = site / record_path
            metadata.parent.mkdir()
            metadata.write_text(record.getvalue())
            (metadata.parent / 'METADATA').write_text('Name: pynvvideocodec\nVersion: 2.0.4\n')
            vendor = integrity.importlib.metadata.PathDistribution(metadata.parent)
            source = root / 'shim.py'
            source.write_bytes(b'# synthetic native shim\n')
            packages = {'vllm': '0.30.0', 'torch': '1', 'transformers': '1', 'safetensors': '1', 'pynvvideocodec': '2.0.4'}
            distributions = {'pynvvideocodec': vendor}
            for name, version in packages.items():
                if name == 'pynvvideocodec':
                    continue
                info = site / f'{name}-{version}.dist-info'
                info.mkdir()
                (site / f'{name}.py').write_bytes(source.read_bytes())
                (info / 'METADATA').write_text(f'Name: {name}\nVersion: {version}\n')
                (info / 'RECORD').write_text(f'{name}.py,sha256={record_hash(source.read_bytes())},{source.stat().st_size}\n')
                distributions[name] = integrity.importlib.metadata.PathDistribution(info)
            cfg = {'schema_version': 1, 'backend': 'vllm-labels', 'packages': packages,
                   'options': {'max_model_len': 40960, 'min_context': 4096, 'gpu_memory_utilization': 0.90},
                   'source': {'path': str(root), 'revision': 'a' * 40, 'entrypoint': 'shim.py',
                              'files': {'shim.py': {'size_bytes': source.stat().st_size, 'sha256': integrity.sha(source)}}},
                   'wheel_attestations': {'pynvvideocodec': attestation}}
            config_path = root / 'runtime.json'
            config_path.write_text(json.dumps(cfg))
            with patch.object(integrity, 'BACKENDS', {'vllm-labels'}), patch.object(integrity.importlib.metadata, 'distribution', side_effect=distributions.__getitem__), patch.object(integrity.importlib.metadata, 'distributions', return_value=list(distributions.values())):
                self.assertEqual(integrity.runtime_identity(config_path)[0], cfg)
                for name in declared:
                    member = site / name
                    with self.subTest(member=name, difference='modified'):
                        member.write_bytes(b'unqualified replacement')
                        with self.assertRaisesRegex(ValueError, 'Runtime package file changed'):
                            integrity.runtime_identity(config_path)
                        member.write_bytes(contents[name])
                    with self.subTest(member=name, difference='missing file'):
                        member.unlink()
                        # Python 3.14 omits absent paths from dist.files; older
                        # versions expose them and fail when hashing the file.
                        with self.assertRaisesRegex((FileNotFoundError, ValueError), 'No such file|not fully consumed'):
                            integrity.runtime_identity(config_path)
                        member.write_bytes(contents[name])
                    with self.subTest(member=name, difference='missing RECORD member'):
                        metadata.write_text(''.join(line for line in record.getvalue().splitlines(keepends=True) if not line.startswith(name + ',')))
                        with self.assertRaisesRegex(ValueError, 'not fully consumed'):
                            integrity.runtime_identity(config_path)
                        metadata.write_text(record.getvalue())
                    with self.subTest(member=name, difference='changed RECORD hash'):
                        metadata.write_text(record.getvalue().replace(name + ',sha256=' + record_hash(b'bad vendor RECORD'), name + ',sha256=' + record_hash(b'unqualified RECORD')))
                        with self.assertRaisesRegex(ValueError, 'Runtime package file changed'):
                            integrity.runtime_identity(config_path)
                        metadata.write_text(record.getvalue())
                with self.subTest(difference='additional unqualified mismatch'):
                    extra = 'PyNvVideoCodec/unqualified.so'
                    (site / extra).write_bytes(b'unqualified native bytes')
                    metadata.write_text(record.getvalue() + f'{extra},sha256={record_hash(b'other bytes')},24\n')
                    with self.assertRaisesRegex(ValueError, 'Runtime package file changed'):
                        integrity.runtime_identity(config_path)
                    metadata.write_text(record.getvalue())
                with self.subTest(difference='modified ordinary member'):
                    (site / 'PyNvVideoCodec/__init__.py').write_bytes(b'# unqualified package\n')
                    with self.assertRaisesRegex(ValueError, 'Runtime package file changed'):
                        integrity.runtime_identity(config_path)

            for name in declared:
                invalid = dict(attestation, mismatches={Path(name).name: attestation['mismatches'][name]})
                with self.subTest(member=name, difference='basename alias'), self.assertRaisesRegex(ValueError, 'Invalid declared wheel RECORD mismatch'):
                    integrity.original_wheel_attestation('pynvvideocodec', '2.0.4', invalid)
            invalid = dict(attestation, mismatches=dict(attestation['mismatches'], **{'PyNvVideoCodec/unqualified.so': '0' * 64}))
            with self.assertRaisesRegex(ValueError, 'Invalid declared wheel RECORD mismatch'):
                integrity.original_wheel_attestation('pynvvideocodec', '2.0.4', invalid)

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
