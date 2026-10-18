"""Provision only the pending Scala-owned root. No tensor imports or model acquisition.

This helper is embedded in Scala releases with the engine's reviewed locks and
original serving resources. The Rust installer owns its process tree and admission.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import urllib.request
import zipfile


def sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def run(*args):
    print('Provisioning: ' + ' '.join(str(a) for a in args[:4]), file=sys.stderr, flush=True)
    subprocess.run([str(a) for a in args], check=True)


def extract_source(archive, output):
    with zipfile.ZipFile(archive) as source:
        roots = {name.split('/')[0] for name in source.namelist()}
        if len(roots) != 1:
            raise ValueError('Source snapshot must have one root')
        prefix = next(iter(roots)) + '/'
        seen = set()
        for member in source.infolist():
            # ZipInfo normalizes backslashes on Windows; inspect the original archive spelling.
            if chr(92) in member.orig_filename:
                raise ValueError('Unsafe source snapshot member: ' + member.orig_filename)
            relative = member.filename.removeprefix(prefix)
            if not relative:
                continue
            parts = relative.rstrip('/').split('/')
            if (any(p in {'', '.', '..'} for p in parts) or '\\' in relative or ':' in relative
                    or relative.casefold() in seen or (member.external_attr >> 16) & 0o170000 == 0o120000):
                raise ValueError('Unsafe source snapshot member: ' + relative)
            seen.add(relative.casefold())
            target = output.joinpath(*parts)
            if member.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.open(member) as src, target.open('xb') as dst:
                    shutil.copyfileobj(src, dst)


def dlpack_locks(text):
    # Preserve both original build_backend.py files in separate wheel sites.
    primary, auxiliary = [], []
    blocks = []
    current = []
    for line in text.splitlines():
        if line and not line.startswith((' ', '#')):
            if current:
                blocks.append('\n'.join(current) + '\n')
            current = [line]
        elif current:
            current.append(line)
    if current:
        blocks.append('\n'.join(current) + '\n')
    for block in blocks:
        (auxiliary if block.startswith('torch-c-dlpack-ext==') else primary).append(block)
    return ''.join(primary), ''.join(auxiliary)


def vendor_attestation(root):
    # The sole reviewed Linux exception, preserved from decision-source-evidence.md.
    name = 'pynvvideocodec-2.0.4-cp312-cp312-manylinux_2_28_x86_64.whl'
    expected = 'b59cec7a1a3f78fad13fead78cad8b6d9686827f9ff4477080245457675a01d0'
    url = 'https://files.pythonhosted.org/packages/ca/49/98da271686e00676f41b1197ba5431ddc341b96d8efb68ea9d68e2b0d870/pynvvideocodec-2.0.4-cp312-cp312-manylinux_2_28_x86_64.whl'
    path = root / name
    with urllib.request.urlopen(url, timeout=60) as src, path.open('xb') as dst:
        shutil.copyfileobj(src, dst)
    if sha(path) != expected:
        raise ValueError('Original PyNvVideoCodec wheel acquisition hash mismatch')
    return ['--wheel-attestation', 'pynvvideocodec', str(path), expected, json.dumps({
        'PyNvVideoCodec/PyNvVideoCodec_121.cpython-312-x86_64-linux-gnu.so': '2fb85f8bcd33c13e240ef2a8c6277f4d5a0260b629ecf9a242a04f1403f582a8',
        'PyNvVideoCodec/PyNvVideoCodec_130.cpython-312-x86_64-linux-gnu.so': '14f12a7977c2f681fb01693e41434308bfb5cf0e2c31ed2c29d1176337c86462'})]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--uv', type=Path, required=True)
    parser.add_argument('--source-archive', type=Path)
    args = parser.parse_args()
    root = args.root.resolve(strict=True)
    if not (root / '.scala-python-pending').is_file():
        raise ValueError('Managed provisioning requires an owned pending package')
    plan = json.loads((root / 'recipe.json').read_text())
    engine = 'pytorch' if plan['backend'] == 'torch-readout' else 'vllm'
    recipes = root / 'recipes'
    recipe = recipes / f'crates/scala-engine-{engine}/runtime'
    windows = os.name == 'nt'
    if windows and engine == 'vllm':
        run(sys.executable, '-I', '-B', recipe / 'windows/provision_h2o.py',
            '--runtime-root', root, '--model-root', recipe / 'fixtures/h2o',
            '--evidence-root', root / 'work', '--name', 'managed', '--uv', args.uv,
            '--managed-root', root, '--backend', plan['backend'],
            '--cache-root', Path(os.environ['UV_CACHE_DIR']).parent / 'windows-acquisition' / plan['dependency_lock_sha256'])
        return
    environment = root / 'environment'
    python = environment / ('Scripts/python.exe' if windows else 'bin/python')
    run(args.uv, '--no-config', 'venv', '--no-python-downloads', '--python', sys.executable, environment)
    lock = recipe / ('windows' if windows else 'linux') / ('h2o-requirements.lock' if engine == 'vllm' else 'imajev-requirements.lock')
    if sha(lock) != plan['dependency_lock_sha256']:
        raise ValueError('Embedded dependency lock changed')
    primary, auxiliary = dlpack_locks(lock.read_text())
    main_lock = root / 'work/primary.lock'
    main_lock.write_text(primary)
    command = [args.uv, '--no-config', 'pip', 'install', '--python', python, '--no-deps', '--require-hashes',
               '--only-binary', ':all:', '--link-mode', 'copy', '--index-url', 'https://pypi.org/simple']
    if windows:
        command += ['--index', 'https://download.pytorch.org/whl/cu130', '--index-strategy', 'unsafe-best-match']
    attestation = vendor_attestation(root) if engine == 'vllm' else []
    if attestation:
        # Use the same original wheel for installation and attestation. No
        # second complete vendor download and no rewritten RECORD metadata.
        wheel = Path(attestation[2])
        lines = main_lock.read_text().splitlines()
        for index, line in enumerate(lines):
            if line.startswith('pynvvideocodec=='):
                lines[index] = 'pynvvideocodec @ ' + wheel.as_uri() + ' \\'
        main_lock.write_text('\n'.join(lines) + '\n')
    run(*command, '-r', main_lock)
    if auxiliary:
        site = environment / ('Lib/site-packages' if windows else 'lib/python3.12/site-packages')
        aux_lock = root / 'work/dlpack.lock'
        aux_lock.write_text(auxiliary)
        run(*command, '--target', site / 'scala-dlpack-wheel', '-r', aux_lock)
        (site / 'scala-dlpack-wheel.pth').write_text(str(site / 'scala-dlpack-wheel') + '\n')
    prepare = [python, '-I', '-B', recipe / 'prepare.py', 'runtime', '--backend', plan['backend'], '--python', python,
               '--managed-root', root, '--output', root / 'runner']
    toolchain = root / 'build-toolchain.json'
    if engine == 'vllm' and toolchain.is_file():
        prepare += ['--build-toolchain', toolchain]
    if engine == 'pytorch':
        archive = args.source_archive
        if not archive or sha(archive) != plan['source']['digest']['value']:
            raise ValueError('Pinned Imajev source archive hash mismatch')
        source = root / 'implementation'
        extract_source(archive, source)
        prepare += ['--source', source, '--source-revision', plan['source_revision'],
                    '--verified-source-archive', archive, '--source-archive-sha256', plan['source']['digest']['value'],
                    '--option', 'device', '"cuda"', '--option', 'rotations', '4', '--option', 'max_input_tokens', '4096']
    elif plan['backend'] == 'vllm-labels':
        prepare += ['--source', recipe / 'fixtures/h2o', '--source-revision', plan['source_revision'], '--entrypoint', 'h2o_lightning_shim.py',
                    '--option', 'max_model_len', '40960', '--option', 'gpu_memory_utilization', '0.90', '--option', 'min_context', '4096']
    if engine == 'vllm':
        prepare += attestation
    run(*prepare)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(f'Managed Python installation failed: {error}')
