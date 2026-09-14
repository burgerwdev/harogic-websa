#!/usr/bin/env python3
"""DOM id contract: every id the TypeScript reads must exist in index.html.

`getElementById('...')` with a string literal is invisible to the type system - renaming or
deleting an element in index.html fails silently at runtime (`if (el)` swallows it). This
check turns that into a build failure (report finding P1-5).

It found a real one when it was written: `cur-ifgain` was read by core/ws.ts to show the
actual IF-gain grade, but no such element existed, so the readout never appeared.

Usage:
    python3 tools/check_dom_ids.py [--list]

Ids that are legitimately optional belong in OPTIONAL below (with a reason), so the check
stays strict for everything else.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / 'frontend' / 'modern' / 'src'
INDEX = ROOT / 'frontend' / 'modern' / 'index.html'

#: ids read on purpose even though they may be absent (popovers injected at runtime, ...).
OPTIONAL: dict[str, str] = {}

READ_ID = re.compile(r"getElementById\(\s*'([^']+)'")
HTML_ID = re.compile(r'id="([^"]+)"')


def read_ids() -> dict[str, list[str]]:
    found: dict[str, list[str]] = {}
    for path in sorted(SRC.rglob('*.ts')):
        if '__tests__' in path.parts:
            continue
        for match in READ_ID.finditer(path.read_text(encoding='utf-8')):
            found.setdefault(match.group(1), []).append(str(path.relative_to(ROOT)))
    return found


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--list', action='store_true', help='print the whole contract')
    args = parser.parse_args()

    html_ids = set(HTML_ID.findall(INDEX.read_text(encoding='utf-8')))
    read = read_ids()
    missing = {name: where for name, where in read.items()
               if name not in html_ids and name not in OPTIONAL}
    stale = {name: where for name, where in OPTIONAL.items() if name not in read}

    if args.list:
        for name in sorted(read):
            where = ', '.join(sorted(set(read[name])))
            state = 'ok' if name in html_ids else 'optional' if name in OPTIONAL else 'MISSING'
            print(f'{state:8s} {name:28s} {where}')
        return 0

    problems = []
    for name, where in sorted(missing.items()):
        problems.append(f'index.html has no id="{name}" but it is read in {", ".join(sorted(set(where)))}')
    for name, where in sorted(stale.items()):
        problems.append(f'OPTIONAL lists "{name}" ({where}) but nothing reads it any more')
    if problems:
        print('DOM id contract violated:', file=sys.stderr)
        for problem in problems:
            print('  ' + problem, file=sys.stderr)
        print('\nFix: add the element to index.html, or list the id in tools/check_dom_ids.py OPTIONAL.',
              file=sys.stderr)
        return 1
    print(f'DOM id contract OK: {len(read)} ids read, all present in index.html')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
