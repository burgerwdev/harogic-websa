#!/usr/bin/env python3
"""Bilingual docs must keep the same section structure (report finding P2-5).

The English and Chinese documents are maintained by hand, and they had already drifted
(ARCHITECTURE.md 126 vs 118 lines, KNOWN_ISSUES.md 62 vs 51) with nothing to catch it.
Wording may be translated; the *structure* may not diverge, or a reader of one language
silently loses a section that exists in the other.

Compares the `##`/`###` heading sequence of every docs/en/*.md with its zh-CN counterpart,
ignoring the translated titles (a heading is identified by its level and its position).

It also compares the **status checklists**: every `[done]`/`[todo]`/`[blocked]`/`[unverified]`
marker in a section must appear in the same order and number in both languages. That gap is how
the VSA roadmap drifted (the English sections were ticked while the Chinese ones still said
todo, and a heading-only check cannot see it).

    python3 tools/check_docs_parity.py
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EN = ROOT / 'docs' / 'en'
ZH = ROOT / 'docs' / 'zh-CN'

HEADING = re.compile(r'^(#{2,3})\s+(.*)$', re.M)
#: Status markers, one canonical name per language (order matters for the sequence compare).
STATUS = {
    'done': '[done]',
    'todo': '[todo]',
    'blocked': '[blocked]',
    'unverified': '[unverified]',
    '已做': '[已做]',
    '待做': '[待做]',
    '受阻': '[受阻]',
    '未验证': '[未验证]',
}
CANON = {'done': 'done', 'todo': 'todo', 'blocked': 'blocked', 'unverified': 'unverified',
         '已做': 'done', '待做': 'todo', '受阻': 'blocked', '未验证': 'unverified'}


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


def status_sequence(path: Path) -> list[str]:
    """The status markers in order, as canonical names (see the module docstring)."""
    text = path.read_text(encoding='utf-8')
    out: list[str] = []
    for marker in re.finditer(r'\[(\w+|\u4e0d\u5b58\u5728)\]', text):
        key = marker.group(1)
        if key in CANON:
            out.append(CANON[key])
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
        sa, sb = status_sequence(EN / name), status_sequence(zh)
        if sa != sb:
            first = next((i for i, (x, y) in enumerate(zip(sa, sb, strict=False)) if x != y), min(len(sa), len(sb)))
            problems.append(
                f'{name}: status checklists differ at item {first + 1} '
                f'(en {sa[first:first + 3]} vs zh {sb[first:first + 3]}); '
                f'en has {len(sa)} markers, zh has {len(sb)}')
    if problems:
        print('bilingual docs have drifted:', file=sys.stderr)
        for problem in problems:
            print('  ' + problem, file=sys.stderr)
        print('\nKeep the section structure aligned (translate the titles, not the outline).',
              file=sys.stderr)
        return 1
    print(f'DOCS parity OK: {len(en_files)} file pairs, identical section structure and status checklists')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
