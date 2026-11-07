"""Fixture conversion rejects unsupported AAC syntax instead of certifying it."""
import importlib
from pathlib import Path

import pytest


@pytest.mark.parametrize(('element', 'prediction', 'tns'), [(1, 0, 0), (0, 1, 0), (0, 0, 1)])
def test_unsupported_aac_syntax_is_rejected_before_reserialization(monkeypatch, element, prediction, tns):
    monkeypatch.syspath_prepend(str(Path(__file__).resolve().parents[1] / 'tools/fixtures'))
    parser = importlib.import_module('drm_aac_reserialize')
    # SCE header, no scale-factor bands. Optional tool bits are set independently.
    bits = f'{element:03b}0000{100:08b}0000{0:06b}{prediction:01b}0{tns:01b}0'
    data = int(bits.ljust(32, '0'), 2).to_bytes(4, 'big')
    with pytest.raises(ValueError, match='not supported|unsupported'):
        parser.parse_frame(data)


def test_supported_aac_units_still_reproduce_committed_drm_bytes(monkeypatch):
    root = Path(__file__).resolve().parents[1]
    monkeypatch.syspath_prepend(str(root / 'tools/fixtures'))
    parser = importlib.import_module('drm_aac_reserialize')
    source = (root / 'tests/fixtures/drm/aac_sine_24k.ga').read_bytes()
    expected = (root / 'tests/fixtures/drm/aac_sine_24k.drm').read_bytes()
    actual = bytearray()
    for offset in range(0, len(source), 320):
        frame = source[offset:offset + 320]
        bits = parser.reserialize_drm(frame, parser.parse_frame(frame))
        actual.extend(int(bits[start:start + 8].ljust(8, '0'), 2)
                      for start in range(0, len(bits), 8))
    assert actual == expected
