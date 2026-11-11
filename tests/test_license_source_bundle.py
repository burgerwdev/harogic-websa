"""A source offer must contain the actual edited inputs and reject stale copies."""
import json
import subprocess
import tarfile
from pathlib import Path

import pytest

from tools.prepare_legal import LICENSE_FILES, check, prepare, snapshot


@pytest.fixture
def checkout(tmp_path):
    root = tmp_path / 'project'
    root.mkdir()
    for name in LICENSE_FILES:
        (root / name).write_text(f'Original {name} — copyright and terms.\n', encoding='utf-8')
    (root / 'pyproject.toml').write_text('[project]\nname = "test"\nversion = "1.2.3"\n')
    (root / '.gitignore').write_text('frontend/public/legal/\n')
    (root / 'backend/src').mkdir(parents=True)
    (root / 'backend/src/lib.rs').write_text('// original source\n')
    (root / 'run.sh').write_text('#!/bin/sh\nexit 0\n')
    (root / 'run.sh').chmod(0o755)
    (root / 'setup.sh').write_text('#!/bin/sh\nexit 0\n')
    (root / 'setup.sh').chmod(0o755)
    subprocess.run(['git', 'init', '-q', str(root)], check=True)
    subprocess.run(['git', '-C', str(root), 'add', '.'], check=True)
    # A throwaway test repository, never the user's index or history.
    subprocess.run(['git', '-C', str(root), '-c', 'user.name=Fixture', '-c',
                    'user.email=fixture@example.invalid', 'commit', '-qm', 'fixture'], check=True)
    return root


def test_archives_modified_and_new_inputs_with_original_notices_and_no_sdk(checkout):
    root = checkout
    (root / 'backend/src/lib.rs').write_text('// locally modified source\n')
    (root / 'frontend/src').mkdir(parents=True)
    (root / 'frontend/src/new.ts').write_text('export const localChange = true;\n')
    (root / 'backend/.env').write_text('private runtime configuration')
    (root / 'htra_api.py').write_text('vendor wrapper, rights unconfirmed')
    (root / 'runtime').mkdir()
    (root / 'runtime/websad').write_bytes(b'old runtime output')
    (root / 'frontend/dist').mkdir()
    (root / 'frontend/dist/index.html').write_text('older build')
    (root / 'dsp/core/fdk').mkdir(parents=True)
    notice = b'Unmodified upstream notice\n'
    (root / 'dsp/core/fdk/NOTICE').write_bytes(notice)
    output = root / 'frontend/public/legal'
    manifest = prepare(root, output)
    assert manifest['git_dirty']
    assert manifest['version'] == '1.2.3'
    assert check(root, output) == manifest
    prefix = f'harogic-websa-{manifest["source_id"]}/'
    with tarfile.open(output / manifest['archive']) as archive:
        names = {name.removeprefix(prefix) for name in archive.getnames()}
        assert {'backend/src/lib.rs', 'frontend/src/new.ts', 'dsp/core/fdk/NOTICE', 'setup.sh', 'SOURCE-MANIFEST.json'} <= names
        assert not {'htra_api.py', 'backend/.env', 'runtime/websad', 'frontend/dist/index.html'} & names
        assert not any('/legal/' in name for name in names)
        assert archive.extractfile(prefix + 'backend/src/lib.rs').read() == b'// locally modified source\n'
        assert archive.getmember(prefix + 'run.sh').mode == 0o755
        assert archive.getmember(prefix + 'setup.sh').mode == 0o755
        embedded = json.load(archive.extractfile(prefix + 'SOURCE-MANIFEST.json'))
        assert embedded['files'] == manifest['files']
    assert notice in (output / 'THIRD-PARTY-NOTICES.txt').read_bytes()
    for original, copy in LICENSE_FILES.items():
        assert (output / copy).read_bytes() == (root / original).read_bytes()


def test_detects_stale_sources_modes_notices_and_corrupt_archives(checkout):
    output = checkout / 'frontend/public/legal'
    original = prepare(checkout, output)
    (checkout / 'backend/src/lib.rs').write_text('// a subsequent edit\n')
    with pytest.raises(ValueError, match='stale'):
        check(checkout, output)
    updated = prepare(checkout, output)
    assert updated['source_id'] != original['source_id']
    (checkout / 'run.sh').chmod(0o644)
    with pytest.raises(ValueError, match='stale'):
        check(checkout, output)
    updated = prepare(checkout, output)
    (output / 'LICENSE-EXCEPTION.txt').write_text('accidentally omitted permission')
    with pytest.raises(ValueError, match='packaged license differs'):
        check(checkout, output)
    prepare(checkout, output)
    (output / updated['archive']).write_bytes(b'truncated source download')
    with pytest.raises(ValueError, match='digest mismatch'):
        check(checkout, output)


def test_extracted_source_retains_its_identity_and_builds_without_git(checkout, tmp_path):
    output = checkout / 'frontend/public/legal'
    manifest = prepare(checkout, output)
    extracted = tmp_path / 'extracted'
    extracted.mkdir()
    with tarfile.open(output / manifest['archive']) as archive:
        for member in archive.getmembers():
            path = extracted.joinpath(*Path(member.name).parts[1:])
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(archive.extractfile(member).read())
            path.chmod(member.mode)
    current, _ = snapshot(extracted)
    assert current['source_id'] == manifest['source_id']
    assert current['git_revision'] == manifest['git_revision']
    rebuilt = prepare(extracted, extracted / 'frontend/public/legal')
    assert rebuilt['source_id'] == manifest['source_id']
    check(extracted, extracted / 'frontend/public/legal')
    (extracted / 'backend/src/added.rs').write_text('// a downstream module\n')
    added = prepare(extracted, extracted / 'frontend/public/legal')
    assert added['source_id'] != manifest['source_id']
    assert 'backend/src/added.rs' in added['files']
