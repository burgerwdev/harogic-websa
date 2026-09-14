#!/usr/bin/env python3
"""Every module-scope registration must be reachable from the entry point.

Registrations (`setRenderer`, `registerViewRenderer`, `registerMeasurementTab`) are side
effects of importing a module. After the render/spectrum cycles were broken, nothing imported
render/spectrum.ts any more, so its `setRenderer(renderAll)` never ran: the canvas stayed
blank with no exception and no console error, and every test that asserted frames, datasets
or counters still passed (report finding / lesson §9.3).

This check walks the import graph from `frontend/modern/src/main.ts` and fails when a module
that registers something at module scope is not reachable. Reachability is computed from the
actual import specifiers, so an entry-point import added for its side effect counts.

Usage:  python3 tools/check_registrations.py [--list]
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / 'frontend' / 'modern' / 'src'
ENTRY = SRC / 'main.ts'

#: Calls that must run for the app to work at all (module scope only; a call inside a
#: function body is reached when that function runs and is not checked here).
REGISTRATION = re.compile(r'^\s*(?:export\s+)?(setRenderer|registerViewRenderer|registerMeasurementTab)\s*\(', re.M)
IMPORT = re.compile(r"""(?:from|import)\s*\(?\s*['"](\.[^'"]+)['"]""")


def resolve(base: Path, spec: str) -> Path | None:
    target = (base.parent / spec).resolve()
    for candidate in (target.with_suffix('.ts'), target.with_suffix('.js'), target / 'index.ts'):
        if candidate.is_file():
            return candidate
    return None


def registry_modules() -> dict[Path, list[str]]:
    found: dict[Path, list[str]] = {}
    for path in SRC.rglob('*.ts'):
        if '__tests__' in path.parts:
            continue
        calls = REGISTRATION.findall(path.read_text(encoding='utf-8'))
        if calls:
            found[path] = sorted(set(calls))
    return found


def reachable_from_entry() -> set[Path]:
    seen: set[Path] = set()
    stack = [ENTRY]
    while stack:
        path = stack.pop()
        if path in seen or not path.is_file():
            continue
        seen.add(path)
        for spec in IMPORT.findall(path.read_text(encoding='utf-8')):
            target = resolve(path, spec)
            if target is not None and target not in seen:
                stack.append(target)
    return seen


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--list', action='store_true')
    args = parser.parse_args()

    registries = registry_modules()
    reachable = reachable_from_entry()
    missing = {p: calls for p, calls in registries.items() if p not in reachable}

    if args.list:
        for path, calls in sorted(registries.items()):
            state = 'ok' if path in reachable else 'UNREACHABLE'
            print(f'{state:12s} {path.relative_to(SRC)}  {", ".join(calls)}')
        return 0

    if missing:
        print('modules that register but are not reachable from main.ts:', file=sys.stderr)
        for path, calls in sorted(missing.items()):
            print(f'  {path.relative_to(SRC)}  ({", ".join(calls)})', file=sys.stderr)
        print('\nFix: import the module from main.ts (side-effect import) or delete the '
              'registration.', file=sys.stderr)
        return 1
    print(f'registration reachability OK: {len(registries)} registering modules, all reachable')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
