#!/usr/bin/env python3
"""Require paired English/Chinese documents with the same section structure.

Compares the `##`/`###` heading sequence of every docs/en/*.md with its zh-CN counterpart,
ignoring translated titles. Every document name must exist in both languages.

    python3 tools/checks/check_docs_parity.py
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
EN = ROOT / 'docs' / 'en'
ZH = ROOT / 'docs' / 'zh-CN'

HEADING = re.compile(r'^(#{2,3})\s+(.*)$', re.M)


def headings(path: Path) -> list[tuple[int, int]]:
    """Level per heading, with the index within that level, so titles can differ."""
    out: list[tuple[int, int]] = []
    counters = {2: 0, 3: 0}
    for level, _title in HEADING.findall(path.read_text(encoding='utf-8')):
        depth = len(level)
        counters[depth] = counters.get(depth, 0) + 1
        for deeper in [k for k in counters if k > depth]:
            counters[deeper] = 0
        out.append((depth, counters[depth]))
    return out


def main() -> int:
    problems: list[str] = []
    en_files = sorted(p.name for p in EN.glob('*.md'))
    zh_files = sorted(p.name for p in ZH.glob('*.md'))
    if en_files != zh_files:
        problems.append(f'docs/en has {en_files}, docs/zh-CN has {zh_files}')
    for name in en_files:
        zh = ZH / name
        if not zh.exists():
            continue
        a, b = headings(EN / name), headings(zh)
        if a != b:
            problems.append(f'{name}: en has {len(a)} headings, zh has {len(b)}')
    if problems:
        print('bilingual docs have drifted:', file=sys.stderr)
        for problem in problems:
            print('  ' + problem, file=sys.stderr)
        print('\nKeep the section structure aligned (translate the titles, not the outline).',
              file=sys.stderr)
        return 1
    print(f'DOCS parity OK: {len(en_files)} file pairs, identical section structure')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
