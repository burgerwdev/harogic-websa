#!/usr/bin/env python3
"""Check architecture source paths and local links in maintained documentation.

Expands `{a,b}` groups and accepts files or directories. Glob and placeholder paths
are skipped; the stage map should use concrete implementation paths where possible.

    python3 tools/checks/check_doc_paths.py
"""
from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent.parent.parent
DOCS = [ROOT / 'docs' / 'en' / 'ARCHITECTURE.md', ROOT / 'docs' / 'zh-CN' / 'ARCHITECTURE.md']

# Read code spans only within the stage map, ending at the next peer/parent heading.
STAGE_MAP = re.compile(r'^### (?:Stage map|阶段映射)\s*\n(.*?)(?=^#{1,3} |\Z)', re.S | re.M)
PATH_LIKE = re.compile(r'^[A-Za-z0-9_./{},*-]+/[A-Za-z0-9_./{},*-]*(\.[A-Za-z0-9]+|/)$')
BRACES = re.compile(r'\{([^}]*)\}')
LINK = re.compile(r'!?\[[^\]\n]*\]\(\s*(?:<([^>\n]+)>|([^\s)]+))(?:\s+"[^"\n]*")?\s*\)')


def broken_links(doc: Path) -> list[str]:
    """Resolve inline Markdown links relative to their document, ignoring examples."""
    content = re.sub(r'^(`{3,}|~{3,}).*?^\1[^\n]*$', '',
                     doc.read_text(encoding='utf-8'), flags=re.M | re.S)
    content = re.sub(r'`[^`\n]+`', '', content)
    missing = []
    for bracketed, plain in LINK.findall(content):
        target = bracketed or plain
        url = urlsplit(target)
        if url.scheme or url.netloc or not url.path:
            continue
        path = unquote(url.path)
        resolved = ROOT / path.lstrip('/') if path.startswith('/') else doc.parent / path
        if not resolved.exists():
            missing.append(target)
    return missing


def maintained_docs() -> list[Path]:
    return sorted({*ROOT.glob('README*.md'), *(ROOT / 'docs').rglob('*.md'),
                   *(ROOT / 'tools').glob('README*.md'),
                   *(ROOT / 'tests' / 'fixtures').rglob('README*.md')})


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
    documents = maintained_docs()
    for doc in documents:
        problems.extend(f'{doc.relative_to(ROOT)}: broken link {target}'
                        for target in broken_links(doc))
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
        print('\nUpdate the document links and source paths to match the repository.',
              file=sys.stderr)
        return 1
    print(f'doc paths OK: {checked} source paths; local links in {len(documents)} documents')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
