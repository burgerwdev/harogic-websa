"""Independent standard-message vectors from ft8_lib's message.c protocol mapping.

https://github.com/kgoba/ft8_lib/blob/master/ft8/message.c, pack28/pack_basecall.
These numeric fields do not use the Rust decoder or regenerate committed IQ.
"""
import importlib
from pathlib import Path

import pytest


@pytest.fixture
def encoder(monkeypatch):
    monkeypatch.syspath_prepend(str(Path(__file__).resolve().parents[1] / 'tools/fixtures'))
    return importlib.import_module('gen_ft8_fixtures')


@pytest.mark.parametrize(('text', 'token'), [
    ('CQ 000', 3), ('CQ 999', 1002), ('CQ A', 1004), ('CQ DX', 1135),
    ('CQ ABCD', 22229), ('CQ ZZZZ', 532443),
    ('K1ABC', 10214965), ('3DA0XYZ', 37178403), ('3XA0XYZ', 199563153),
])
def test_callsign_protocol_fields(encoder, text, token):
    assert encoder.pack28(text) == (token, 0)


@pytest.mark.parametrize('text', ['CQ A1BC', 'CQ 12', 'CQ ABCDE', 'CQ １２３', 'A1BCDE'])
def test_unrepresentable_callsigns_are_rejected(encoder, text):
    with pytest.raises(SystemExit):
        encoder.pack28(text)


@pytest.mark.parametrize(('message', 'to_field', 'from_field', 'kind'), [
    ('CQ DX K1ABC', 1135 * 2, 10214965 * 2, 1),
    ('CQ 123 K1ABC', 126 * 2, 10214965 * 2, 1),
    ('K1ABC/R K1ABC', 10214965 * 2 + 1, 10214965 * 2, 1),
    ('K1ABC K1ABC/P', 10214965 * 2, 10214965 * 2 + 1, 2),
    ('K1ABC/P K1ABC/P', 10214965 * 2 + 1, 10214965 * 2 + 1, 2),
])
def test_standard_message_fields_preserve_cq_and_suffixes(encoder, message, to_field, from_field, kind):
    packed, bits = encoder.standard_payload(message)
    # 29 + 29 callsign bits, separate R flag, 15-bit blank report, 3-bit message type.
    expected = (to_field << 48) | (from_field << 19) | (32401 << 3) | kind
    assert packed == expected
    assert bits == [int(bit) for bit in f'{expected:077b}']


@pytest.mark.parametrize('message', ['K1ABC/R K1ABC/P', 'K1ABC/P K1ABC/R'])
def test_mixed_rover_and_portable_suffixes_cannot_share_a_message_type(encoder, message):
    with pytest.raises(SystemExit):
        encoder.standard_payload(message)
