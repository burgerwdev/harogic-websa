#!/usr/bin/env python3
"""DSP parity harness: run identical IQ through the Python reference and the Rust/WASM kernels.

The Python DSP is not only the fallback, it is the *numeric reference* the browser kernels are held
to. This tool makes that comparison a command anyone can run and read:

  1. re-runs the Python reference over the committed IQ and asserts the golden fixtures still match
     (`tools/gen_dsp_fixtures.py --check`) — i.e. the Python side reproduces the bytes;
  2. runs the Rust comparison tests against those same bytes, with their measured differences
     printed (`cargo test --test ddc_reference --test demod_reference -- --nocapture`);
  3. reports one row per stage/mode with the measured worst difference, the tolerance from the
     fixture manifest, and a verdict.

Exit status is non-zero when either side fails or a difference exceeds the tolerance, so this can be
a release gate.

    python3 tools/dsp_parity.py [--wasm]      # --wasm: rebuild/check the artifact first
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / 'tests' / 'fixtures' / 'dsp'
WASM_DIR = ROOT / 'wasm'

#: Printed lines the Rust tests emit, e.g. "fir: worst difference 0e0" or
#: "usb: worst difference vs the Python reference 2.9e-8".
DIFF_LINE = re.compile(r'^(?P<name>[a-z0-9_ ]+): worst difference(?: vs the Python reference)? '
                       r'(?P<value>[0-9.eE+-]+)$')


def run(cmd: list[str], cwd: Path) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


def tolerance() -> float:
    manifest = json.loads((FIXTURES / 'manifest.json').read_text())
    return float(manifest['tolerance'])


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--wasm', action='store_true',
                        help='also rebuild the wasm artifact and verify it matches a fresh build')
    args = parser.parse_args()

    print('== Python reference (the golden bytes) ==')
    check = run([sys.executable, 'tools/gen_dsp_fixtures.py', '--check'], ROOT)
    print(check.stdout.strip() or check.stderr.strip())
    if check.returncode != 0:
        print('the Python reference no longer reproduces the committed fixtures', file=sys.stderr)
        return 1

    if args.wasm:
        print('\n== WASM artifact ==')
        build = run(['./wasm/build.sh', '--check'], ROOT)
        print(build.stdout.strip().splitlines()[-1] if build.stdout.strip() else build.stderr.strip())
        if build.returncode != 0:
            return 1

    print('\n== Rust/WASM kernels vs the Python reference ==')
    tests = run(['cargo', 'test', '--release', '--test', 'ddc_reference', '--test', 'demod_reference',
                 '--', '--nocapture'], WASM_DIR)
    output = tests.stdout + tests.stderr

    rows: list[tuple[str, float, bool]] = []
    for line in output.splitlines():
        match = DIFF_LINE.match(line.strip())
        if match:
            value = float(match.group('value'))
            rows.append((match.group('name'), value, value <= tolerance()))

    if not rows:
        print(output[-2000:], file=sys.stderr)
        print('no measured differences were reported: the comparison tests did not run', file=sys.stderr)
        return 1

    limit = tolerance()
    worst = max(value for _name, value, _ok in rows)
    print(f'tolerance (from the fixture manifest): {limit:.1e} absolute on f32, full scale 1.0\n')
    print(f'{"stage / mode":<26}{"worst difference":>18}{"verdict":>10}')
    for name, value, ok in sorted(rows):
        print(f'{name:<26}{value:>18.3e}{"ok" if ok else "FAIL":>10}')
    print(f'\nworst over {len(rows)} comparisons: {worst:.3e}')

    failed = [name for name, value, ok in rows if not ok]
    if failed or tests.returncode != 0:
        print(f'\nparity FAILED: {failed or "a comparison test failed"}', file=sys.stderr)
        if tests.returncode != 0:
            print(output[-2000:], file=sys.stderr)
        return 1
    print('parity OK: every kernel agrees with the Python reference inside the tolerance')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
