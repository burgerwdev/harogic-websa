#!/usr/bin/env python3
"""Keep the version in one place.

`pyproject.toml` is the single source of truth (report finding P0-5); the two files that
must display it are rewritten from it:

  frontend/modern/package.json   "version"
  frontend/modern/index.html     the top-bar `vX.Y.Z` tag

Usage:
    python3 tools/sync_version.py            # apply
    python3 tools/sync_version.py --check    # fail when a file is out of step (CI)
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PYPROJECT = ROOT / 'pyproject.toml'
PACKAGE = ROOT / 'frontend' / 'modern' / 'package.json'
INDEX = ROOT / 'frontend' / 'modern' / 'index.html'

VERSION_TAG = re.compile(r'(<span class="version-tag"[^>]*>v)(\d+\.\d+\.\d+)')


def read_version() -> str:
    text = PYPROJECT.read_text(encoding='utf-8')
    match = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    if not match:
        raise SystemExit('pyproject.toml has no [project] version')
    return match.group(1)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()

    version = read_version()
    problems = []

    pkg_text = PACKAGE.read_text(encoding='utf-8')
    pkg = json.loads(pkg_text)
    if pkg.get('version') != version:
        problems.append(f'package.json: {pkg.get("version")} != {version}')
        if not args.check:
            # Rewrite only the version line so the file keeps its formatting.
            pkg_text = re.sub(r'("version"\s*:\s*")[^"]+(")',
                              lambda m: m.group(1) + version + m.group(2), pkg_text, count=1)
            PACKAGE.write_text(pkg_text, encoding='utf-8')

    index_text = INDEX.read_text(encoding='utf-8')
    found = VERSION_TAG.search(index_text)
    if not found:
        problems.append('index.html: no version-tag span found')
    elif found.group(2) != version:
        problems.append(f'index.html: {found.group(2)} != {version}')
        if not args.check:
            INDEX.write_text(VERSION_TAG.sub(lambda m: m.group(1) + version, index_text, count=1),
                             encoding='utf-8')

    if args.check and problems:
        print('version drift (run python3 tools/sync_version.py):', file=sys.stderr)
        for problem in problems:
            print('  ' + problem, file=sys.stderr)
        return 1
    print(f'version {version}: ' + ('in sync' if not problems else 'updated'))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
