#!/usr/bin/env python3
"""One entry point for the PlutoSDR bench transmitters.

Each modulation keeps its own signal generator. This wrapper enumerates the
generators in one place and sends the arguments to the right tool. It adds two
commands:

  * `--list`       - print every mode and the tool that sends it
  * `--dry-run-all`- dry-run every mode and report PASS / FAIL / SKIP. This needs
                     no radio, so it is the quick check after a change.

    python3 tools/pluto_tx.py --list
    python3 tools/pluto_tx.py --dry-run-all
    python3 tools/pluto_tx.py nfm --lo 411e6 --gain -10
    python3 tools/pluto_tx.py drm --iq drm_iq.wav --dry-run
    python3 tools/pluto_tx.py cw 'CQ CQ DE N0CALL' --pitch 700

Run `python3 tools/pluto_tx.py <mode> --help` for the mode's own options.
"""
from __future__ import annotations

import argparse
import contextlib
import importlib.util
import io
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent / 'pluto'  # the per-mode generators

#: mode -> (tool file or None, one-line description). None means no source here.
TOOLS: dict[str, tuple[str | None, str]] = {
    'am': ('pluto_analog_tx.py', 'AM: full-carrier amplitude modulation'),
    'dsb': ('pluto_analog_tx.py', 'DSB: double sideband with a residual carrier'),
    'usb': ('pluto_analog_tx.py', 'USB: upper-sideband tone above the dial'),
    'lsb': ('pluto_analog_tx.py', 'LSB: lower-sideband tone below the dial'),
    'nfm': ('pluto_analog_tx.py', 'NFM: narrowband FM (3 kHz deviation)'),
    'wfm': ('pluto_analog_tx.py', 'WFM: wideband FM (75 kHz deviation)'),
    'pm': ('pluto_analog_tx.py', 'PM: phase modulation'),
    'cw': ('pluto_cw_tx.py', 'CW: keyed Morse carrier at the Pitch'),
    'ft8': ('pluto_ft8_tx.py', 'FT8: 8-FSK burst in a 15 s slot'),
    'drm': ('pluto_drm_tx.py', 'DRM30: OFDM from a DecDRM int16 IQ WAV'),
    'drmplus': (None, 'DRM+ mode E: no external transmitter in this repository'),
}
#: The analog tool takes the mode as a positional argument.
ANALOG_TOOL = 'pluto_analog_tx.py'


def load_tool(filename: str):
    """Load a tool by path, so `tools/` needs no package."""
    path = HERE / filename
    spec = importlib.util.spec_from_file_location(path.stem, path)
    if spec is None or spec.loader is None:  # pragma: no cover - packaging error
        raise SystemExit(f'cannot load tool at {path}')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run(mode: str, argv: list[str]) -> int:
    """Send `argv` to the tool for `mode` and return its exit code."""
    tool, _desc = TOOLS[mode]
    if tool is None:
        print(f'{mode}: no transmitter in this repository.', file=sys.stderr)
        print('  DecDRM supports modes A-D only. Mode E / DRM+ has no external source here.',
              file=sys.stderr)
        return 2
    module = load_tool(tool)
    call = [mode, *argv] if tool == ANALOG_TOOL else list(argv)
    return module.main(call)


def list_modes() -> int:
    width = max(len(mode) for mode in TOOLS)
    for mode, (tool, desc) in TOOLS.items():
        source = tool if tool else '(none)'
        print(f'{mode:<{width}}  {source:<20}  {desc}')
    return 0


def dry_run_all() -> int:
    """Dry-run every mode in-process and report the result. No radio is needed."""
    ok = True
    width = max(len(mode) for mode in TOOLS)
    for mode, (tool, desc) in TOOLS.items():
        if tool is None:
            print(f'SKIP  {mode:<{width}}  {desc}')
            continue
        buf = io.StringIO()
        code = 1
        try:
            with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
                code = run(mode, ['--dry-run'])
        except SystemExit as exc:
            code = exc.code if isinstance(exc.code, int) else 1
        if code == 0:
            print(f'PASS  {mode:<{width}}  {desc}')
        else:
            ok = False
            print(f'FAIL  {mode:<{width}}  {desc}')
            detail = buf.getvalue().rstrip()
            if detail:
                print(detail)
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--list', action='store_true', help='list every mode and its tool')
    parser.add_argument('--dry-run-all', action='store_true',
                        help='dry-run every mode and report PASS / FAIL / SKIP (no radio)')
    parser.add_argument('mode', nargs='?', choices=list(TOOLS),
                        help='the modulation to transmit; run --list to see them')
    parser.add_argument('mode_args', nargs=argparse.REMAINDER,
                        help='arguments for the mode tool')
    args = parser.parse_args(argv)
    if args.list:
        return list_modes()
    if args.dry_run_all:
        return dry_run_all()
    if args.mode is None:
        parser.error('choose a mode, or use --list / --dry-run-all')
    return run(args.mode, args.mode_args)


if __name__ == '__main__':
    raise SystemExit(main())
