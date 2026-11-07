"""Document moves must not leave broken local links hidden by a passing source map."""
import json
import re
from pathlib import Path

import pytest
from jsonschema import Draft202012Validator

from tools.checks import check_docs_parity
from tools.checks.check_doc_paths import broken_links


@pytest.mark.parametrize('language', ['en', 'zh-CN'])
def test_reference_json_examples_are_parseable(language):
    root = Path(__file__).resolve().parents[1] / 'docs' / language / 'reference'
    for document in sorted(root.glob('*.md')):
        for example in re.finditer(r'^```json\n(.*?)\n```', document.read_text(), re.M | re.S):
            try:
                json.loads(example[1])
            except ValueError as exc:
                pytest.fail(f'{document.name} JSON example at line '
                            f'{document.read_text()[:example.start()].count(chr(10)) + 1}: {exc}')


@pytest.mark.parametrize('language', ['en', 'zh-CN'])
def test_receiver_reference_examples_match_portable_schema(language):
    root = Path(__file__).resolve().parents[1]
    schema = json.loads((root / 'tests/fixtures/multichannel/draft-v1.schema.json').read_text())
    Draft202012Validator.check_schema(schema)
    document = root / 'docs' / language / 'reference/RECEIVERS.md'
    for example in re.finditer(r'^```json\n(.*?)\n```', document.read_text(), re.M | re.S):
        Draft202012Validator(schema).validate(json.loads(example[1]))


def test_resolves_local_documents_images_and_encoded_names(tmp_path):
    (tmp_path / 'guide name.md').write_text('# Guide')
    (tmp_path / 'plot.png').write_bytes(b'fixture')
    doc = tmp_path / 'index.md'
    doc.write_text('[guide](guide%20name.md#build) ![plot](plot.png) '
                   '[title](<guide name.md> "Guide") [missing](missing.md#run)')
    assert broken_links(doc) == ['missing.md#run']


def test_ignores_remote_links_anchors_and_code_examples(tmp_path):
    doc = tmp_path / 'index.md'
    doc.write_text('[web](https://example.com/page) [anchor](#build)\n'
                   '`[inline](missing.md)`\n```md\n[example](missing.md)\n```\n'
                   '~~~md\n[example](missing.md)\n~~~\n[real](gone.md)\n')
    assert broken_links(doc) == ['gone.md']


@pytest.mark.parametrize('change', ['heading', 'missing'])
def test_bilingual_check_covers_nested_reference_documents(tmp_path, monkeypatch, change):
    en, zh = tmp_path / 'en', tmp_path / 'zh-CN'
    for directory in (en, zh):
        (directory / 'reference').mkdir(parents=True)
        (directory / 'reference/API.md').write_text('# API\n\n## Commands\n')
    monkeypatch.setattr(check_docs_parity, 'EN', en)
    monkeypatch.setattr(check_docs_parity, 'ZH', zh)
    assert check_docs_parity.main() == 0
    target = zh / 'reference/API.md'
    if change == 'missing':
        target.unlink()
    else:
        target.write_text('# API\n\n### Commands\n')
    assert check_docs_parity.main() == 1
