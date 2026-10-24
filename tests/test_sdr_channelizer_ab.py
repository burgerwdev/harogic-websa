import numpy as np

from tools.bench.sdr_channelizer_ab import _tone_quality


def test_known_tone_residual_is_reported_in_dbc():
    fs = 48_000
    t = np.arange(4800) / fs
    audio = 2 * np.sin(2 * np.pi * 1000 * t) + 0.02 * np.sin(2 * np.pi * 3000 * t)
    result = _tone_quality(audio, fs, 1000)
    assert result['tone_rms'] == 1.4142
    assert result['residual_dbc'] == -40.0
