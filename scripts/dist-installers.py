#!/usr/bin/env python3
"""Add embedded archive verification to the pinned upstream PowerShell installer.

Run after dist build --artifacts=global, before uploading. Hashes come from actual
local artifacts and must agree with cargo-dist's manifest. Keep all installation,
PATH, receipt and updater behavior upstream-owned. Refuse unknown template shapes.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re

BEGIN = '  # BEGIN Scala archive verification (dist 0.33.0)\n'
END = '  # END Scala archive verification\n'
ANCHOR = '  Invoke-DownloadFile -client $wc -url $url -path $dir_path\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text())
    if manifest['dist_version'] != '0.33.0':
        raise ValueError('Review installer customization before upgrading dist')
    artifacts = manifest['artifacts']
    entry = artifacts['scala-installer.ps1']
    if entry.get('checksum') or entry.get('checksums'):
        raise ValueError('Installer checksum metadata needs an upstream review')
    # Merged CI manifests omit artifact-local paths; upload_files is authoritative.
    installers = [Path(p) for p in manifest['upload_files'] if Path(p).name == 'scala-installer.ps1']
    if len(installers) != 1:
        raise ValueError('Expected exactly one PowerShell installer upload')
    installer = installers[0]
    original = installer.read_text(encoding='utf-8')
    text = original
    if BEGIN in text:
        if text.count(BEGIN) != 1 or text.count(END) != 1:
            raise ValueError('Unexpected verification block')
        start = text.index(BEGIN)
        stop = text.index(END, start) + len(END)
        text = text[:start] + text[stop:]
    if text.count(ANCHOR) != 1 or text.count('  Write-Verbose "Unpacking to $tmp"') != 1:
        raise ValueError('Upstream download/extraction template changed')
    if ANCHOR + '\n  Write-Verbose "Unpacking to $tmp"' not in text:
        raise ValueError('Upstream download/extraction order changed')
    names = sorted(set(re.findall(r'"artifact_name" = "([^"]+)"', text)))
    if not names or any(not re.fullmatch(r'scala-[a-z0-9_-]+\.(zip|tar\.(xz|gz|zst))', name) for name in names):
        raise ValueError('Unexpected PowerShell artifact selection')
    lines = []
    for name in names:
        artifact = artifacts[name]
        expected = artifact.get('checksums', {}).get('sha256')
        if not expected or not re.fullmatch('[0-9a-f]{64}', expected):
            raise ValueError(f'Missing built archive SHA-256: {name}')
        # Global jobs download archives beside the generated installer. Manifest
        # paths from other runners may be absolute paths that no longer exist.
        archive = installer.parent / name
        if hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
            raise ValueError(f'Archive differs from dist manifest: {name}')
        lines.append(f'    "{name}" = "{expected}"\n')
    block = (BEGIN + '  $scalaArchiveHashes = @{\n' + ''.join(lines) + '  }\n'
             '  $expectedHash = $scalaArchiveHashes[$artifact_name]\n'
             '  if (-not $expectedHash -or $expectedHash -notmatch "^[0-9a-f]{64}$") {\n'
             '    throw "ERROR: missing SHA-256 for $artifact_name"\n'
             '  }\n'
             '  $actualHash = (Get-FileHash -LiteralPath $dir_path -Algorithm SHA256 -ErrorAction Stop).Hash\n'
             '  if ($actualHash -ine $expectedHash) {\n'
             '    throw "ERROR: SHA-256 mismatch for $artifact_name"\n'
             '  }\n' + END)
    text = text.replace(ANCHOR, ANCHOR + block)
    if args.check:
        if original != text:
            raise ValueError('PowerShell installer verification is absent or stale')
    else:
        installer.write_text(text, encoding='utf-8', newline='\n')
    print(f'Verified PowerShell archive hashes for {len(names)} artifact(s)')


if __name__ == '__main__':
    main()
