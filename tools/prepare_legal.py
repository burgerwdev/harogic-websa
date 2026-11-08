#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Harogic-WebSA
# AGPL-3.0-only with ../LICENSE-EXCEPTION; scope in ../LICENSING.md.
"""Prepare local license notices and a source snapshot for the frontend build.

No SDK installation, network service, Git index or tracked runtime is modified.
The content hash represents checkout bytes, including uncommitted project files.
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[1]
SOURCE_DIRS = {'.github', 'backend', 'contracts', 'docs', 'dsp', 'frontend', 'tests', 'tools', 'LICENSES'}
ROOT_FILES = {
    'AGENTS.md', '.gitignore', '.gitattributes', 'LICENSE', 'LICENSE-EXCEPTION', 'LICENSE-PACKAGE',
    'LICENSING.md', 'THIRD_PARTY_NOTICES.md', 'README.md', 'README.zh-CN.md',
    'Makefile', 'pyproject.toml', 'requirements.txt', 'requirements-dev.txt',
    'requirements-lock.txt', 'build.sh', 'run.sh', 'stop.sh', 'status.sh',
    'lifecycle.sh', 'clean.sh', 'test.sh',
}
EXCLUDED_PARTS = {'target', 'node_modules', 'dist', '__pycache__', 'pkg'}
LICENSE_FILES = {
    'LICENSE': 'AGPL-3.0-only.txt',
    'LICENSE-EXCEPTION': 'LICENSE-EXCEPTION.txt',
    'LICENSE-PACKAGE': 'LICENSE-PACKAGE.txt',
    'LICENSING.md': 'LICENSING.txt',
    'THIRD_PARTY_NOTICES.md': 'THIRD_PARTY_NOTICES.txt',
}


def source_path(name: str) -> bool:
    path = PurePosixPath(name)
    if path.is_absolute() or '..' in path.parts or not path.parts:
        return False
    if name in ROOT_FILES:
        return True
    if path.parts[0] not in SOURCE_DIRS or EXCLUDED_PARTS.intersection(path.parts):
        return False
    if any(part.startswith('.') and part != '.github' for part in path.parts):
        return False
    # The SDK's Python wrapper is not project-owned. Never collect vendor
    # libraries, local captures/logs, or recursively include the generated offer.
    return not (name.startswith('frontend/public/legal/')
                or re.search(r'\.(?:so(?:\.[0-9.]+)?|dll|dylib|log|pid|pyc)$', name))


def git(root: Path, *args: str) -> bytes:
    return subprocess.check_output(['git', '-C', str(root), *args], stderr=subprocess.DEVNULL)


def source_files(root: Path) -> tuple[list[str], str | None, bool]:
    try:
        if Path(os.fsdecode(git(root, 'rev-parse', '--show-toplevel')).strip()).resolve() != root:
            raise ValueError('not the project Git root')
        names = [os.fsdecode(p) for p in git(root, 'ls-files', '-z', '--cached', '--others', '--exclude-standard').split(b'\0') if p]
        revision = git(root, 'rev-parse', 'HEAD').decode().strip()
        dirty = bool(git(root, 'status', '--porcelain', '--untracked-files=normal'))
    except (subprocess.CalledProcessError, FileNotFoundError, ValueError):
        manifest = json.loads((root / 'SOURCE-MANIFEST.json').read_text())
        names = list(manifest['files'])
        # An archive recipient can add modules without creating a Git checkout.
        # Include those additions too, pruning generated/dependency directories.
        names.extend(name for name in ROOT_FILES if (root / name).is_file())
        for directory in SOURCE_DIRS:
            for current, directories, files in os.walk(root / directory):
                relative = Path(current).relative_to(root)
                directories[:] = [name for name in directories if source_path((relative / name).as_posix() + '/')]
                names.extend((relative / name).as_posix() for name in files)
        revision, dirty = manifest['git_revision'], True
    return sorted({name for name in names if source_path(name) and (root / name).is_file()}), revision, dirty


def snapshot(root: Path) -> tuple[dict, dict[str, bytes]]:
    names, revision, dirty = source_files(root)
    contents, files = {}, {}
    digest = hashlib.sha256()
    for name in names:
        path = root / name
        if path.is_symlink():
            raise ValueError(f'source symlink needs explicit packaging: {name}')
        data = path.read_bytes()
        mode = 0o755 if path.stat().st_mode & 0o111 else 0o644
        checksum = hashlib.sha256(data).hexdigest()
        files[name] = {'sha256': checksum, 'mode': mode}
        contents[name] = data
        digest.update(f'{name}\0{mode:o}\0{checksum}\n'.encode())
    for name in (*LICENSE_FILES, 'pyproject.toml'):
        if name not in contents:
            raise ValueError(f'required source/license file missing: {name}')
    project = re.search(r'^\[project\]\s*$(.*?)(?=^\[|\Z)', contents['pyproject.toml'].decode(), re.M | re.S)
    version = re.search(r'^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"', project[1] if project else '', re.M)
    if not version:
        raise ValueError('missing project version')
    tree_hash = digest.hexdigest()
    source_id = f'{version[1]}-{tree_hash[:16]}'
    manifest = {
        'format': 1, 'source_id': source_id, 'version': version[1],
        'tree_sha256': tree_hash, 'git_revision': revision, 'git_dirty': dirty,
        'license': 'LicenseRef-Harogic-WebSA', 'files': files,
        'archive': f'websa-source-{source_id}.tar.gz',
        'excluded': 'Runtime/build outputs, generated legal bundle, Git internals, vendor SDK and htra_api.py. See docs/en/LICENSING.md for dependency source/provenance limitations.',
    }
    return manifest, contents


def json_bytes(value: dict) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + '\n').encode()


def third_party_notices(contents: dict[str, bytes]) -> bytes:
    parts = [b'Original third-party license and attribution files from this source snapshot.\n'
             b'See THIRD_PARTY_NOTICES.txt for scope and unresolved provenance.\n']
    for name, data in contents.items():
        if '/' not in name:
            continue
        base = PurePosixPath(name).name.lower()
        if base.startswith(('license', 'licence', 'copying', 'notice', 'thirdpartynotices')):
            parts.extend([f'\n===== {name} =====\n\n'.encode(), data, b'\n'])
    return b''.join(parts)


def prepare(root: Path, output: Path) -> dict:
    manifest, contents = snapshot(root)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.legal-', dir=output.parent) as directory:
        staging = Path(directory)
        archive = staging / manifest['archive']
        with archive.open('wb') as raw, gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed, tarfile.open(fileobj=compressed, mode='w') as bundle:
            for name, data in {**contents, 'SOURCE-MANIFEST.json': json_bytes(manifest)}.items():
                entry = tarfile.TarInfo(f'harogic-websa-{manifest["source_id"]}/{name}')
                entry.size, entry.mtime = len(data), 0
                entry.mode = manifest['files'].get(name, {}).get('mode', 0o644)
                bundle.addfile(entry, io.BytesIO(data))
        for name, target in LICENSE_FILES.items():
            (staging / target).write_bytes(contents[name])
        (staging / 'THIRD-PARTY-NOTICES.txt').write_bytes(third_party_notices(contents))
        manifest['archive_sha256'] = hashlib.sha256(archive.read_bytes()).hexdigest()
        (staging / 'source-manifest.json').write_bytes(json_bytes(manifest))
        # Only this generated directory is replaced; never touch tracked runtime.
        if output.exists():
            shutil.rmtree(output)
        output.mkdir()
        for path in staging.iterdir():
            path.replace(output / path.name)
    return manifest


def check(root: Path, output: Path) -> dict:
    manifest = json.loads((output / 'source-manifest.json').read_text())
    current, contents = snapshot(root)
    if manifest['tree_sha256'] != current['tree_sha256'] or manifest['files'] != current['files']:
        raise ValueError('source snapshot is stale; rebuild the frontend from the matching source')
    archive = output / manifest['archive']
    if archive.parent != output or hashlib.sha256(archive.read_bytes()).hexdigest() != manifest['archive_sha256']:
        raise ValueError('source archive digest mismatch')
    for name, target in LICENSE_FILES.items():
        if (output / target).read_bytes() != contents[name]:
            raise ValueError(f'packaged license differs: {name}')
    if (output / 'THIRD-PARTY-NOTICES.txt').read_bytes() != third_party_notices(contents):
        raise ValueError('packaged third-party notices differ')
    return manifest


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=ROOT / 'frontend/public/legal')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    try:
        manifest = (check if args.check else prepare)(ROOT, args.output.resolve())
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, f'License/source bundle: {error}\n')
    print(json.dumps({key: manifest[key] for key in ('source_id', 'archive', 'archive_sha256')}))


if __name__ == '__main__':
    main()
