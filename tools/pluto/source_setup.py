"""Prepare pinned, separately licensed bench encoders outside the repository."""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ONAIR_REVISION = '6b97328b1939f569b161c5066fbea8053a76c130'


def digital_helper() -> Path:
    cache = Path(os.environ.get('XDG_CACHE_HOME', str(Path.home() / '.cache')))
    build = Path(os.environ.get('PLUTO_DIGITAL_BUILD_DIR', str(cache / 'harogic-websa/pluto-digital')))
    return build / 'bin/pluto_digital_source'


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def prepared_digital(path: Path) -> bool:
    try:
        info = json.loads(Path(str(path) + '.json').read_text())
        return (os.access(path, os.X_OK) and info['revision'] == ONAIR_REVISION
                and info['encoder_sha256'] == digest(path)
                and info['wrapper_sha256'] == digest(HERE / 'pluto_digital_source.cpp')
                and info['build_sha256'] == digest(HERE / 'build-digital-source.sh'))
    except (OSError, ValueError, KeyError):
        return False


def ensure_digital(path: Path, *, setup: bool = True, force: bool = False) -> Path:
    path = path.expanduser().resolve()
    default = path == digital_helper().expanduser().resolve()
    if (default and prepared_digital(path)) or (not default and path.is_file() and os.access(path, os.X_OK)):
        return path
    if not force and (not setup or not default):
        raise ValueError(f'Protocol encoder is not prepared: {path}. '
                         'Run python tools/pluto_tx.py aprs --prepare, or supply a working --encoder.')
    print('Preparing APRS / FT4 / WSPR / AIS encoder (first run; no radio access)…', file=sys.stderr, flush=True)
    subprocess.run(['bash', str(HERE / 'build-digital-source.sh'), '', str(path)], check=True,
                   stdout=sys.stderr, stderr=sys.stderr)
    if not prepared_digital(path):
        raise ValueError('Encoder build completed without matching provenance; rerun --prepare')
    return path


def ensure_gps(root: Path, *, setup: bool = True, force: bool = False) -> None:
    if (root / 'SHA256SUMS').is_file():
        return  # The caller validates every installed file before executing it.
    if not setup and not force:
        raise ValueError('GPS simulator is not prepared. Run python tools/pluto_tx.py gps --prepare')
    print('Preparing the pinned GPS simulator and navigation fixture (first run; no radio access)…', file=sys.stderr, flush=True)
    subprocess.run(['bash', str(HERE.parent / 'setup-gps-simulator.sh'), '--prefix', str(root)], check=True,
                   stdout=sys.stderr, stderr=sys.stderr)
