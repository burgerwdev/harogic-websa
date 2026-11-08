"""Offline captures must contain one continuous, bounded baseband stream."""
import asyncio
from types import SimpleNamespace

import aiohttp
import numpy as np
import pytest

from tools.bench import drm_capture
from tools.reference.framer import BASEBAND_HEADER, MAGIC_BASEBAND, encode_baseband


def frame(seq=1, *, epoch=7, channel_id=2, rate=24000., center=435e6, iq=None):
    return encode_baseband(seq, rate, center, [0.1, -0.1, 0.2, -0.2] if iq is None else iq,
                           channel_id=channel_id, epoch=epoch, input_index=seq * 1024,
                           delay_samples=1.5)


def capture(monkeypatch, packets, max_bytes=1024):
    class Session:
        async def __aenter__(self):
            return self
        async def __aexit__(self, *_):
            pass
        def ws_connect(self, *_args, **_kwargs):
            return self
        async def receive(self):
            if packets:
                return SimpleNamespace(type=aiohttp.WSMsgType.BINARY, data=packets.pop(0))
            return SimpleNamespace(type=aiohttp.WSMsgType.CLOSED)
    monkeypatch.setattr(drm_capture.aiohttp, 'ClientSession', lambda **_: Session())
    return asyncio.run(drm_capture.capture('http://test', 1., max_bytes, settle=0.))


@pytest.mark.parametrize('bad', [frame(rate=float('nan')), frame(rate=0),
                                 frame(center=float('inf')), frame(iq=[float('nan'), 0.]),
                                 MAGIC_BASEBAND + BASEBAND_HEADER.pack(2, 1, 2, 24000., 435e6, 2, 7, 0, 1.5)])
def test_invalid_iqbf_is_an_error_not_silent_missing_evidence(bad):
    with pytest.raises(ValueError):
        drm_capture.decode_baseband(bad)


@pytest.mark.parametrize('second', [frame(2, epoch=8), frame(2, channel_id=3),
                                    frame(2, rate=12000.), frame(2, center=436e6),
                                    frame(3), frame(1), frame(0, iq=[])])
def test_capture_rejects_identity_geometry_gap_duplicate_and_reset(monkeypatch, second):
    with pytest.raises(ValueError):
        capture(monkeypatch, [frame(), second])


def test_byte_limit_is_enforced_before_appending_an_entire_frame(monkeypatch):
    result = capture(monkeypatch, [frame(), frame(2)], max_bytes=24)
    assert result['iq'].nbytes == 16
    assert result['meta']['samples'] == 2


def test_contiguous_capture_preserves_samples(monkeypatch):
    result = capture(monkeypatch, [frame(), frame(2)])
    assert result['meta']['seq_gaps'] == 0
    assert result['meta']['frames'] == 2
    np.testing.assert_array_equal(result['iq'], np.array([0.1, -0.1, 0.2, -0.2] * 2, np.float32))
