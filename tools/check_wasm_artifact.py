#!/usr/bin/env python3
"""Verify the committed WASM artifact without a Rust toolchain (so CI can gate it).

`wasm/build.sh --check` is the real proof (it rebuilds and compares), but it needs Rust, and
CI deliberately has none: adding the artifact must not add a toolchain to every push. This
check keeps the artifact honest with pure Python:

  * the file exists and its size and sha256 match `wasm/dsp.artifact.json`
  * the recorded ABI version is the one the loader expects
  * the module's *export section* actually contains every export the manifest lists

The last point is the one that catches a real mistake: a rebuilt artifact whose exports were
renamed (or a hand-edited manifest) would otherwise fail only inside the browser.

    python3 tools/check_wasm_artifact.py
"""
from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / 'wasm' / 'dsp.artifact.json'
ARTIFACT = ROOT / 'frontend' / 'public' / 'dsp.wasm'
#: ABI version this toolchain's loader understands; keep in step with wasm/src/abi.rs.
EXPECTED_ABI_VERSION = 1
WASM_MAGIC = b'\x00asm'


def read_leb(data: bytes, offset: int) -> tuple[int, int]:
    """Unsigned LEB128 (the encoding wasm uses for lengths and indices)."""
    result = 0
    shift = 0
    while True:
        if offset >= len(data):
            raise ValueError('truncated LEB128')
        byte = data[offset]
        offset += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, offset
        shift += 7
        if shift > 63:
            raise ValueError('LEB128 too long')


def wasm_exports(data: bytes) -> set[str]:
    """Every name in the module's export section (id 7).

    Only the section framing and the name prefix of each export entry are needed, which is why
    this stays a few lines of stdlib instead of a wasm parser dependency.
    """
    if data[:4] != WASM_MAGIC:
        raise ValueError('not a wasm module (bad magic)')
    offset = 8                                    # magic + version
    while offset < len(data):
        section_id = data[offset]
        offset += 1
        size, offset = read_leb(data, offset)
        end = offset + size
        if end > len(data):
            raise ValueError('section runs past the end of the file')
        if section_id == 7:
            count, cursor = read_leb(data, offset)
            names: set[str] = set()
            for _ in range(count):
                length, cursor = read_leb(data, cursor)
                names.add(data[cursor:cursor + length].decode('utf-8'))
                cursor += length + 1              # + kind
                _index, cursor = read_leb(data, cursor)
            return names
        offset = end
    return set()


def main() -> int:
    problems: list[str] = []
    if not MANIFEST.exists():
        print('wasm/dsp.artifact.json is missing; run wasm/build.sh', file=sys.stderr)
        return 1
    manifest = json.loads(MANIFEST.read_text())
    if not ARTIFACT.exists():
        print(f'{ARTIFACT.relative_to(ROOT)} is missing; run wasm/build.sh', file=sys.stderr)
        return 1

    data = ARTIFACT.read_bytes()
    if len(data) != manifest['bytes']:
        problems.append(f'size {len(data)} != manifest {manifest["bytes"]}')
    digest = hashlib.sha256(data).hexdigest()
    if digest != manifest['sha256']:
        problems.append(f'sha256 {digest[:16]}... != manifest {manifest["sha256"][:16]}...')
    if manifest.get('abi_version', EXPECTED_ABI_VERSION) != EXPECTED_ABI_VERSION:
        problems.append(f'manifest ABI version {manifest.get("abi_version")} != '
                        f'{EXPECTED_ABI_VERSION} (the loader would reject the module)')

    try:
        exports = wasm_exports(data)
    except ValueError as exc:
        problems.append(f'cannot read the export section: {exc}')
        exports = set()
    missing = set(manifest.get('exports', [])) - exports
    if missing:
        problems.append(f'exports missing from the module: {sorted(missing)}')
    if 'memory' not in exports:
        # Without an exported memory the loader has no views: the whole ABI is unusable.
        problems.append('the module does not export its memory')

    if problems:
        print('wasm artifact check failed:', file=sys.stderr)
        for problem in problems:
            print(f'  {problem}', file=sys.stderr)
        print('run wasm/build.sh to republish the artifact', file=sys.stderr)
        return 1

    print(f'wasm artifact OK: {len(data)} bytes, {len(exports)} exports, '
          f'{manifest["sha256"][:16]}... ({manifest.get("rustc", "unknown toolchain")})')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
