"""Integration: capture Dream's decoded audio through the output null sink.

Dream cannot decode the *audio* of the synthetic DecDRM fixture (its MSC frames
come back as CRC_ERROR - a DecDRM<->Dream interop limitation), but it decodes
real DRM signals. This test therefore runs Dream on a real recording supplied via
``DRM_DREAM_AUDIO_FIXTURE`` (Dream's own ``drm-XHE-AAC-iq.rec`` MDI sample works)
and asserts the PCM capture plumbing returns non-silent mono int16.

Skipped when the fixture, PulseAudio or the vendored Dream are unavailable.
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
AUDIO_FIXTURE = os.environ.get('DRM_DREAM_AUDIO_FIXTURE', '')


def _available() -> bool:
    return (bool(AUDIO_FIXTURE) and Path(AUDIO_FIXTURE).exists()
            and DRM_BIN.exists() and all(shutil.which(t) for t in ('pactl', 'pacat', 'parec')))


pytestmark = pytest.mark.skipif(
    not _available(),
    reason='set DRM_DREAM_AUDIO_FIXTURE to a Dream-decodable recording to run this test')


def test_audio_capture_returns_non_silent_pcm(tmp_path):
    dec = DreamDecoder(dream_bin=str(DRM_BIN), input_file=AUDIO_FIXTURE,
                       status_socket=str(tmp_path / 'audio.sock'),
                       sink_name='drm_dream_test_a_in',
                       audio_sink_name='drm_dream_test_a_out',
                       capture_audio=True)
    dec.start()
    try:
        deadline = time.time() + 25.0
        pcm = np.zeros(0, dtype=np.int16)
        while time.time() < deadline:
            time.sleep(0.5)
            pcm = dec.drain_audio()
            if pcm.size > 4800 and float(np.sqrt(np.mean(pcm.astype(np.float64) ** 2))) > 100.0:
                break
        assert pcm.size > 0, 'no PCM captured from Dream'
        rms = float(np.sqrt(np.mean(pcm.astype(np.float64) ** 2)))
        assert rms > 100.0, f'captured audio is silent (rms={rms:.1f})'
    finally:
        dec.stop()
    assert not dec.alive
