#!/usr/bin/env python3
"""DSP parity harness: run identical IQ through the Python reference and the Rust/WASM kernels.

The Python DSP is not only the fallback, it is the *numeric reference* the browser kernels are held
to. This tool makes that comparison a command anyone can run and read:

  1. re-runs the Python reference over the committed IQ and asserts the golden fixtures still match
     (`tools/fixtures/gen_dsp_fixtures.py --check`) — i.e. the Python side reproduces the bytes;
  2. runs the Rust comparison tests against those same bytes (`cargo test --release --test
     ddc_reference --test demod_reference --test ft8_reference -- --nocapture`);
  3. reports one row per DDC stage and per analog mode with the measured worst difference and its
     bound, plus the FT8 message equality check, and a verdict.

Exit status is non-zero when either side fails or a difference exceeds the tolerance, so this can be
a release gate.

    python3 tools/analysis/dsp_parity.py [--wasm]      # --wasm: rebuild/check the artifact first
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
FIXTURES = ROOT / 'tests' / 'fixtures' / 'dsp'
WASM_DIR = ROOT / 'wasm'

#: Printed lines the Rust tests emit, e.g. "fir: worst difference 0e0" or
#: "usb: worst difference vs the Python reference 2.9e-8".
DIFF_LINE = re.compile(r'^(?P<name>[a-z0-9_ ]+): worst difference(?: vs the (?:Python )?reference)? '
                       r'(?P<value>[0-9.eE+-]+)$')
#: The FT8 test prints the message it decoded; the fixture records what it must be.
FT8_DECODED = re.compile(r"FT8 decoded '([^']*)'")


def run(cmd: list[str], cwd: Path) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


def tolerance() -> float:
    manifest = json.loads((FIXTURES / 'manifest.json').read_text())
    return float(manifest['tolerance'])


def demod_bounds() -> dict[str, float]:
    """Each analog mode's own bound, from its fixture entry.

    They are not all the same: the three modes whose detector is followed by a one-pole DC blocker
    amplify this crate's ~1e-7 FIR difference by the blocker's 2000x noise gain, so their bound is
    2e-3 (measured worst 7.4e-4) while the rest hold to 1e-5 (measured worst 1.8e-7).
    """
    manifest = json.loads((FIXTURES / 'manifest.json').read_text())
    fallback = float(manifest['tolerance'])
    return {entry['mode']: float(entry.get('tolerance', fallback))
            for entry in manifest.get('demod', [])}


def manifest_message() -> str:
    """The message the FT8 fixture carries (its own manifest, next to the DSP one)."""
    ft8 = ROOT / 'tests' / 'fixtures' / 'ft8' / 'manifest.json'
    return str(json.loads(ft8.read_text())['message'])


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--wasm', action='store_true',
                        help='also rebuild the wasm artifact and verify it matches a fresh build')
    args = parser.parse_args()

    print('== Python reference (the golden bytes) ==')
    check = run([sys.executable, 'tools/fixtures/gen_dsp_fixtures.py', '--check'], ROOT)
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
                 '--test', 'ft8_reference', '--', '--nocapture'], WASM_DIR)
    output = tests.stdout + tests.stderr
    if tests.returncode != 0:
        print(output[-3000:], file=sys.stderr)
        print('a comparison test failed', file=sys.stderr)
        return 1
    # The FT8 contract is about the decoded text, not a numeric difference: the fixture's message
    # must come back exactly. Reported here so one command covers every kernel comparison.
    decoded = FT8_DECODED.search(output)
    expected = manifest_message()

    bounds = demod_bounds()
    rows: list[tuple[str, float, bool]] = []
    for line in output.splitlines():
        match = DIFF_LINE.match(line.strip())
        if match:
            value = float(match.group('value'))
            name = match.group('name')
            rows.append((name, value, value <= bounds.get(name, tolerance())))

    if not rows:
        print(output[-2000:], file=sys.stderr)
        print('no measured differences were reported: the comparison tests did not run', file=sys.stderr)
        return 1

    limit = tolerance()
    worst = max(value for _name, value, _ok in rows)
    print(f'DDC-stage bound: {limit:.1e} absolute on f32, full scale 1.0')
    print('analog modes: 1e-5, or 2e-3 for the DC-blocked detectors (their blocker has a 2000x '
          'noise gain); each mode carries its own bound in tests/fixtures/dsp/manifest.json\n')
    print(f'{"stage / mode":<26}{"worst difference":>18}{"verdict":>10}')
    for name, value, ok in sorted(rows):
        print(f'{name:<26}{value:>18.3e}{"ok" if ok else "FAIL":>10}')
    print(f'\nworst over {len(rows)} comparisons: {worst:.3e}')
    if decoded is None:
        print('\nFT8: the fixture did not decode', file=sys.stderr)
        return 1
    print(f'FT8 message: {decoded.group(1)!r} (expected {expected!r}) '
          f'{"ok" if decoded.group(1) == expected else "FAIL"}')
    if decoded.group(1) != expected:
        return 1

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
