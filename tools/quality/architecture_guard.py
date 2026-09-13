#!/usr/bin/env python3
"""Architecture guard rails — fail when a structural metric gets *worse* than the baseline.

These are the "fitness functions" from docs/*/ARCH_REVIEW.md §7.5. They do not demand the
refactor be finished; they stop the situation from degrading while it is carried out:

  * module cycles (frontend + backend)
  * the two command-layer god functions (line counts)
  * mode/session-name branching in the scheduler and command layers
  * direct `import htra_api` outside the hardware binding layer
  * untyped limit literals in command validation

Usage:
    python3 tools/quality/architecture_guard.py              # check (CI)
    python3 tools/quality/architecture_guard.py --baseline   # show the current numbers
    python3 tools/quality/architecture_guard.py --update     # accept the current numbers
"""
from __future__ import annotations

import argparse
import ast
import json
import os
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
    root = ROOT / 'frontend' / 'modern' / 'src'
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


def _module_path(dotted: str) -> Path | None:
    rel = dotted.replace('.', os.sep)
    for candidate in (ROOT / rel).with_suffix('.py'), ROOT / rel / '__init__.py':
        if candidate.is_file():
            return candidate
    return None


def backend_cycles() -> int:
    graph: dict[Path, set[Path]] = {}
    for path in (ROOT / 'web_sa').rglob('*.py'):
        graph[path] = set()
    for path, edges in graph.items():
        tree = ast.parse(path.read_text(encoding='utf-8'))
        package = path.parent.relative_to(ROOT).as_posix().replace('/', '.')
        for node in ast.walk(tree):
            if isinstance(node, ast.ImportFrom):
                if node.level:
                    base = package.split('.')
                    base = base[:len(base) - node.level + 1]
                    name = '.'.join(base + ([node.module] if node.module else []))
                else:
                    name = node.module or ''
                if name.startswith('web_sa'):
                    target = _module_path(name)
                    if target is not None and target != path:
                        edges.add(target)
            elif isinstance(node, ast.Import):
                for alias in node.names:
                    if alias.name.startswith('web_sa'):
                        target = _module_path(alias.name)
                        if target is not None and target != path:
                            edges.add(target)
    return _cycle_count(graph)[0]


def function_lines(rel_path: str, name: str) -> int:
    tree = ast.parse((ROOT / rel_path).read_text(encoding='utf-8'))
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == name:
            return node.end_lineno - node.lineno + 1
    raise SystemExit(f'{rel_path}: function {name} not found')


MODE_BRANCH = re.compile(r"""\.mode\s*==|\.name\s*==\s*['"](?:rta|sdr|std|harmonic|pnm)['"]|\.name\s+in\s*\('(?:rta|sdr)'""")
MODE_FILES = ('web_sa/web/ws.py', 'web_sa/web/publisher.py', 'web_sa/hardware/device.py')


def mode_branches() -> int:
    total = 0
    for rel in MODE_FILES:
        total += len(MODE_BRANCH.findall((ROOT / rel).read_text(encoding='utf-8')))
    return total


HTRA_IMPORT = re.compile(r'^\s*import\s+htra_api|^\s*from\s+htra_api', re.M)


def htra_imports_outside_bindings() -> int:
    count = 0
    for path in (ROOT / 'web_sa').rglob('*.py'):
        if path.name == 'sdk_bindings.py':
            continue
        count += len(HTRA_IMPORT.findall(path.read_text(encoding='utf-8')))
    return count


LIMIT_LITERAL = re.compile(r'maximum=(?:\d|[\d.]+e\d|[A-Za-z_]+\.\w+)')


def validation_limit_literals() -> int:
    """Numeric `maximum=` arguments in ws.py validation (should come from capabilities)."""
    text = (ROOT / 'web_sa/web/ws.py').read_text(encoding='utf-8')
    return len([m for m in LIMIT_LITERAL.findall(text) if not m.startswith('maximum=caps')])


METRICS = {
    'frontend_cycles': frontend_cycles,
    'backend_cycles': backend_cycles,
    'dispatch_lines': lambda: function_lines('web_sa/web/ws.py', '_dispatch'),
    'validate_lines': lambda: function_lines('web_sa/web/ws.py', '_validate_command'),
    'mode_branches': mode_branches,
    'htra_imports_outside_bindings': htra_imports_outside_bindings,
    'validation_limit_literals': validation_limit_literals,
}


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
