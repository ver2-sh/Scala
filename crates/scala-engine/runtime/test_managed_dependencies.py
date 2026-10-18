"""Compiler/JIT binding fixtures only: no packages, compilers or models executed."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('compiler_integrity', Path(__file__).with_name('python_integrity.py'))
integrity = importlib.util.module_from_spec(spec)
spec.loader.exec_module(integrity)


class RecipeContractTests(unittest.TestCase):
    def test_native_policy_is_hashed_with_the_python_install_recipe(self):
        root = Path(__file__).resolve().parents[2]
        catalog = (root / 'scala-engine-vllm/src/catalog.rs').read_text()
        for resource in ('linux/cpp.lock.json', 'msvc.lock.json', 'windows-tools.ps1', 'prepare-msvc.ps1', 'vs-channel.json'):
            self.assertIn('runtime/native/' + resource, catalog)


@unittest.skipIf(os.name == 'nt', 'Linux durable compiler filesystem and symlinks')
class CompilerBindingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='scala-compiler-test-')
        self.root = Path(self.temp.name).resolve()
        self.managed = self.root / 'runtimes/engine/platform/version/id'
        self.managed.mkdir(parents=True)
        impl = 'scala-linux-dependencies-v1'
        bootstrap = 'a' * 64
        lock = b'{"schema_version":1,"platform":"linux-64","minimum_glibc":"2.28","cuda_release":null,"packages":[]}'
        recipe_hash = hashlib.sha256(lock).hexdigest()
        environment = hashlib.sha256((impl+'\n'+bootstrap+'\n'+recipe_hash+'\n').encode()).hexdigest()
        self.prefix = self.root / 'runtime-dependencies/linux' / environment / 'prefix'
        (self.prefix / 'scala-bin').mkdir(parents=True)
        (self.prefix / 'bin').mkdir()
        (self.prefix / 'lib/pkgconfig').mkdir(parents=True)
        (self.prefix / 'share/pkgconfig').mkdir(parents=True)
        self.cache = self.root / 'cache'
        self.cache.mkdir()
        payload = self.cache / 'compiler-fixture'
        payload.write_bytes(b'synthetic compiler payload, never executed')
        payload.chmod(0o755)
        for name in ('cc', 'c++'):
            os.link(payload, self.prefix / 'scala-bin' / name)
        self.inventory = {}
        for p in self.prefix.rglob('*'):
            st = p.lstat()
            kind = 'directory' if p.is_dir() else 'file'
            self.inventory[p.relative_to(self.prefix).as_posix()] = {
                'kind': kind, 'size': 0 if p.is_dir() else st.st_size,
                'sha256': '' if p.is_dir() else integrity.sha(p),
                'target': None, 'mode': stat.S_IMODE(st.st_mode),
            }
        self.ref = {'schema_version': 1, 'recipe_sha256': recipe_hash, 'environment_sha256': environment,
                    'bootstrap_sha256': bootstrap, 'implementation_version': impl,
                    'inventory_sha256': '', 'prefix': str(self.prefix)}
        (self.prefix.parent / '.scala-owner').write_text(impl+':'+environment)
        (self.prefix.parent / 'lock.json').write_bytes(lock)
        self.binding = {'schema_version':1, 'reference': self.ref, 'environment': {
            'CC': str(self.prefix/'scala-bin/cc'), 'CXX': str(self.prefix/'scala-bin/c++'),
            'PATH': str(self.prefix/'scala-bin')+':'+str(self.prefix/'bin')+':/usr/bin:/bin',
            'CMAKE_PREFIX_PATH': str(self.prefix),
            'PKG_CONFIG_LIBDIR': str(self.prefix/'lib/pkgconfig')+':'+str(self.prefix/'share/pkgconfig'),
        }}
        self.publish_inventory()

    def publish_inventory(self):
        data = json.dumps(self.inventory, sort_keys=True).encode()
        (self.prefix.parent / 'inventory.json').write_bytes(data)
        self.ref['inventory_sha256'] = hashlib.sha256(data).hexdigest()
        (self.prefix.parent / 'ready.json').write_text(json.dumps({'reference':self.ref,'toolchain':{}}))

    def tearDown(self):
        self.temp.cleanup()

    def verify(self, binding=None):
        return integrity.verify_build_toolchain(binding or self.binding, str(self.managed))

    def test_complete_binding_survives_acquisition_cache_cleanup(self):
        before = dict(os.environ)
        self.assertEqual(self.verify(), self.ref['inventory_sha256'])
        shutil.rmtree(self.cache)
        self.assertEqual(self.verify(), self.ref['inventory_sha256'])
        self.assertEqual(dict(os.environ), before, 'inspection must not inject build settings into Scala')

    def test_changed_file_and_unexpected_environment_are_rejected(self):
        changed = copy.deepcopy(self.binding)
        changed['environment']['CXX'] = '/usr/bin/untrusted'
        with self.assertRaises(ValueError): self.verify(changed)
        (self.prefix/'scala-bin/c++').write_bytes(b'tampered')
        with self.assertRaises(ValueError): self.verify()

    def test_changed_ready_and_inventory_receipts_are_rejected(self):
        (self.prefix.parent/'ready.json').write_text('{"reference":{}}')
        with self.assertRaises(ValueError): self.verify()
        self.publish_inventory()
        (self.prefix.parent/'inventory.json').write_text('{}')
        with self.assertRaises(ValueError): self.verify()

    def test_extra_payload_and_external_symlinks_are_rejected(self):
        extra = self.prefix/'extra'
        extra.write_bytes(b'not in inventory')
        with self.assertRaises(ValueError): self.verify()
        extra.unlink()
        target = self.prefix/'scala-bin/link'
        target.symlink_to('/etc/passwd')
        self.inventory['scala-bin/link'] = {'kind':'symlink','size':0,'sha256':'','target':'/etc/passwd','mode':stat.S_IMODE(target.lstat().st_mode)}
        self.publish_inventory()
        with self.assertRaises(ValueError): self.verify()

    def test_internal_symlink_is_allowed_but_prefix_escape_is_not(self):
        link = self.prefix/'scala-bin/g++'
        link.symlink_to('c++')
        self.inventory['scala-bin/g++'] = {'kind':'symlink','size':0,'sha256':'','target':'c++','mode':stat.S_IMODE(link.lstat().st_mode)}
        self.publish_inventory()
        self.assertEqual(self.verify(), self.ref['inventory_sha256'])
        changed = copy.deepcopy(self.binding)
        changed['reference']['prefix'] = str(self.root/'foreign')
        with self.assertRaises((ValueError, FileNotFoundError)): self.verify(changed)


if __name__ == '__main__':
    unittest.main()
