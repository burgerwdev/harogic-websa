"""Rust native channelizer ABI and packet continuity (opt-in path only)."""
import shutil
import subprocess
from pathlib import Path

import numpy as np
import pytest

from tools.reference.native_ddc import NativeDdcChannel


@pytest.fixture(scope='module')
def library():
    if not shutil.which('cargo'):
        pytest.skip('cargo not available')
    crate = Path(__file__).resolve().parents[1] / 'native-ddc'
    subprocess.run(['cargo', 'build', '-j', '1', '--release', '--lib', '--manifest-path', str(crate / 'Cargo.toml')],
                   check=True)
    return str(crate / 'target/release/libwebsa_channelizer.so')


def test_packet_clock_retune_and_input_bounds(library):
    channel = NativeDdcChannel(None, library)
    iq = np.zeros(16240 * 2, np.int16)
    with pytest.raises(ValueError, match='contiguous'):
        channel.configure(31_250_000.0, -300_000, 640, 16240)
        channel.process(iq[::2], 16240)
    counts = []
    for _ in range(32):
        consumed, produced = channel.clock()
        skip = channel._drop_pending
        i, q = channel.process(iq, 16240)
        assert i.shape == q.shape
        if i.size:
            assert consumed + channel.output_input_offset == (produced + skip + 1) * 640 - 1
        counts.append(i.size)
    assert sum(counts) == 812 - round(channel.delay)
    assert set(counts[1:]).issubset({25, 26})
    assert counts[0] == 25 - round(channel.delay)
    channel.configure(31_250_000.0, 300_000, 640, 16240)
    assert channel.process(iq, 16240)[0].size == 25 - round(channel.delay)
    with pytest.raises(ValueError, match='out of range'):
        channel.process(iq, 16241)
    with pytest.raises(RuntimeError, match='unsupported'):
        channel.configure(31_250_000.0, 0, 651, 16240)
    channel.close()


def test_independent_channels_share_input_without_phase_or_clock_crosstalk(library):
    fs, n = 31_250_000.0, 16240
    t = np.arange(n * 10) / fs
    z = 12000 * np.exp(2j * np.pi * 301_000 * t)
    iq = np.empty((n * 10, 2), dtype=np.int16)
    iq[:, 0] = np.rint(z.real).astype(np.int16)
    iq[:, 1] = np.rint(z.imag).astype(np.int16)
    blocks = iq.reshape(10, n * 2)
    first = NativeDdcChannel(None, library)
    second = NativeDdcChannel(None, library)
    reference = NativeDdcChannel(None, library)
    try:
        for channel, offset in ((first, -300_000), (second, 100_000),
                                (reference, -300_000)):
            channel.configure(fs, offset, 640, n)
        for block in blocks:
            actual = first.process(block)
            second.process(block)
            expected = reference.process(block)
            np.testing.assert_array_equal(actual[0], expected[0])
            np.testing.assert_array_equal(actual[1], expected[1])
        assert first.clock() == second.clock() == reference.clock() == (n * 10, 253)
    finally:
        first.close()
        second.close()
        reference.close()


def test_wide_capture_geometry_not_locked_to_25_mhz(library):
    channel = NativeDdcChannel(None, library)
    try:
        assert channel.quantum(62_500_000.0) == 64
        channel.configure(62_500_000.0, -300_000, 1280, 16240)
        assert channel.fs_out == 62_500_000.0 / 1280
        assert channel.delay > 0
        iq = np.zeros(16240 * 2, dtype=np.int16)
        channel.process(iq)
        assert channel.clock() == (16240, 12)
    finally:
        channel.close()


def test_rational_output_rate_preserves_clock_and_input_offset(library):
    channel = NativeDdcChannel(None, library)
    fs = 31_250_000.0
    try:
        channel.configure(fs, -200_000, 96, 16240,
                          ratio_up=8, ratio_down=9, passband_hz=90000.0)
        assert channel.fs_out == pytest.approx(fs / 108)
        assert channel.delay > 50
        iq = np.zeros(16240*2, np.int16)
        skip = channel._drop_pending
        i, _q = channel.process(iq)
        assert channel.clock()[0] == 16240
        assert i.size > 0
        target_coarse = (2 * skip * 9 + 95 * 8 + 2 * 8 - 1) // (2 * 8)
        assert channel.output_input_offset == (target_coarse + 1) * 96 - 1
    finally:
        channel.close()


def test_supported_rate_preserves_wfm_band_and_rejects_near_alias(library):
    fs, n = 31_250_000.0, 16240
    channel = NativeDdcChannel(None, library)
    try:
        decimate = channel.select_decimate(fs, 108)
        assert decimate == 104
        fs_out = fs / decimate

        def run(tone):
            channel.configure(fs, -200_000, decimate, n)
            t = np.arange(n * 12) / fs
            z = 12000 * np.exp(2j * np.pi * (200_000 + tone) * t)
            iq = np.empty((n * 12, 2), np.int16)
            iq[:, 0] = np.rint(z.real).astype(np.int16)
            iq[:, 1] = np.rint(z.imag).astype(np.int16)
            parts = []
            for block in iq.reshape(12, n * 2):
                i, q = channel.process(block)
                parts.append(i + 1j*q)
            return np.concatenate(parts)[100:]

        wanted = run(1000)
        edge = run(90_000)
        alias = run(fs_out - 90_000)
        edge_db = 20 * np.log10(np.sqrt(np.mean(abs(edge)**2) / np.mean(abs(wanted)**2)))
        alias_db = 20 * np.log10(np.sqrt(np.mean(abs(alias)**2) / np.mean(abs(wanted)**2)))
        assert edge_db > -1
        assert alias_db < -60
    finally:
        channel.close()


def test_tone_phase_and_alias_rejection(library):
    fs, decimate, n, packets = 31_250_000.0, 640, 16240, 16
    t = np.arange(n * packets) / fs

    def receive(tone):
        channel = NativeDdcChannel(None, library)
        channel.configure(fs, -300_000, decimate, n)
        z = np.exp(2j * np.pi * (tone + 300_000) * t) * 12000
        iq = np.empty((n * packets, 2), np.int16)
        iq[:, 0] = np.rint(z.real).astype(np.int16)
        iq[:, 1] = np.rint(z.imag).astype(np.int16)
        parts = []
        for block in iq.reshape(packets, n * 2):
            i, q = channel.process(block)
            parts.append(i + 1j*q)
        channel.close()
        return np.concatenate(parts)[80:]

    wanted = receive(1000)
    ph = np.angle(wanted[1:] * wanted[:-1].conj())
    expected = 2 * np.pi * 1000 / (fs / decimate)
    assert np.max(np.abs(ph - expected)) < 0.002  # includes packet boundaries
    for tone in (fs / decimate + 1000, -fs / decimate + 1000):
        alias = receive(tone)
        attenuation = 20 * np.log10(np.sqrt(np.mean(abs(alias)**2) / np.mean(abs(wanted)**2)))
        assert attenuation < -90
    edge = receive(0.4 * fs / decimate)
    edge_db = 20 * np.log10(np.sqrt(np.mean(abs(edge)**2) / np.mean(abs(wanted)**2)))
    assert edge_db > -1
    folding = receive(0.6 * fs / decimate)
    fold_db = 20 * np.log10(np.sqrt(np.mean(abs(folding)**2) / np.mean(abs(wanted)**2)))
    assert fold_db < -60
