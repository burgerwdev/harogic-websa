#!/usr/bin/env python3
"""Architecture guard rails — fail when a structural metric gets *worse* than the baseline.

The active gate measures frontend module cycles against the reviewed baseline.
Retired Python service metrics are intentionally removed rather than reported as zero.

Usage:
    python3 tools/checks/architecture_guard.py              # check (CI)
    python3 tools/checks/architecture_guard.py --baseline   # show the current numbers
    python3 tools/checks/architecture_guard.py --update     # accept the current numbers
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
BASELINE = Path(__file__).resolve().parent / 'baseline.json'

# ---------------- metrics ----------------


def _resolve_ts(base: Path, spec: str) -> Path | None:
    target = (base.parent / spec).resolve()
    for candidate in (target.with_suffix('.ts'), target.with_suffix('.js'), target / 'index.ts'):
        if candidate.is_file():
            return candidate
    return None


def _ts_graph() -> dict[Path, set[Path]]:
    root = ROOT / 'frontend' / 'src'
    files = [p for p in root.rglob('*') if p.suffix in ('.ts', '.js')]
    graph: dict[Path, set[Path]] = {p: set() for p in files}
    pattern = re.compile(r"""(?:from|import)\s*\(?\s*['"](\.[^'"]+)['"]""")
    for path in files:
        for spec in pattern.findall(path.read_text(encoding='utf-8')):
            target = _resolve_ts(path, spec)
            if target is not None and target in graph and target != path:
                graph[path].add(target)
    return graph


def _cycle_count(graph: dict[Path, set[Path]]) -> tuple[int, list[list[Path]]]:
    color: dict[Path, int] = {}
    stack: list[Path] = []
    cycles: list[list[Path]] = []

    def dfs(node: Path) -> None:
        color[node] = 1
        stack.append(node)
        for nxt in sorted(graph.get(node, ())):
            state = color.get(nxt, 0)
            if state == 0:
                dfs(nxt)
            elif state == 1:
                cycles.append(stack[stack.index(nxt):] + [nxt])
        stack.pop()
        color[node] = 2

    for node in graph:
        if color.get(node, 0) == 0:
            dfs(node)
    unique = {frozenset(cycle) for cycle in cycles}
    return len(unique), [sorted(c, key=str) for c in unique]


def frontend_cycles() -> int:
    return _cycle_count(_ts_graph())[0]




METRICS = {'frontend_cycles': frontend_cycles}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--update', action='store_true', help='write the current values as the baseline')
    parser.add_argument('--baseline', action='store_true', help='print the current values only')
    args = parser.parse_args()

    current = {name: fn() for name, fn in METRICS.items()}

    if args.baseline:
        for name, value in current.items():
            print(f'{name}: {value}')
        return 0

    if args.update or not BASELINE.exists():
        BASELINE.write_text(json.dumps(current, indent=1, sort_keys=True) + '\n', encoding='utf-8')
        print(f'baseline written: {BASELINE.relative_to(ROOT)}')
        for name, value in current.items():
            print(f'  {name}: {value}')
        return 0

    baseline = json.loads(BASELINE.read_text(encoding='utf-8'))
    regressions = []
    improvements = []
    for name, value in current.items():
        was = baseline.get(name)
        if was is None:
            regressions.append(f'{name}: no baseline entry (run --update)')
        elif value > was:
            regressions.append(f'{name}: {value} > baseline {was}')
        elif value < was:
            improvements.append(f'{name}: {value} < baseline {was} (lower the baseline with --update)')

    for line in improvements:
        print(f'improved: {line}')
    if regressions:
        print('architecture guard failed:', file=sys.stderr)
        for line in regressions:
            print('  ' + line, file=sys.stderr)
        return 1
    print('architecture guard OK: ' + ', '.join(f'{k}={v}' for k, v in current.items()))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
