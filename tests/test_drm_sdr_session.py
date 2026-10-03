"""SdrSession's DRM glue: the DDC baseband -> Dream feed, AUDF framing and metadata publish.

These exercise the actual production methods (`_drm_frames_locked`, `_sync_drm_locked`,
`_publish_drm`) rather than a re-implementation, so the integration the UI depends on is covered
without hardware. The vendor binding layer is imported by `measurements.sdr`, so the module is
skipped where it is unavailable.
"""
from __future__ import annotations

from types import SimpleNamespace

import numpy as np
import pytest

try:
    from web_sa.measurements import sdr as sdr_module
    from web_sa.measurements.framer import AUDIO_HEADER, MAGIC_AUDIO
    from web_sa.measurements.sdr import DRM_RATE, SdrSession
except OSError as exc:                       # vendor SDK missing (CI / contributor machine)
    pytest.skip(f'vendor SDK unavailable: {exc}', allow_module_level=True)


class StubDecoder:
    """Stands in for DreamDecoder: records what it is fed, replays canned PCM and metadata."""

    def __init__(self, pcm: np.ndarray | None = None):
        self.fed: list[tuple[np.ndarray, np.ndarray]] = []
        self._pcm = np.zeros(0, dtype=np.int16) if pcm is None else pcm
        self.alive = True
        self.dropped_blocks = 0
        self.last_error = ''
        self.meta = {'station': 'SAN90 DRM TEST', 'robustness': 'B', 'sync': True}

    def feed(self, i, q) -> int:
        self.fed.append((np.asarray(i).copy(), np.asarray(q).copy()))
        return int(np.asarray(i).size)

    def drain_audio(self) -> np.ndarray:
        pcm = self._pcm
        self._pcm = np.zeros(0, dtype=np.int16)
        return pcm

    def metadata(self) -> dict:
        return dict(self.meta)

    def status(self) -> dict:
        # The decoded PCM is gated on the MSC being clean; the stub defaults to locked.
        return {'status': {'msc': self.msc}}

    msc = 0


def _state(demod: str = 'drm') -> SimpleNamespace:
    return SimpleNamespace(sdr_demod=demod, sdr_volume=1.0, sdr_squelch=-110.0,
                           sdr_level_dbfs=-120.0, sdr_squelch_open=False, sdr_drm={})


def _session(decoder, fs_out: float = 48000.0, state: SimpleNamespace | None = None) -> SdrSession:
    """A bare SdrSession with only the attributes the DRM path touches (no SDK/hardware init)."""
    sess = SdrSession.__new__(SdrSession)
    sess.dev = SimpleNamespace(state=state or _state())
    sess._drm = decoder
    sess._drm_error = ''
    sess._drm_geo = 0.0
    sess._drm_lp_i = sess._drm_lp_q = None
    sess._drm_res_i = sess._drm_res_q = None
    sess._drm_audio = np.zeros(0, dtype=np.float32)
    sess._last_drm_meta = 0.0
    sess._drm_audio_ok_until = 0.0
    sess._ddc = SimpleNamespace(fs_out=fs_out)
    sess._audio_seq = 0
    sess._audio_reset_pending = False
    return sess


def _audio_frames(frames: list[bytes]) -> list[tuple[int, int, int]]:
    out = []
    for frame in frames:
        if frame[:4] == MAGIC_AUDIO:
            seq, rate, samples = AUDIO_HEADER.unpack_from(frame, 4)
            out.append((seq, rate, samples))
    return out


def test_drm_frames_feed_the_decoder_and_emit_audf():
    pcm = (np.sin(2 * np.pi * 1000 * np.arange(960) / DRM_RATE) * 10000).astype(np.int16)
    dec = StubDecoder(pcm=pcm)
    sess = _session(dec)
    n = 4800
    t = np.arange(n) / DRM_RATE
    i = np.cos(2 * np.pi * 1000 * t).astype(np.float32)
    q = np.sin(2 * np.pi * 1000 * t).astype(np.float32)

    frames: list[bytes] = []
    sess._drm_frames_locked(frames, i, q)

    assert dec.fed, 'the DDC block was not handed to Dream'
    assert sum(x[0].size for x in dec.fed) > 0
    aud = _audio_frames(frames)
    assert aud, 'no AUDF frame was published for the decoded audio'
    seq, rate, samples = aud[0]
    assert rate == DRM_RATE and samples == sess.AUDIO_FRAME
    assert sess.dev.state.sdr_drm['station'] == 'SAN90 DRM TEST'
    assert sess.dev.state.sdr_drm['active'] is True
    assert sess.dev.state.sdr_level_dbfs > -120.0


