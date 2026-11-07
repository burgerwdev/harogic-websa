"""PCM interpolation must preserve endpoints and handle short WAVs explicitly."""
import wave

import numpy as np
import pytest

from tools.pluto import pluto_analog_tx as tx


@pytest.mark.parametrize('frames', [1, 2])
@pytest.mark.parametrize('channels', [1, 2])
def test_audio_interpolation_holds_the_last_sample(tmp_path, frames, channels):
    path = tmp_path / 'short.wav'
    data = np.array([[1000, -1000], [4000, -4000]][:frames], dtype='<i2')[:, :channels]
    with wave.open(str(path), 'wb') as output:
        output.setparams((channels, 2, 8000, frames, 'NONE', 'not compressed'))
        output.writeframes(data.tobytes())
    positions = np.arange(frames * 2) / 2
    left_expected = np.interp(positions, np.arange(frames), data[:, 0]) / 32768
    right_expected = np.interp(positions, np.arange(frames), data[:, channels - 1]) / 32768
    np.testing.assert_allclose(tx.load_audio(str(path), 16000, 'left'), left_expected, atol=0, rtol=0)
    left, right = tx.load_audio_two(str(path), 16000)
    np.testing.assert_allclose(left, left_expected, atol=0, rtol=0)
    np.testing.assert_allclose(right, right_expected, atol=0, rtol=0)


def test_empty_audio_is_a_clear_error(tmp_path):
    path = tmp_path / 'empty.wav'
    with wave.open(str(path), 'wb') as output:
        output.setparams((1, 2, 8000, 0, 'NONE', 'not compressed'))
    with pytest.raises(ValueError, match='empty'):
        tx.load_audio(str(path), 8000)
