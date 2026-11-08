"""Protocol vectors and independent decoder replay, without radio access."""
import json
import os
import subprocess
from pathlib import Path

import numpy as np
import pytest

from tools.pluto.rtty_encode import codes, source


def test_ita2_known_letters_figures_and_shift_back():
    assert codes('RY 73 R') == [31, 31, 10, 21, 4, 27, 7, 1, 4, 31, 10]
    for text in ('', '\0', 'HELLO ☃', 'A' * 257):
        with pytest.raises(ValueError):
            codes(text)


@pytest.mark.parametrize('reverse', [False, True])
def test_rtty_has_continuous_phase_and_exact_global_symbol_clock(reverse):
    signal, rate, period, expected, phase = source('RY 73', reverse=reverse)
    assert rate == 48000 and expected == 'RY 73'
    assert len(signal) == round((30 + 7.5 * len(codes(expected))) * rate / 45.45)
    assert period == len(signal) / rate and np.isfinite(phase)
    np.testing.assert_allclose(np.abs(signal), 1, atol=2e-7)
    frequency = np.angle(signal[1:] * signal[:-1].conj()) * rate / (2 * np.pi)
    assert abs(float(np.mean(frequency[:1000])) - (2295 if reverse else 2125)) < .02
    assert np.max(np.abs(np.diff(frequency))) < 171  # No phase resets at frame/bit edges.


@pytest.mark.parametrize('reverse', [False, True])
def test_rtty_round_trips_through_independent_onair_companion(reverse):
    executable = Path(os.environ.get('WEBSA_RTTY_PATH', str(Path.home() / '.local/share/websa/decoders/bin/websa-rtty')))
    if not executable.is_file():
        pytest.skip('optional independently installed websa-rtty companion is unavailable')
    text = 'RYRY CQ CQ DE N0CALL TEST 12345\r\n'
    signal, *_ = source(text, reverse=reverse)
    # The external companion consumes real 8 kHz audio. These two tones lie
    # below Nyquist; this replay is independent of the transmitter's framing.
    audio = np.asarray(signal.real[::6], dtype='<f4')
    result = subprocess.run([str(executable), '--baud', '45.45', '--shift', '170',
                             '--low-tone', '2125', '--usos', '0', '--reverse', str(int(reverse))],
                            input=audio.tobytes(), capture_output=True, check=True, timeout=12)
    records = [json.loads(line) for line in result.stdout.decode().splitlines() if line.startswith('{')]
    decoded = ''.join(str(record.get('text', '')) for record in records)
    assert 'CQ CQ DE N0CALL TEST 12345' in decoded, decoded