def test_drm_frames_resample_an_off_rate_ddc_output_to_48k():
    dec = StubDecoder(pcm=np.zeros(0, dtype=np.int16))
    sess = _session(dec, fs_out=61440.0)          # 1.28x the Dream rate
    n = 6144                                       # 0.1 s at 61.44 kHz
    i = np.ones(n, dtype=np.float32)
    q = np.zeros(n, dtype=np.float32)

    sess._drm_frames_locked([], i, q)

    fed = sum(x[0].size for x in dec.fed)
    # 0.1 s at 48 kHz, less the first low-pass block's transient.
    assert fed == pytest.approx(4800, rel=0.1), fed


def test_drm_frames_drop_unlocked_noise(monkeypatch):
    """Dream emits full-scale noise when it is not decoding; it must not reach the speaker."""
    pcm = np.full(960, 20000, dtype=np.int16)          # the idle "noise" Dream emits
    dec = StubDecoder(pcm=pcm)
    dec.msc = 1                                        # CRC error / no programme
    sess = _session(dec)
    i = np.ones(4800, dtype=np.float32)
    q = np.zeros(4800, dtype=np.float32)
    frames: list[bytes] = []

    sess._drm_frames_locked(frames, i, q)

    assert _audio_frames(frames) == [], 'gated audio must not be published'
    assert sess.dev.state.sdr_level_dbfs == -120.0
    assert sess.dev.state.sdr_squelch_open is False


def test_publish_reports_a_dead_decoder():
    dec = StubDecoder()
    dec.alive = False
    dec.last_error = 'boom'
    sess = _session(dec)
    sess._publish_drm(sess.dev.state, force=True)
    assert sess.dev.state.sdr_drm['active'] is False
    assert sess.dev.state.sdr_drm['error'] == 'boom'


def test_sync_drm_starts_and_stops_the_decoder(monkeypatch):
    events: list[str] = []

    class FakeDecoder:
        def __init__(self, **_kwargs):
            self.pid = 4242
            self.alive = True
            self.last_error = ''
            self.dropped_blocks = 0

        def start(self):
            events.append('start')

        def stop(self):
            events.append('stop')

        def metadata(self):
            return {'station': 'SAN90 DRM TEST'}

    monkeypatch.setattr(sdr_module, 'DreamDecoder', FakeDecoder)
    monkeypatch.setattr(sdr_module, '_default_dream_bin', lambda: sdr_module.__file__)
    state = _state('drm')
    sess = _session(None, state=state)

    sess._sync_drm_locked()
    assert events == ['start']
    assert sess._drm is not None
    assert state.sdr_drm.get('active') is True

    state.sdr_demod = 'am'
    sess._sync_drm_locked()
    assert events == ['start', 'stop']
    assert sess._drm is None
    assert state.sdr_drm['active'] is False


def test_sync_drm_reports_a_missing_binary(monkeypatch):
    monkeypatch.setattr(sdr_module, '_default_dream_bin', lambda: '/nonexistent/dream')
    state = _state('drm')
    sess = _session(None, state=state)
    sess._sync_drm_locked()
    assert sess._drm is None
    assert 'not found' in state.sdr_drm['error']


def test_default_dream_bin_honours_the_explicit_override(monkeypatch):
    monkeypatch.setenv('DRM_DREAM_BIN', '/explicit/dream')
    sdr_module._dream_bin_cache.clear()
    assert sdr_module._default_dream_bin() == '/explicit/dream'


def test_default_dream_bin_picks_the_first_capable_candidate(monkeypatch):
    """The installed binary is preferred, but only when it advertises --status-socket."""
    monkeypatch.delenv('DRM_DREAM_BIN', raising=False)
    monkeypatch.setattr(sdr_module, '_DRM_DREAM_CANDIDATES', ('/first/dream', '/second/dream'))
    monkeypatch.setattr(sdr_module.os.path, 'exists', lambda _path: True)
    monkeypatch.setattr(sdr_module, '_dream_supports_status_socket',
                        lambda path: path == '/second/dream')
    sdr_module._dream_bin_cache.clear()
    try:
        # The first candidate lacks the status socket, so the second (installed) one is used.
        assert sdr_module._default_dream_bin() == '/second/dream'
    finally:
        sdr_module._dream_bin_cache.clear()


def test_chain_params_force_a_48k_ddc_for_drm():
    """DRM ignores the wide IF filter for the DDC rate: Dream needs ~48 kHz, not 488 kHz."""
    sess = _session(None)
    sess._fs_in = 488281.25
    sess.dev.state.sdr_if_bw = 180000.0
    if_bw, decimate = sess._chain_params()
    assert if_bw == 180000.0
    assert decimate == 10                                  # floor(488281.25 / 48000)
    assert sess._fs_in / decimate == pytest.approx(48828.1, rel=0.01)


def test_chain_params_keep_the_wide_ddc_for_analog_modes():
    sess = _session(None, state=_state('am'))
    sess._fs_in = 488281.25
    sess.dev.state.sdr_if_bw = 180000.0
    _if_bw, decimate = sess._chain_params()
    assert decimate == 1                                   # 1.6 * 180 kHz > 48 kHz
