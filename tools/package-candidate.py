#!/usr/bin/env python3
"""Stage a local candidate from current builds and verify it in its own directory.

Does not replace tracked runtime artifacts or copy proprietary SDK files.
Unresolved third-party permissions remain recorded in the licensing review.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import socket
import subprocess
import tarfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def checksum(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def verify(directory, manifest):
    with socket.socket() as endpoint:
        endpoint.bind(('127.0.0.1', 0))
        port = endpoint.getsockname()[1]
    environment = {key: value for key, value in os.environ.items()
                   if key not in ('WEBSA_BIN', 'WEBSA_STATIC', 'WEBSA_INSTANCE', 'WEBSA_UNIT')}
    origin = f'http://127.0.0.1:{port}'
    command = ['./run.sh', '--fake', '--host', '127.0.0.1', '--port', str(port), '--instance', 'candidate-check']
    subprocess.run(command, cwd=directory, env=environment, check=True, timeout=45)
    try:
        def read(path):
            with urllib.request.urlopen(origin + path, timeout=10) as response:
                assert response.status == 200
                return response.read()
        state = json.loads(read('/api/state'))
        assert state
        page = read('/static/dist/legal.html').decode()
        assert manifest['source_id'] in page
        assert manifest['version'] in read('/static/dist/index.html').decode()
        source = json.loads(read('/static/dist/legal/source-manifest.json'))
        assert source['source_id'] == manifest['source_id']
        data = read('/static/dist/legal/' + source['archive'])
        assert hashlib.sha256(data).hexdigest() == source['archive_sha256']
        archive = directory / 'runtime/frontend/dist/legal' / source['archive']
        with tarfile.open(archive) as bundle:
            entry = next(item for item in bundle.getmembers() if item.name.endswith('/SOURCE-MANIFEST.json'))
            embedded = json.load(bundle.extractfile(entry))
            assert embedded['tree_sha256'] == source['tree_sha256']
        result = subprocess.check_output(['./status.sh', '--instance', 'candidate-check'],
                                         cwd=directory, env=environment, text=True)
        assert 'Binary: current' in result
        return {'startup': True, 'health': True, 'version': source['version'], 'legal_page': True,
                'source_download_sha256': source['archive_sha256'], 'running_binary_current': True}
    finally:
        subprocess.run(['./stop.sh', '--instance', 'candidate-check'], cwd=directory, env=environment,
                       check=True, timeout=30)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, help='new, empty candidate directory outside the checkout')
    parser.add_argument('--shim', type=Path, default=Path.home() / '.local/share/harogic-websa/shims/libwebsa_san90_sdk_session.so')
    parser.add_argument('--verify', action='store_true', help='start a fixture service from the staged directory, verify and stop')
    args = parser.parse_args()
    destination = args.output.resolve()
    if destination == ROOT or ROOT in destination.parents:
        parser.error('candidate output must be outside the checkout')
    if destination.exists():
        parser.error('candidate output already exists; choose a fresh directory')
    subprocess.run(['python3', 'tools/prepare_legal.py', '--check', '--output', 'frontend/dist/legal'],
                   cwd=ROOT, check=True)
    legal = json.loads((ROOT / 'frontend/dist/legal/source-manifest.json').read_text())
    files = {name: ROOT / name for name in (
        'run.sh', 'stop.sh', 'status.sh', 'lifecycle.sh', 'supervise.py', 'pyproject.toml',
        'LICENSE', 'LICENSE-EXCEPTION', 'LICENSE-PACKAGE', 'LICENSING.md', 'THIRD_PARTY_NOTICES.md')}
    files.update({
        'runtime/linux-x86_64/websad': ROOT / 'backend/target/release/websad',
        'runtime/linux-x86_64/sdr_decode': ROOT / 'backend/target/release/sdr_decode',
        'runtime/linux-x86_64/libwebsa_san90_sdk_session.so': args.shim,
        'runtime/linux-x86_64/libonnxruntime.so.1.23.2': ROOT / 'runtime/linux-x86_64/libonnxruntime.so.1.23.2',
    })
    for source in files.values():
        if not source.is_file():
            parser.error(f'missing build input: {source}')
    destination.mkdir(parents=True)
    for name, source in files.items():
        output = destination / name
        output.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, output)
    shutil.copytree(ROOT / 'frontend/dist', destination / 'runtime/frontend/dist')
    shutil.copytree(ROOT / 'runtime/linux-x86_64/nr-notices', destination / 'runtime/linux-x86_64/nr-notices')
    shutil.copytree(ROOT / 'docs', destination / 'docs')
    manifest = {'source_id': legal['source_id'], 'version': legal['version'],
                'purpose': 'local validation candidate; licensing questions in docs/en/LICENSING.md remain open',
                'files': {path.relative_to(destination).as_posix(): checksum(path)
                          for path in sorted(destination.rglob('*')) if path.is_file()}}
    if args.verify:
        manifest['verification'] = verify(destination, manifest)
    (destination / 'candidate-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print(f'Candidate: {destination}\nSource: {legal["source_id"]}')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
