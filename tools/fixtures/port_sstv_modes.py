#!/usr/bin/env python3
"""Port the vendored SSTV mode tables into a JSON table the Python tools can read.

The decoder/encoder of record is the vendored Rust tree (`dsp/core/src/digital/sstv/vendor`, from
unexcellent/sstv 0.3.1, MIT). The PlutoSDR transmitter and the committed fixture are Python, so they
need the same timing tables; transcribing them by hand would be a second list that drifts. This script
reads them out of the Rust source instead, exactly the way `port_ft8_tables.py` reads the FT8
constants out of ft8_lib.

What is read, per mode file (`vendor/modes/<mode>.rs`):

  * the `Mode` literal: name, VIS code, resolution, whether a starting sync pulse precedes the image,
    how many lines one pass of the sequence carries, and how the scans combine into colour;
  * the `const` step definitions it references (control tones and scan durations);
  * the `SEQUENCE` array, in order.

The display names come from `dsp/core/src/digital/sstv/mod.rs` (the plugin's `SSTV_MODES`), because that is
what the decode window's buttons show; keeping them means the fixture, the transmitter and the UI all
name a mode the same way.

    python3 tools/fixtures/port_sstv_modes.py           # write the table
    python3 tools/fixtures/port_sstv_modes.py --check   # fail if the table has drifted
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
MODES_DIR = ROOT / 'dsp' / 'core' / 'src' / 'digital' / 'sstv' / 'vendor' / 'modes'
PLUGIN_RS = ROOT / 'dsp' / 'core' / 'src' / 'digital' / 'sstv' / 'mod.rs'
OUT = ROOT / 'tests' / 'fixtures' / 'sstv' / 'modes.json'

#: The Rust files that hold a mode table; the module's own files are not modes.
NOT_MODES = {'mod.rs', 'mode.rs', 'step.rs', 'vis_code.rs', 'testing.rs'}

#: `vis_code!(44)`, `resolution: (320, 256)`, `lines_per_sequence: 2`, `color: ColorMode::Rgb`, …
MODE_FIELD = {
    'name': re.compile(r'\bname:\s*"([^"]+)"'),
    'vis': re.compile(r'\bvis_code:\s*vis_code!\((\d+)\)'),
    'resolution': re.compile(r'\bresolution:\s*\((\d+),\s*(\d+)\)'),
    'starting_sync_pulse': re.compile(r'\bstarting_sync_pulse:\s*(true|false)'),
    'lines_per_sequence': re.compile(r'\blines_per_sequence:\s*(\d+)'),
    'color': re.compile(r'\bcolor:\s*ColorMode::(\w+)'),
}

#: `const SYNC_PULSE: Step = Step::Control(tone!(1200 Hz, 9 ms));`
CONST_STEP = re.compile(
    r'const\s+(\w+)\s*:\s*Step\s*=\s*Step::Control\(\s*tone!\(\s*([\d_]+)\s*Hz\s*,\s*([\d_]+)\s*(ns|us|ms)\s*\)\s*\);'
)
#: `const SCAN: Duration = us!(138_240);`
CONST_DURATION = re.compile(r'const\s+(\w+)\s*:\s*Duration\s*=\s*(ns|us|ms)!\(\s*([\d_]+)\s*\);')

TONE = re.compile(r'tone!\(\s*([\d_]+)\s*Hz\s*,\s*([\d_]+)\s*(ns|us|ms)\s*\)')
DURATION = re.compile(r'\b(ns|us|ms)!\(\s*([\d_]+)\s*\)')
SCAN_START = re.compile(r'Step::Scan\(\s*Channel::(\w+)\s*,\s*')
SEQUENCE = re.compile(r'const\s+SEQUENCE\s*:\s*\[Step;\s*\d+\]\s*=\s*\[(.*?)\n\];', re.S)

#: The plugin's own display-name table: `("Martin M1", vendor::modes::MARTIN_1),`.
PLUGIN_MODE = re.compile(r'\(\s*"([^"]+)"\s*,\s*vendor::modes::(\w+)\s*\)')

UNIT_NS = {'ns': 1, 'us': 1_000, 'ms': 1_000_000}


def number(text: str) -> int:
    """A Rust integer literal with `_` separators."""
    return int(text.replace('_', ''))


def duration_ns(value: str, unit: str) -> int:
    return number(value) * UNIT_NS[unit]


def display_names() -> dict[str, str]:
    """The plugin's mode constants to their display names, in table order."""
    names: dict[str, str] = {}
    for name, constant in PLUGIN_MODE.findall(PLUGIN_RS.read_text(encoding='utf-8')):
        names[constant] = name
        names[constant.lower()] = name
    return names


def control_tone(text: str, consts: dict[str, dict]) -> dict:
    """A `tone!(…)` or a named `Step::Control` constant, as `{"hz": …, "ns": …}`."""
    match = TONE.search(text)
    if match:
        return {'hz': number(match.group(1)), 'ns': duration_ns(match.group(2), match.group(3))}
    name = text.strip()
    if name in consts:
        return dict(consts[name])
    raise SystemExit(f'cannot resolve a control step: {text!r}')


