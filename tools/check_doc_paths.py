#!/usr/bin/env python3
"""Every repository path named in the architecture docs must exist.

The stage map is the architecture's contract — "every stage of the diagram is a named module" — and
it is only useful if the names are real. The bilingual docs-parity gate compares section structure,
not paths, so a renamed or aspirational module could sit there indefinitely (it did: the map named
`frontend/modern/src/sdr/worker.ts`, `sdr/analog.ts` and `wasm/src/analog/{am,dsb,...}.rs`, none of
which were ever written).

This checks the backticked, repository-looking paths in both `ARCHITECTURE.md` files, expanding
`{a,b}` groups. Paths are accepted as files or directories; anything else is a failure.

    python3 tools/check_doc_paths.py
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = [ROOT / 'docs' / 'en' / 'ARCHITECTURE.md', ROOT / 'docs' / 'zh-CN' / 'ARCHITECTURE.md']

#: The section that makes the promise. Scanning the whole file pairs backticks across prose (the
#: first version did, and reported zero paths), so the map is read section by section: everything
#: between this heading and the next one, split on backticks (odd pieces are the code spans).
STAGE_MAP = re.compile(r'### (?:Stage map|阶段映射)(.*?)(?=\n### )', re.S)
PATH_LIKE = re.compile(r'^[A-Za-z0-9_./{},*-]+/[A-Za-z0-9_./{},*-]*(\.[A-Za-z0-9]+|/)$')
BRACES = re.compile(r'\{([^}]*)\}')


def expand(path: str) -> list[str]:
    """Expand a `{a,b}` group into the individual paths (one level is enough for these docs)."""
    if '{' not in path:
        return [path]
    prefix = path[:path.index('{')]
    body = BRACES.search(path).group(1)
    suffix = path[BRACES.search(path).end():]
    return [f'{prefix}{part}{suffix}' for part in body.split(',')]


def main() -> int:
    problems: list[str] = []
    checked = 0
    for doc in DOCS:
        text = doc.read_text(encoding='utf-8')
        section = STAGE_MAP.search(text)
        if section is None:
            problems.append(f'{doc.relative_to(ROOT)}: no stage map section found')
            continue
        pieces = section.group(1).split('`')
        for token in pieces[1::2]:
            candidate = token.strip().rstrip('.,;:')
            if not PATH_LIKE.match(candidate):
                continue
            for path in expand(candidate):
                if any(ch in path for ch in ' {}*'):
                    continue                     # a glob or a placeholder is not a concrete path
                checked += 1
                if not (ROOT / path).exists():
                    problems.append(f'{doc.relative_to(ROOT)}: {path} does not exist')
    if problems:
        print('documented paths that do not exist:', file=sys.stderr)
        for problem in problems:
            print(f'  {problem}', file=sys.stderr)
        print('\nFix the doc (or the code), so the stage map keeps describing the real tree.',
              file=sys.stderr)
        return 1
    print(f'doc path contract OK: {checked} paths across {len(DOCS)} architecture docs exist')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
