"""Live path: the real SdrSession DRM glue driving the real Dream decoder.

Unlike the stub-based unit tests, this calls the production `_drm_frames_locked` with a
DreamDecoder that actually runs Dream over the PulseAudio loopback, so the chain
"channelized baseband -> low-pass + resample -> pacat -> null sink -> dream -> --status-socket ->
state.sdr_drm" is exercised as one piece. Only the hardware DDC is replaced by the fixture I/Q.

Skipped without PulseAudio/PipeWire, the vendored Dream build, or the fixture.
"""
from __future__ import annotations

import os
import shutil
import time
from pathlib import Path
from types import SimpleNamespace

import numpy as np
import pytest

try:
    from web_sa.drm_dream import DreamDecoder
    from web_sa.measurements.sdr import SdrSession
    _IMPORT_OK = True
except OSError as exc:                        # vendor SDK missing
    _IMPORT_OK = False
    _IMPORT_ERROR = str(exc)

REPO = Path(__file__).resolve().parent.parent
DRM_BIN = Path(os.environ.get('DRM_DREAM_BIN', REPO / 'tools/drm_dream/build/dream'))
FIXTURE = REPO / 'tests/fixtures/drm/drm_modeB_so3_48k.f32'
RATE = 48000
BLOCK = 4800                                   # 100 ms


def _available() -> bool:
    return (_IMPORT_OK and DRM_BIN.exists() and FIXTURE.exists()
            and all(shutil.which(t) for t in ('pactl', 'pacat', 'parec')))


pytestmark = pytest.mark.skipif(not _available(), reason='needs vendor SDK, PulseAudio and dream')


def _session(decoder) -> SdrSession:
    sess = SdrSession.__new__(SdrSession)
    sess.dev = SimpleNamespace(state=SimpleNamespace(
        sdr_demod='drm', sdr_volume=1.0, sdr_squelch=-110.0, sdr_level_dbfs=-120.0,
        sdr_squelch_open=False, sdr_drm={}))
    sess._drm = decoder
    sess._drm_error = ''
    sess._drm_geo = 0.0
    sess._drm_lp_i = sess._drm_lp_q = None
    sess._drm_res_i = sess._drm_res_q = None
    sess._drm_audio = np.zeros(0, dtype=np.float32)
    sess._last_drm_meta = 0.0
    sess._ddc = SimpleNamespace(fs_out=float(RATE))
    sess._audio_seq = 0
    sess._audio_reset_pending = False
    return sess


def test_live_ddc_to_dream_decodes_metadata(tmp_path):
    iq = np.fromfile(FIXTURE, dtype='<f4').reshape(-1, 2)
    dec = DreamDecoder(dream_bin=str(DRM_BIN), status_socket=str(tmp_path / 'live.sock'),
                       sink_name='drm_dream_live_in', audio_sink_name='drm_dream_live_out',
                       capture_audio=False)
    sess = _session(dec)
    dec.start()
    pid = dec.pid
    try:
        pos = 0
        deadline = time.time() + 25.0
        meta: dict = {}
        while time.time() < deadline:
            if pos + BLOCK > iq.shape[0]:
                pos = 0
            block_i = iq[pos:pos + BLOCK, 0].copy()
            block_q = iq[pos:pos + BLOCK, 1].copy()
            sess._drm_frames_locked([], block_i, block_q)
            pos += BLOCK
            time.sleep(BLOCK / RATE)
            meta = sess.dev.state.sdr_drm
            if meta.get('station') == 'SAN90 DRM TEST':
                break
        assert meta.get('station') == 'SAN90 DRM TEST', meta
        assert meta.get('robustness') == 'B'
        assert meta.get('bandwidth_khz') == 10.0
    finally:
        dec.stop()
    for _ in range(50):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.1)
    else:
        raise AssertionError(f'dream child {pid} survived stop()')
