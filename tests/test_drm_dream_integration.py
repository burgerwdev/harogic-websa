"""Integration: feed the DRM fixture through DreamDecoder's real (loopback) path.

Skipped unless PulseAudio/PipeWire and the vendored console Dream exist. This is
the end-to-end proof for task-3: complex baseband -> feed() -> pacat -> null sink
-> Dream -> --status-socket -> parsed metadata.
"""
from __future__ import annotations

import os
import shutil
import time
from pathlib import Path

import numpy as np
import pytest

from web_sa.drm_dream import DreamDecoder

REPO = Path(__file__).resolve().parent.parent
DRM_BIN = Path(os.environ.get('DRM_DREAM_BIN', REPO / 'tools/drm_dream/build/dream'))
FIXTURE = REPO / 'tests/fixtures/drm/drm_modeB_so3_48k.f32'
RATE = 48000
BLOCK = 4800   # 100 ms


def _available() -> bool:
    return (all(shutil.which(t) for t in ('pactl', 'pacat', 'parec'))
            and DRM_BIN.exists() and FIXTURE.exists())


pytestmark = pytest.mark.skipif(not _available(),
                                reason='needs pactl/pacat/parec and the vendored dream build')


def test_decoder_decodes_fixture_through_loopback(tmp_path):
    iq = np.fromfile(FIXTURE, dtype='<f4').reshape(-1, 2)
    i = iq[:, 0]
    q = iq[:, 1]
    dec = DreamDecoder(dream_bin=str(DRM_BIN),
                       status_socket=str(tmp_path / 'status.sock'),
                       sink_name='drm_dream_test_feed',
                       audio_sink_name='drm_dream_test_audio',
                       capture_audio=False)
    dec.start()
    pid = dec.pid
    try:
        pos = 0
        deadline = time.time() + 25.0
        meta = {}
        while time.time() < deadline:
            if pos + BLOCK > i.size:
                pos = 0
            dec.feed(i[pos:pos + BLOCK], q[pos:pos + BLOCK])
            pos += BLOCK
            time.sleep(BLOCK / RATE)
            meta = dec.metadata()
            if meta.get('station') == 'SAN90 DRM TEST':
                break
        assert meta.get('station') == 'SAN90 DRM TEST', meta
        assert meta.get('robustness') == 'B'
        assert meta.get('bandwidth_khz') == 10.0
        assert meta.get('bitrate_kbps') == pytest.approx(20.96, abs=0.1)
        assert dec.status_events > 0
    finally:
        dec.stop()

    assert not dec.alive
    assert dec.metadata() == {}
    for _ in range(50):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.1)
    else:
        raise AssertionError(f'dream child {pid} survived stop()')