def scan_step(text: str, consts: dict[str, dict]) -> dict:
    """One `Step::Scan(Channel::X, duration)` entry.

    The duration is cut out by parenthesis depth rather than a `[^)]*` pattern: a `ms!(88)` carries a
    closing parenthesis of its own, and a pattern that stops at the first one reads `ms!(88`.
    """
    opening = SCAN_START.search(text)
    if opening is None:
        raise SystemExit(f'cannot parse a scan step: {text!r}')
    rest = text[opening.end():]
    depth = 1
    end = len(rest)
    for index, char in enumerate(rest):
        if char == '(':
            depth += 1
        elif char == ')':
            depth -= 1
            if depth == 0:
                end = index
                break
    rest = rest[:end].strip()
    if rest in consts:
        ns = consts[rest]['ns']
    else:
        duration = DURATION.search(rest)
        if duration is None:
            raise SystemExit(f'cannot resolve a scan duration: {rest!r}')
        ns = duration_ns(duration.group(2), duration.group(1))
    return {'scan': {'channel': opening.group(1), 'ns': ns}}


def split_entries(body: str) -> list[str]:
    """Split a Rust array body on its top-level commas.

    A `tone!(1200 Hz, 9 ms)` carries a comma of its own, so a plain `split(',')` cuts entries in
    half. Depth counting on parentheses is what keeps them whole.
    """
    entries: list[str] = []
    depth = 0
    current = ''
    for char in body:
        if char == '(':
            depth += 1
        elif char == ')':
            depth -= 1
        if char == ',' and depth == 0:
            entries.append(current)
            current = ''
            continue
        current += char
    entries.append(current)
    return [entry.strip() for entry in entries if entry.strip()]


def parse_mode(path: Path, names: dict[str, str]) -> dict:
    text = path.read_text(encoding='utf-8')
    fields = {key: pattern.search(text) for key, pattern in MODE_FIELD.items()}
    missing = [key for key, match in fields.items() if match is None]
    if missing:
        raise SystemExit(f'{path.name}: missing {", ".join(missing)}')

    consts: dict[str, dict] = {}
    for name, unit, value in CONST_DURATION.findall(text):
        consts[name] = {'ns': duration_ns(value, unit)}
    for name, hz, value, unit in CONST_STEP.findall(text):
        consts[name] = {'hz': number(hz), 'ns': duration_ns(value, unit)}

    body = SEQUENCE.search(text)
    if body is None:
        raise SystemExit(f'{path.name}: no SEQUENCE')
    # Comments carry prose that names steps; drop them before splitting on commas is wrong (a comma
    # inside a comment would split an entry), so cut the comments first.
    clean = re.sub(r'//[^\n]*', '', body.group(1))
    sequence: list[dict] = []
    for entry in split_entries(clean):
        if entry.startswith('Step::Scan'):
            sequence.append(scan_step(entry, consts))
        elif entry.startswith('Step::Control'):
            sequence.append({'control': control_tone(entry, consts)})
        else:
            # A named constant: a control tone or a scan distance, whichever it resolved to.
            if entry not in consts:
                raise SystemExit(f'{path.name}: unknown sequence entry {entry!r}')
            resolved = consts[entry]
            if 'hz' in resolved:
                sequence.append({'control': dict(resolved)})
            else:
                raise SystemExit(f'{path.name}: a scan entry must name its channel: {entry!r}')

    constant = path.stem
    constant = path.stem.upper()
    key = path.stem
    total_ns = sum((step.get('control') or step['scan'])['ns'] for step in sequence)
    return {
        'key': key,
        'constant': constant,
        'display': names.get(constant, fields['name'].group(1)),
        'rust_name': fields['name'].group(1),
        'vis': int(fields['vis'].group(1)),
        'width': int(fields['resolution'].group(1)),
        'height': int(fields['resolution'].group(2)),
        'starting_sync_pulse': fields['starting_sync_pulse'].group(1) == 'true',
        'lines_per_sequence': int(fields['lines_per_sequence'].group(1)),
        'color': fields['color'].group(1),
        'sequence': sequence,
        # The sum of one pass, carried here so the Rust side can assert the table it decodes with
        # agrees with the one the Python transmitter sends (see dsp/core/tests/sstv_decode.rs).
        'total_ns': total_ns,
    }


def build_table() -> dict:
    names = display_names()
    modes = []
    for path in sorted(MODES_DIR.glob('*.rs')):
        if path.name in NOT_MODES:
            continue
        modes.append(parse_mode(path, names))
    # The plugin's order is the ABI's order (the mode pin and the button highlight are indices), so
    # the table is sorted by it rather than by file name.
    order = {constant: index for index, (_, constant) in enumerate(PLUGIN_MODE.findall(PLUGIN_RS.read_text(encoding='utf-8')))}
    if len(order) != len(modes):
        raise SystemExit(f'the plugin table names {len(order)} modes, the vendor tree has {len(modes)}')
    unknown = [mode['constant'] for mode in modes if mode['constant'] not in order]
    if unknown:
        raise SystemExit(f'the plugin table does not list: {unknown}')
    modes.sort(key=lambda mode: order[mode['constant']])
    for index, mode in enumerate(modes):
        mode['index'] = index
    return {
        'source': 'dsp/core/src/digital/sstv/vendor/modes (unexcellent/sstv 0.3.1, MIT) — generated by '
                  'tools/fixtures/port_sstv_modes.py, do not edit by hand',
        'modes': modes,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--check', action='store_true',
                        help='compare against the committed table instead of writing it')
    args = parser.parse_args()

    table = build_table()
    text = json.dumps(table, indent=1) + '\n'
    if args.check:
        if not OUT.exists():
            print(f'{OUT} is missing; run this script without --check', file=sys.stderr)
            return 1
        if OUT.read_text(encoding='utf-8') != text:
            print(f'{OUT} has drifted from the Rust tables; re-run this script', file=sys.stderr)
            return 1
        print(f'SSTV mode table OK: {len(table["modes"])} modes match the vendored Rust tables')
        return 0

    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text, encoding='utf-8')
    print(f'wrote {OUT.relative_to(ROOT)} ({len(table["modes"])} modes)')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
