#!/usr/bin/env python3
"""Generate the SSTV fixture: one Python-encoded transmission, with the picture it carries.

Why a committed waveform: the decoder is Rust/WASM inside the committed artifact, so its input has to
exist outside it. The transmitter here is *this* repository's Python encoder
(`tools/fixtures/sstv_encode.py`, tables ported from the vendored Rust tree), which is what makes the
test a cross-implementation check rather than a round trip through one implementation: Python sends,
Rust receives.

Robot 36 is the mode to commit because it is the shortest (36 s), so the fixture stays a couple of
megabytes while exercising the hardest path — a two-line sequence with shared colour differences,
where a row order or channel swap shows up immediately.

    python3 tools/fixtures/gen_sstv_fixtures.py           # write the fixture
    python3 tools/fixtures/gen_sstv_fixtures.py --check   # fail if it has drifted
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(Path(__file__).resolve().parent))

import sstv_encode as sstv  # noqa: E402  (path set above, so tools/ needs no package)

OUT = ROOT / 'tests' / 'fixtures' / 'sstv'
MODE = 'Robot 36'
RATE = 8_000
CALLSIGN = 'CQ WE0BSA'
LABEL = 'SSTV TEST PATTERN'
#: Mean absolute error per channel the Rust decoder must stay under on this waveform. Measured on the
#: committed fixture; the decoder's own tolerance on a synthesised transmission is in the same range.
TOLERANCE = 24.0
#: Trailing silence committed with the waveform. A decoder that re-reads its buffer every few seconds
#: needs audio *after* the last scanline before its final attempt can see a complete picture — which is
#: what a live stream always has, and what a capture cut at the last sample would not.
TAIL_SILENCE_S = 10.0


def build() -> dict[Path, bytes]:
    """The fixture's files: the waveform, the picture it carries and the manifest."""
    mode = sstv.find_mode(MODE)
    picture = sstv.pattern_image(mode['width'], mode['height'], CALLSIGN, LABEL)
    tones = sstv.transmission_tones(mode, picture.bytes(), picture.width, picture.height)
    audio = sstv.synth(tones, RATE)
    audio = np.concatenate([audio, np.zeros(int(RATE * TAIL_SILENCE_S), dtype=np.int16)])

    # The plugin is handed complex baseband, so the audio goes in the I channel and Q stays zero: its
    # band pass and real part then recover the audio, which is what an SSB signal gives it.
    iq = np.zeros(audio.size * 2, dtype=np.float32)
    iq[0::2] = audio.astype(np.float32) / sstv.FULL_SCALE

    manifest = {
        'mode': mode['display'],
        'key': mode['key'],
        'vis': mode['vis'],
        'rate': RATE,
        'samples': int(audio.size),
        'tail_silence_s': TAIL_SILENCE_S,
        'width': mode['width'],
        'height': mode['height'],
        'lines_per_sequence': mode['lines_per_sequence'],
        'iq_file': 'robot36_iq.bin',
        'rgb_file': 'robot36_rgb.bin',
        'tolerance_mean_abs_error': TOLERANCE,
        'callsign': CALLSIGN,
        'generator': 'tools/fixtures/gen_sstv_fixtures.py (Python encoder; the decoder is the '
                     'vendored Rust tree)',
    }
    return {
        OUT / 'manifest.json': (json.dumps(manifest, indent=1) + '\n').encode(),
        OUT / manifest['iq_file']: iq.tobytes(),
        OUT / manifest['rgb_file']: picture.bytes(),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--check', action='store_true',
                        help='compare against the committed fixture instead of writing it')
    args = parser.parse_args()

    files = build()
    if args.check:
        problems: list[str] = []
        for path, content in files.items():
            if not path.exists():
                problems.append(f'{path.relative_to(ROOT)} is missing')
            elif path.read_bytes() != content:
                problems.append(f'{path.relative_to(ROOT)} has drifted')
        if problems:
            print('the SSTV fixture is stale:', file=sys.stderr)
            for problem in problems:
                print('  ' + problem, file=sys.stderr)
            print('re-run tools/fixtures/gen_sstv_fixtures.py', file=sys.stderr)
            return 1
        print(f'SSTV fixture OK: {len(files)} files for {MODE} match')
        return 0

    OUT.mkdir(parents=True, exist_ok=True)
    for path, content in files.items():
        path.write_bytes(content)
        print(f'wrote {path.relative_to(ROOT)} ({len(content)} bytes)')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
