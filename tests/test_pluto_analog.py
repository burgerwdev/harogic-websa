"""The analog transmit helpers: audio loading, length fitting, level and modulation.

These run without a radio: the helpers and the baseband builder are pure numpy.
"""
from __future__ import annotations

import argparse
import sys
import wave
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.pluto import pluto_analog_tx as analog  # noqa: E402
from tools.pluto import rds_encode as rds  # noqa: E402


def _write_pcm24(path: Path, samples: np.ndarray, rate: int) -> None:
    ints = np.round(np.clip(samples, -1.0, 1.0) * (2 ** 23 - 1)).astype(np.int64)
    u = ints & 0xFFFFFF
    b = np.empty((ints.size, 3), dtype=np.uint8)
    b[:, 0] = u & 0xFF
    b[:, 1] = (u >> 8) & 0xFF
    b[:, 2] = (u >> 16) & 0xFF
    with wave.open(str(path), 'wb') as w:
        w.setnchannels(1)
        w.setsampwidth(3)
        w.setframerate(rate)
        w.writeframes(b.tobytes())


def _args(mode: str, **over) -> argparse.Namespace:
    base = dict(mode=mode, wav=None, channel='sum', duration=None, no_loop=False,
                normalize_peak=0.9, no_normalize=False, audio_gain_db=0.0,
                tone_hz=1000.0, am_depth=0.5, dsb_carrier=0.5, deviation_hz=3000.0,
                pm_index=1.0, base_hz=0.0, scale=1.0, rds_rt=None, rds_pi=0x1234,
                rds_pty=0, rds_ps='RDS TEST', rds_rt_cycle_seconds=None,
                rds_deviation_hz=2000.0, pilot_deviation_hz=0.0, rds_buffer=False,
                rds_tp=False, rds_ct=False, rds_ab_fixed=False,
                rds_ps_groups=rds.PS_GROUPS_PER_PASS)
    base.update(over)
    return argparse.Namespace(**base)


def test_fit_length_keeps_loops_pads_and_truncates():
    a = np.arange(4, dtype=np.float32)
    assert analog.fit_length(a, 10, None, True).size == 4          # no duration: whole file
    assert analog.fit_length(a, 1, 10.0, True).size == 10          # loop to the length
    assert analog.fit_length(a, 1, 10.0, False).size == 10         # pad when not looping
    assert analog.fit_length(np.arange(100, dtype=np.float32), 1, 10.0, True).size == 10


def test_apply_level_normalises_the_peak():
    a = np.array([0.1, -0.2], dtype=np.float32)
    out = analog.apply_level(a, 0.9, 0.0, True)
    assert abs(float(np.max(np.abs(out))) - 0.9) < 1e-6
    assert np.array_equal(analog.apply_level(a, 0.9, 0.0, False), a)


def test_load_audio_reads_24_bit_and_picks_the_channel(tmp_path):
    rate = 16000
    t = np.arange(rate) / rate
    tone = 0.5 * np.cos(2 * np.pi * 440 * t)
    path = tmp_path / 't24.wav'
    _write_pcm24(path, tone, rate)
    audio = analog.load_audio(str(path), 48000.0)
    assert audio.size == 48000
    assert 0.49 < float(np.max(np.abs(audio))) < 0.52


def test_nfm_deviation_matches_the_setting():
    rate = 100000.0
    sig = analog.build_baseband(_args('nfm', deviation_hz=3000.0, duration=0.05), rate)
    phase = np.unwrap(np.angle(sig))
    inst = np.diff(phase) / (2 * np.pi) * rate
    mid = inst[inst.size // 4:-inst.size // 4]
    assert abs(float(np.max(mid)) - 3000.0) < 5.0


def test_usb_puts_the_tone_above_the_dial():
    rate = 100000.0
    sig = analog.build_baseband(_args('usb', tone_hz=1000.0, duration=0.05), rate)
    n = sig.size
    spectrum = np.abs(np.fft.fftshift(np.fft.fft(sig))) ** 2
    freqs = np.fft.fftshift(np.fft.fftfreq(n, 1.0 / rate))
    assert abs(float(freqs[np.argmax(spectrum)]) - 1000.0) < 5.0
    assert spectrum[freqs < -50].sum() < spectrum[freqs > 50].sum() * 1e-3


# --- RDS RadioText on the wfm composite -----------------------------------------
# The receiver below is a standard RDS receiver written from the specification: it
# band-limits the 57 kHz channel, down-converts it in quadrature, matched-filters
# with the standard's data-shaping pulse H_T(f) = cos(pi f td/4), decides each
# symbol by its sign under a decision-directed carrier phase, undoes the
# differential coding and then checks the block offset words. It proves a standard
# receiver recovers the RadioText from the transmitted buffer.
#
# It is *not* a test of the symbol's shape: an MF-and-sign receiver also locks
# onto a non-standard symbol, because it only needs the per-bit polarity. The
# shape is pinned by test_rds_symbol_is_the_inverse_transform_of_the_standard_filter
# (the symbol must be the IFT of the standard's transfer function), by the
# zero-mean/no-carrier tests below, and by the waveform being within 1e-6 of the
# published waveform_biphase table of PiFmRds, PiFmAdv and mpxgen.

RDS_SUBCARRIER_HZ = 57000.0
RDS_SYMBOL_RATE = 1187.5
RDS_BIT_HZ = RDS_SYMBOL_RATE
RDS_POLY = 0x5B9
RDS_OFFSETS = (0x0FC, 0x198, 0x168, 0x1B4)
RDS_WFM_DEVIATION_HZ = 75000.0


def _bits_to_int(bits: np.ndarray) -> int:
    value = 0
    for bit in bits:
        value = (value << 1) | int(bit)
    return value


def _remainder(value: int) -> int:
    """Remainder of a 26-bit block modulo the RDS generator polynomial."""
    for bit in range(25, 9, -1):
        if value & (1 << bit):
            value ^= RDS_POLY << (bit - 10)
    return value & 0x3FF


def _spec_pulse(tau: np.ndarray) -> np.ndarray:
    """The standard's data-shaping pulse h(tau), from H_T(f) = cos(pi f td/4).

    This is the receiver's matched filter. It is the IFT of the transfer function
    the standard names, band-limited to |f| <= 2/td, with a removable singularity
    at tau = +-1/8.
    """
    den = 0.0625 - 4.0 * tau ** 2
    near = np.abs(den) < 1e-9
    return np.where(near, 4.0 * np.pi, np.cos(4.0 * np.pi * tau) / np.where(near, 1.0, den))


def _rds_decisions(sig: np.ndarray, rate: float,
                   deviation_hz: float = RDS_WFM_DEVIATION_HZ) -> np.ndarray:
    """The transmitted bits a standard receiver recovers from an FM baseband.

    FM-demodulate, band-pass 57 kHz +- 2.4 kHz, down-convert in quadrature,
    matched-filter with the standard pulse, acquire the symbol timing and the
    carrier phase from the data itself (the reference decoders use a PLL for one
    and a phase average for the other), then take the sign of each symbol.
    """
    phase = np.unwrap(np.angle(sig))
    mpx = np.diff(phase, prepend=phase[0]) * rate / (2.0 * np.pi) / deviation_hz
    n = mpx.size
    freqs = np.fft.rfftfreq(n, 1.0 / rate)
    spectrum = np.fft.rfft(mpx)
    spectrum[(freqs < RDS_SUBCARRIER_HZ - 2400.0) | (freqs > RDS_SUBCARRIER_HZ + 2400.0)] = 0.0
    t = np.arange(n) / rate
    # quadrature down-conversion of the DSB subcarrier, then the matched filter
    complex_freqs = np.fft.fftfreq(n, 1.0 / rate)
    z = np.fft.irfft(spectrum, n=n) * np.exp(-2j * np.pi * RDS_SUBCARRIER_HZ * t)
    matched = np.cos(np.pi * complex_freqs / (4.0 * RDS_BIT_HZ))
    z = np.fft.ifft(np.fft.fft(z) * np.where(np.abs(complex_freqs) <= 2.0 * RDS_BIT_HZ,
                                             matched, 0.0))

    # symbol timing: the receiver knows the rate, not the phase, so search for it.
    # The search starts at the bit start: sampling early would slip a whole bit.
    best = None
    for delta in np.arange(0.0, 0.5, 0.01):
        index = np.round((np.arange(int(n * RDS_BIT_HZ / rate) - 4) + delta)
                         * rate / RDS_BIT_HZ).astype(np.int64)
        index = np.clip(index, 0, n - 1)
        strength = float(np.mean(np.abs(z[index])))
        if best is None or strength > best[0]:
            best = (strength, index)
    y = z[best[1]]
    assert y.size > 0, 'no symbol instants found'

    # carrier phase, decision-directed: the DSB phase is unknown to the receiver
    phi = 0.0
    for _ in range(4):
        decided = np.where((y * np.exp(-1j * phi)).real > 0, 1.0, -1.0)
        phi = np.angle(np.sum(decided * y))
    sent = ((y * np.exp(-1j * phi)).real > 0).astype(np.uint8)
    return np.concatenate((sent[:1], sent[1:] ^ sent[:-1]))  # undo the differential coding


def _rds_groups(sig: np.ndarray, rate: float, deviation_hz: float = RDS_WFM_DEVIATION_HZ,
                group_type: int = 2) -> list[tuple[int, list[int]]]:
    """The groups of one type in an FM baseband, as (first bit index, 4 words).

    Slide over the received bit decisions looking for four blocks whose offset
    words are A, B, C, D in turn; anything else is not a group boundary. The
    default type 2 is RadioText; 0 is the PS name.
    """
    data = _rds_decisions(sig, rate, deviation_hz)
    groups: list[tuple[int, list[int]]] = []
    position = 0
    while position + 4 * 26 <= data.size:
        blocks = []
        for i, offset in enumerate(RDS_OFFSETS):
            value = _bits_to_int(data[position + 26 * i:position + 26 * (i + 1)])
            if _remainder(value) != offset:
                break
            blocks.append(value >> 10)
        kind = blocks[1] >> 12 if len(blocks) == 4 else None
        if len(blocks) == 4 and kind == group_type and not (blocks[1] >> 11) & 1:
            groups.append((position, blocks))
            position += 4 * 26
        else:
            position += 1
    return groups


def _ps_name(groups: list[tuple[int, list[int]]]) -> str:
    """The PS name from decoded Group 0A: two characters per group in block D."""
    chars = {block_b & 0x03: chr(block_d >> 8) + chr(block_d & 0xFF)
             for _start, (_a, block_b, _c, block_d) in groups}
    return ''.join(chars[index] for index in sorted(chars)).strip()


def _rt_passes(bits: np.ndarray) -> tuple[list[tuple[int, str]], list[int]]:
    """The complete RadioText passes of a transmitted bit stream, and their A/B flags.

    The stream is read the receiver's way: undo the differential coding, verify the
    four offset words, then collect Group 2A by segment address. A pass is complete
    when all 16 addresses have arrived.
    """
    data = np.concatenate((bits[:1], bits[1:] ^ bits[:-1]))
    chars: dict[int, str] = {}
    passes: list[tuple[int, str]] = []
    flags: list[int] = []
    flag = 0
    position = 0
    while position + 4 * rds.GROUP_BITS <= data.size:
        words = [_bits_to_int(data[position + 26 * i:position + 26 * (i + 1)]) for i in range(4)]
        if [_remainder(word) for word in words] != list(RDS_OFFSETS):
            position += 1
            continue
        block_b, block_c, block_d = (word >> 10 for word in words[1:])
        if (block_b >> 12) == 2 and not (block_b >> 11) & 1:
            segment = block_b & 0x0F
            if segment == 0 and chars:
                _close_pass(chars, flag, passes, flags)
                chars = {}
            if not chars:
                flag = (block_b >> 4) & 1
            chars[segment] = (chr(block_c >> 8) + chr(block_c & 0xFF)
                              + chr(block_d >> 8) + chr(block_d & 0xFF))
        position += rds.GROUP_BITS
    if chars:
        _close_pass(chars, flag, passes, flags)
    return passes, flags


def _close_pass(chars: dict[int, str], flag: int, passes: list[tuple[int, str]],
                flags: list[int]) -> None:
    """Add a pass to the list when all 16 of its segments arrived."""
    if len(chars) != rds.RT_SEGMENTS:
        return
    text = ''.join(chars[index] for index in range(rds.RT_SEGMENTS)).split('\r', 1)[0]
    passes.append((len(passes), text.rstrip(' ')))
    flags.append(flag)


def _radiotext(groups: list[tuple[int, list[int]]]) -> tuple[int | None, int | None, list[str]]:
    """The PI, the PTY and every complete RadioText pass of a decoded stream.

    A pass is one sweep of the 16 segment addresses. The stream may start and end
    inside a pass, and a rotation repeats the messages, so the caller sees the
    passes in stream order and decides what to make of them.
    """
    passes: list[list[int]] = []
    counts: list[int] = []
    current = [0x0D] * rds.RT_CHARS
    seen = 0
    for _start, (block_a, block_b, block_c, block_d) in groups:
        if not seen and not passes:
            pi, pty = block_a, (block_b >> 5) & 0x1F
        segment = block_b & 0x0F
        if segment == 0 and seen:
            passes.append(current)
            counts.append(seen)
            current = [0x0D] * rds.RT_CHARS
            seen = 0
        current[4 * segment:4 * segment + 4] = [block_c >> 8, block_c & 0xFF,
                                                block_d >> 8, block_d & 0xFF]
        seen += 1
    if seen:
        passes.append(current)
        counts.append(seen)
    complete = [''.join(chr(code) for code in text).split('\r', 1)[0]
                for text, count in zip(passes, counts, strict=True) if count == rds.RT_SEGMENTS]
    if not groups:
        return None, None, []
    return pi, pty, complete


def test_rds_checkwords_satisfy_the_specification():
    """Every block must divide to its offset word, and the offsets are the standard ones."""
    assert rds.OFFSET_WORDS == {'A': 0x0FC, 'B': 0x198, 'C': 0x168, "C'": 0x350, 'D': 0x1B4}
    for info in (0x0000, 0x1234, 0xABCD, 0xFFFF, 0x0001):
        for offset in rds.OFFSET_WORDS.values():
            assert _remainder(_bits_to_int(rds.block(info, offset))) == offset


def test_rds_symbol_is_the_standard_biphase_doublet():
    """EN 50067 1.7: a lobe at the bit start, the opposite one mid-bit, no mean.

    The mean matters: a symbol with a mean radiates a carrier at 57 kHz instead of a
    suppressed subcarrier.
    """
    tau = np.linspace(-1.25, 1.75, 200001)
    sym = rds.symbol(tau)
    assert abs(float(np.abs(sym).max()) - 1.0) < 1e-9      # normalized to +-1
    assert float(rds.symbol(np.array([0.0]))[0]) > 0.99    # positive lobe at the bit start
    assert float(rds.symbol(np.array([0.5]))[0]) < -0.99   # negative lobe at the bit middle
    assert abs(float(rds.symbol(np.array([0.25]))[0])) < 1e-9   # crosses zero mid-bit
    assert abs(float(sym.mean())) < 1e-3                   # no DC: suppressed carrier
    assert np.all(rds.symbol(np.array([-1.26, 1.76])) == 0.0)   # the standard's 3-bit window


def test_rds_symbol_is_the_inverse_transform_of_the_standard_filter():
    """The symbol must be the standard shaping applied to the impulse pair.

    EN 50067 1.7: a bit is e(t) = delta(t) - delta(t - td/2), shaped by
    H_T(f) = cos(pi f td/4) below 2/td. The module uses the closed form of the filter
    response. This test integrates the transfer function instead, so any other symbol
    fails, the single half-cosine lobe per bit included.
    """
    integrate = getattr(np, 'trapezoid', None) or np.trapz
    freqs = np.linspace(-2.0*RDS_BIT_HZ, 2.0*RDS_BIT_HZ, 400001)
    spectrum = np.cos(np.pi*freqs/(4.0*RDS_BIT_HZ))

    def h(seconds: float) -> float:
        return float(integrate(spectrum*np.cos(2.0*np.pi*freqs*seconds), freqs))

    tau = np.array([0.05, 0.3, 0.6, 0.9])
    expected = np.array([h(t/RDS_BIT_HZ) - h((t - 0.5)/RDS_BIT_HZ) for t in tau])
    got = rds.symbol(tau)
    assert np.allclose(got/got[0], expected/expected[0], rtol=2e-3)


def test_rds_all_zero_message_has_no_carrier_and_a_bit_rate_sine():
    """The calibration case of the standard, which a single-lobe symbol fails.

    With no data the baseband has no zero-frequency component, so the composite has a
    null at 57 kHz. Its spectrum peaks at the 1187.5 Hz bit rate.
    """
    rate = 521000.0
    # a whole number of bit periods, so only the window edges disturb the mean
    n = int(round(1000 * rate / RDS_BIT_HZ))
    t = np.arange(n) / rate
    base = rds.baseband(t, np.zeros(1004, dtype=np.uint8))
    # 1.7e-4 is the window-edge residual; the single-lobe symbol gives 0.64 here
    assert abs(float(base.mean())) < 2e-3 * float(np.abs(base).max())
    spectrum = np.abs(np.fft.rfft(base * np.hanning(base.size)))
    freqs = np.fft.rfftfreq(base.size, 1.0 / rate)
    assert abs(float(freqs[np.argmax(spectrum)]) - RDS_BIT_HZ) < 60.0


def test_rds_composite_suppresses_the_subcarrier():
    """57 kHz sits in a null of the data spectrum: no tone is radiated there."""
    rate = 521000.0
    t = np.arange(521000) / rate
    stream = rds.StationStream(0x1234, 0, 'RDS TEST', ['CQ DE N0CALL'])
    wave = rds.station_wave(t, stream, level=1.0)
    spectrum = np.abs(np.fft.rfft(wave * np.hanning(wave.size)))
    freqs = np.fft.rfftfreq(wave.size, 1.0 / rate)

    def at(frequency: float) -> float:
        return float(spectrum[int(np.argmin(np.abs(freqs - frequency)))])

    hump = max(at(RDS_SUBCARRIER_HZ - RDS_BIT_HZ), at(RDS_SUBCARRIER_HZ + RDS_BIT_HZ))
    assert at(RDS_SUBCARRIER_HZ) < 0.1 * hump


def test_rds_self_decodes_through_the_fm_modulator():
    """End to end: build the composite, FM-demodulate it, read the PS and the text back.

    Every character of the 64 differs from its neighbours, so a missed segment cannot
    hide behind the padding.
    """
    rate = 521000.0
    text = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-'
    assert len(text) == rds.RT_CHARS
    sig = analog.build_baseband(_args('wfm', duration=4.0, rds_rt=[text],
                                      rds_ps='BENCH 1', rds_pi=0x4321, rds_pty=10,
                                      deviation_hz=RDS_WFM_DEVIATION_HZ,
                                      pilot_deviation_hz=6750.0), rate)
    assert _ps_name(_rds_groups(sig, rate, group_type=0)) == 'BENCH 1'
    pi, pty, passes = _radiotext(_rds_groups(sig, rate))
    assert (pi, pty) == (0x4321, 10)
    assert set(passes) == {text}          # the same message, held over its two passes


def test_rds_rotation_holds_each_message_for_two_passes():
    """The station cadence on the bit stream: hold, rotation and the A/B flag.

    A receiver compares a segment with the previous pass, and it needs the A/B flag to
    move when the message does. A message therefore holds for whole passes, never one.
    """
    texts = ['HELLO WORLD', '123']
    stream = rds.StationStream(0x1234, 10, 'RDS TEST', texts, ab_toggle=True)
    hold = int(round(rds.hold_seconds(None, 'RDS TEST') / rds.pass_seconds('RDS TEST')))
    assert hold >= rds.DEFAULT_HOLD_PASSES          # never one pass
    assert rds.hold_seconds(None, 'RDS TEST') >= rds.DEFAULT_HOLD_S    # the station cadence

    groups = (2 * hold + 1) * rds.pass_groups('RDS TEST')   # two holds, plus the closing group
    bits, _first = stream.transmitted(0, groups * rds.GROUP_BITS - 1)

    passes, flags = _rt_passes(bits)
    held = [message for _pass, message in passes]
    assert held[:2 * hold] == [texts[0]] * hold + [texts[1]] * hold
    assert flags[0] != flags[hold]                # the flag moves with the message
    assert all(flags[index] == flags[0] for index in range(hold))


def test_rds_long_radiotext_is_truncated_to_64_characters():
    stream = rds.StationStream(0x1234, 10, 'RDS TEST', ['x' * 100])
    groups = 3 * rds.pass_groups('RDS TEST')   # two complete passes; the third closes the second
    bits, _first = stream.transmitted(0, groups * rds.GROUP_BITS - 1)
    passes, _flags = _rt_passes(bits)
    assert len(passes) >= 2
    assert {message for _pass, message in passes} == {'x' * rds.RT_CHARS}


def test_rds_pilot_is_the_19khz_reference():
    """EN 50067 1.6: the pilot is a 19 kHz sine, and RDS is 90 degrees from it.

    A receiver multiplies the pilot by three to get its 57 kHz clock, so the composite
    must carry it. The subcarrier is in phase with a cosine of 57 kHz, while the third
    harmonic of the pilot is a sine: that is the 90 degrees.
    """
    rate = 521000.0
    n = int(2.0 * rate)
    t = np.arange(n) / rate
    args = _args('wfm', duration=2.0, rds_rt=['CQ DE N0CALL'],
                 deviation_hz=RDS_WFM_DEVIATION_HZ, pilot_deviation_hz=6750.0)
    mpx = analog.broadcast_composite(np.zeros(n, dtype=np.float32), t, args,
                                     RDS_WFM_DEVIATION_HZ)

    sine = 2.0 / n * float(np.dot(mpx, np.sin(2.0*np.pi*rds.PILOT_HZ*t)))
    cosine = 2.0 / n * float(np.dot(mpx, np.cos(2.0*np.pi*rds.PILOT_HZ*t)))
    assert 0.08 < sine < 0.10          # 9% of the deviation, as a sine
    assert abs(cosine) < 1e-3          # not a cosine

    freqs = np.fft.fftfreq(n, 1.0 / rate)
    down = mpx * np.exp(-2j*np.pi*rds.SUBCARRIER_HZ*t)
    baseband = np.fft.ifft(np.where(np.abs(freqs) < 3000.0, np.fft.fft(down), 0.0))
    assert float(np.mean(baseband.imag**2)) < 1e-6 * float(np.mean(baseband.real**2))

    # the pilot, the RDS subcarrier and the audio share one deviation budget
    tone = np.cos(2.0*np.pi*1000.0*t).astype(np.float32)
    assert float(np.max(np.abs(analog.broadcast_composite(tone, t, args,
                                                         RDS_WFM_DEVIATION_HZ)))) <= 1.0




def test_rds_station_stream_sends_the_group_cadence():
    """The station sequence: four Group 0A (PS) then one Group 2A, and the PS in block D."""
    stream = rds.StationStream(0x4321, 10, 'BENCH 1', ['HELLO WORLD'])
    bits, _first = stream.transmitted(0, 2 * rds.pass_groups('') * rds.GROUP_BITS - 1)
    data = np.concatenate((bits[:1], bits[1:] ^ bits[:-1]))

    kinds = []
    block_cs = set()
    position = 0
    while position + 4 * rds.GROUP_BITS <= data.size:
        words = [_bits_to_int(data[position + 26 * i:position + 26 * (i + 1)]) for i in range(4)]
        if [_remainder(word) for word in words] != list(RDS_OFFSETS):
            position += 1
            continue
        block_b, block_c = words[1] >> 10, words[2] >> 10
        kinds.append(block_b >> 12)
        if (block_b >> 12) == 0:
            block_cs.add(block_c)
        position += rds.GROUP_BITS

    assert kinds[:16] == [2] * rds.RT_SEGMENTS      # the text arrives as one burst
    assert kinds[16:20] == [0] * rds.PS_GROUPS_PER_PASS
    assert block_cs == {0xCDCD}          # no alternative frequencies, the station convention


def test_rds_streaming_chunks_join_without_a_seam():
    """The live path pushes chunks; joined they must equal one long build.

    The group sequence continues and the FM carrier phase carries over, so a receiver
    stays locked across the joins.
    """
    rate = 521000.0
    args = _args('wfm', duration=1.0, rds_rt=['HELLO WORLD'], deviation_hz=RDS_WFM_DEVIATION_HZ,
                 pilot_deviation_hz=6750.0, scale=16384.0)
    samples = int(round(rate * 1.0))

    single, _phase = analog.broadcast_chunk(args, rate, 0, 3 * samples,
                                            rds.StationStream(args.rds_pi, args.rds_pty,
                                                              args.rds_ps, args.rds_rt), 0.0)
    chunks = []
    stream = rds.StationStream(args.rds_pi, args.rds_pty, args.rds_ps, args.rds_rt)
    phase = 0.0
    for start in (0, samples, 2 * samples):
        chunk, phase = analog.broadcast_chunk(args, rate, start, samples, stream, phase)
        chunks.append(chunk)
    joined = np.concatenate(chunks)
    # exact in the signal; the two paths differ only by how the carrier phase is
    # carried (reduced to 2*pi per chunk), which is float rounding, not a seam
    assert np.max(np.abs(single - joined)) < 1e-6 * float(np.abs(single).max())


def test_rds_is_refused_outside_wfm():
    with pytest.raises(SystemExit, match='needs wfm'):
        analog.build_baseband(_args('nfm', rds_rt=['CQ'], deviation_hz=3000.0), 100000.0)
    with pytest.raises(SystemExit, match='needs wfm'):
        analog.build_baseband(_args('nfm', pilot_deviation_hz=1000.0), 100000.0)


def test_wfm_without_rds_stays_plain_fm():
    """The RDS path must not touch a wfm transmission that does not ask for it.

    The reference repeats the plain chain bit for bit: the tone, the float32
    accumulator of the modulator and the DAC scale.
    """
    rate = 521000.0
    args = _args('wfm', duration=0.05, deviation_hz=RDS_WFM_DEVIATION_HZ, scale=16384.0)
    sig = analog.build_baseband(args, rate)
    t = np.arange(sig.size) / rate
    audio = np.cos(2.0 * np.pi * 1000.0 * t).astype(np.float32)  # what tone_audio returns
    integral = np.cumsum(audio) / rate
    phase = 2.0 * np.pi * args.deviation_hz * integral
    base = np.exp(1j * phase).astype(np.complex64)
    assert np.array_equal(sig, (base * args.scale).astype(np.complex64))


def test_rds_cli_plan_lists_the_payload(capsys):
    assert analog.main(['wfm', '--rds-rt', 'one, two', '--rds-ps', 'BENCH 1',
                        '--rds-pi', '0x4321', '--rds-pty', '10', '--dry-run']) == 0
    out = capsys.readouterr().out
    assert 'PI 4321' in out and 'RadioText "one" + 1 more' in out
    assert '57.000 kHz' in out and '90 deg to the pilot third harmonic' in out
    assert '19.000 kHz' in out and 'ps             : "BENCH 1"' in out
    assert '16 x 2A + 4 x 0A' in out and 'A/B flag moves with each message' in out


def test_rds_live_chunks_decode_across_the_joins():
    """The path the tool sends: one-second chunks, decoded end to end.

    This builds the live transmission the way transmit_broadcast does, and reads the
    PS and the RadioText from it.
    """
    rate = 521000.0
    text = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-'
    args = _args('wfm', duration=1.0, rds_rt=[text], rds_ps='BENCH 1', rds_pi=0x4321,
                 rds_pty=10, deviation_hz=RDS_WFM_DEVIATION_HZ,
                 pilot_deviation_hz=6750.0, scale=16384.0)
    samples = int(round(rate * 1.0))
    stream = rds.StationStream(args.rds_pi, args.rds_pty, args.rds_ps, args.rds_rt)
    chunks, phase = [], 0.0
    for start in range(0, int(round(4 * rate)), samples):
        chunk, phase = analog.broadcast_chunk(args, rate, start, samples, stream, phase)
        chunks.append(chunk)
    sig = np.concatenate(chunks)

    assert _ps_name(_rds_groups(sig, rate, group_type=0)) == 'BENCH 1'
    pi, pty, passes = _radiotext(_rds_groups(sig, rate))
    assert (pi, pty) == (0x4321, 10)
    assert set(passes) == {text}


def test_rds_without_a_ps_name_sends_radiotext_groups_only():
    """`--rds-ps ''` drops the Group 0A cadence: RadioText alone, a 1.4 s pass."""
    assert rds.group_pattern('') == (2,) * rds.RT_SEGMENTS
    assert rds.pass_groups('') == rds.RT_SEGMENTS
    assert abs(rds.pass_seconds('') - rds.RT_SEGMENTS * rds.GROUP_S) < 1e-12

    stream = rds.StationStream(0x1234, 10, '', ['HELLO WORLD'])
    bits, _first = stream.transmitted(0, 2 * rds.pass_groups('') * rds.GROUP_BITS - 1)
    data = np.concatenate((bits[:1], bits[1:] ^ bits[:-1]))
    kinds, position = [], 0
    while position + 4 * rds.GROUP_BITS <= data.size:
        words = [_bits_to_int(data[position + 26 * i:position + 26 * (i + 1)]) for i in range(4)]
        if [_remainder(word) for word in words] != list(RDS_OFFSETS):
            position += 1
            continue
        kinds.append(words[1] >> 22)
        position += rds.GROUP_BITS
    assert set(kinds) == {2}


def test_rds_composite_uses_the_given_deviation_budget():
    """The budget belongs to the transmission, not to one chunk.

    The live path passes the plan peak into every chunk. A peak per chunk would step
    the deviation, so the same composite with a doubled budget must come out at half.
    """
    rate = 521000.0
    samples = int(rate)
    t = np.arange(samples) / rate
    args = _args('wfm', duration=1.0, rds_rt=['HELLO WORLD'], deviation_hz=RDS_WFM_DEVIATION_HZ,
                 pilot_deviation_hz=6750.0)
    audio = np.cos(2.0 * np.pi * 1000.0 * t).astype(np.float32)
    plain = analog.broadcast_composite(audio, t, args, RDS_WFM_DEVIATION_HZ, None, 1.0)
    halved = analog.broadcast_composite(audio, t, args, RDS_WFM_DEVIATION_HZ, None, 2.0)
    assert np.allclose(plain / 2.0, halved)


def test_rds_radiotext_is_space_padded_to_64_characters():
    """The whole 64-character field is written, or a radio keeps the old tail.

    A receiver replaces the characters it receives. CR padding leaves the tail of the
    previous message in place, so the field is padded with spaces, as the deployed
    encoders do.
    """
    codes = rds.text_codes('123')
    assert codes[:3] == [ord('1'), ord('2'), ord('3')]
    assert set(codes[3:]) == {0x20}
    assert len(rds.text_codes('x' * 100)) == rds.RT_CHARS
    assert 0x0D in rds.text_codes('two\rlines')


def test_rds_streaming_needs_no_buffer_at_all():
    """The default path encodes as it goes, so the hold costs no memory.

    A buffer (--rds-buffer) must fit the device: this unit takes about 28 s of
    baseband, so one message has to fit one. A longer rotation is the reason to stream.
    """
    assert rds.pass_seconds('RDS TEST') < 2.0
    one = rds.hold_seconds(None, 'RDS TEST')
    assert one * 521000 * 4 / 1e6 < 30               # one message still fits a buffer
    assert one >= rds.DEFAULT_HOLD_S                 # the station cadence, not a pass
    assert rds.hold_seconds(20.0, 'RDS TEST') > one  # a longer hold is free when streamed


def test_rds_without_audio_is_the_rds_test_signal():
    """`--no-audio` leaves the pilot and RDS alone, so RDS takes 23% of the modulation
    instead of 2%.

    A tone at full deviation spends the modulation on an empty 1 kHz. The subcarrier
    keeps its level, but far more of the signal is data, which is the difference
    between a text that completes and one that shows a few segments.
    """
    rate = 521000.0
    n = int(rate)
    t = np.arange(n) / rate
    tone = np.cos(2.0 * np.pi * 1000.0 * t).astype(np.float32)
    args = _args('wfm', duration=1.0, rds_rt=['HELLO WORLD'], deviation_hz=RDS_WFM_DEVIATION_HZ,
                 pilot_deviation_hz=6750.0, no_audio=True)
    quiet = analog.broadcast_composite(tone, t, args, RDS_WFM_DEVIATION_HZ)
    args.no_audio = False
    loud = analog.broadcast_composite(tone, t, args, RDS_WFM_DEVIATION_HZ)

    def tone_level(x: np.ndarray) -> float:
        return 2.0 / n * float(np.dot(x, np.cos(2.0 * np.pi * 1000.0 * t)))

    # no audio at all: the 1e-5 is the pilot and RDS leaking into the 1 kHz bin
    assert abs(tone_level(quiet)) < 1e-5
    assert tone_level(loud) > 0.5                # the tone, at its usual level
    assert float(np.max(np.abs(quiet))) < 0.2    # pilot and RDS only


def test_rds_clock_group_carries_the_time_once_a_minute():
    """Group 4A: the date as a modified Julian day, the time and the local offset.

    A radio sets its clock from this group. A real encoder gives it priority, so the
    first PS slot of the first pass of a minute is the clock.
    """
    start = datetime(2026, 1, 2, 3, 4, 0, tzinfo=timezone.utc)
    clock = rds.Clock(start, rds.pass_seconds('RDS TEST'), offset_half_hours=16)
    stream = rds.StationStream(0x1234, 10, 'BENCH 1', ['HELLO WORLD'], clock=clock)
    groups = stream.transmitted(0, (2 * rds.pass_groups('RDS TEST') + 1) * rds.GROUP_BITS - 1)[0]
    data = np.concatenate((groups[:1], groups[1:] ^ groups[:-1]))

    found, position, kinds = [], 0, []
    while position + 4 * rds.GROUP_BITS <= data.size:
        words = [_bits_to_int(data[position + 26 * i:position + 26 * (i + 1)]) for i in range(4)]
        if [_remainder(word) for word in words] != list(RDS_OFFSETS):
            position += 1
            continue
        block_b, block_c, block_d = (word >> 10 for word in words[1:])
        kinds.append(block_b >> 12)
        if (block_b >> 12) == 4:
            found.append((block_b, block_c, block_d))
        position += rds.GROUP_BITS

    assert kinds[16] == 4                      # the first PS slot of the first pass
    assert kinds[17:20] == [0, 0, 0]           # the rest of the PS name
    assert 4 not in kinds[20:]                 # and not again inside the same minute
    block_b, block_c, block_d = found[0]
    mjd = ((block_b & 0x03) << 15) | (block_c >> 1)   # MJD bits 16,15 sit in block B
    assert mjd == 61042                        # 2026-01-02, the standard's day number
    assert ((block_c & 0x01) << 4) | (block_d >> 12) == 3      # hour, UTC
    assert (block_d >> 6) & 0x3F == 4                          # minute
    assert block_d & 0x1F == 16 and not block_d & 0x20         # +8 h, as half hours


def test_rds_tp_flag_is_set_by_default():
    """TP is in block B of every group; the reference encoders send it set."""
    stream = rds.StationStream(0x1234, 10, 'BENCH 1', ['HELLO WORLD'], tp=True)
    bits = stream.transmitted(0, 2 * rds.pass_groups('RDS TEST') * rds.GROUP_BITS)[0]
    data = np.concatenate((bits[:1], bits[1:] ^ bits[:-1]))
    tps, position = [], 0
    while position + 4 * rds.GROUP_BITS <= data.size:
        words = [_bits_to_int(data[position + 26 * i:position + 26 * (i + 1)]) for i in range(4)]
        if [_remainder(word) for word in words] != list(RDS_OFFSETS):
            position += 1
            continue
        block_b = words[1] >> 10
        tps.append((block_b >> 10) & 1)          # TP, bit 10 of block B
        position += rds.GROUP_BITS
    assert tps and set(tps) == {1}


def test_rds_ps_names_rotate_on_the_same_hold_as_the_texts():
    """Two PS names take turns, so a receiver has to re-render its station name.

    A real station keeps its PS fixed. Rotation is a bench trick, and it tests the
    refresh of an eight-character field, the field a RadioText display is judged on.
    """
    names = ['BENCH 1', 'CLASSIC']
    stream = rds.StationStream(0x1234, 10, names, ['HELLO WORLD'])
    hold = int(round(rds.hold_seconds(None, names) / rds.pass_seconds(names)))
    groups = (2 * hold + 1) * rds.pass_groups(names)
    bits = stream.transmitted(0, groups * rds.GROUP_BITS - 1)[0]
    data = np.concatenate((bits[:1], bits[1:] ^ bits[:-1]))

    passes: list[dict[int, str]] = []
    position = 0
    while position + 4 * rds.GROUP_BITS <= data.size:
        words = [_bits_to_int(data[position + 26 * i:position + 26 * (i + 1)]) for i in range(4)]
        if [_remainder(word) for word in words] != list(RDS_OFFSETS):
            position += 1
            continue
        block_b, block_d = words[1] >> 10, words[3] >> 10
        if (block_b >> 12) == 0:
            index = position // rds.GROUP_BITS // rds.pass_groups(names)
            while len(passes) <= index:
                passes.append({})
            passes[index][block_b & 0x03] = chr(block_d >> 8) + chr(block_d & 0xFF)
        position += rds.GROUP_BITS

    complete = [''.join(pair[index] for index in sorted(pair)).strip()
                for pair in passes if len(pair) == 4]
    assert complete[:hold] == [names[0]] * hold
    assert complete[hold:2 * hold] == [names[1]] * hold


def test_rds_without_a_ps_name_sends_no_group_0a():
    """An empty `--rds-ps` is a request for RadioText only, not a mistake."""
    assert rds.group_pattern('') == (2,) * rds.RT_SEGMENTS
    assert rds.group_pattern('', 0) == (2,) * rds.RT_SEGMENTS
    assert rds.pass_seconds('', 0) < 1.5


def test_rds_fewer_ps_groups_per_pass_still_complete_the_name():
    """The PS segment address runs on across passes, so the name still arrives.

    Four groups fill the name in one pass. Two groups per pass take two passes, which
    is the shorter pass a transmitter can send when a receiver mishandles one of the
    four PS slots.
    """
    names = ['BENCH 1']
    stream = rds.StationStream(0x1234, 10, names, ['HELLO WORLD'], ps_groups=2)
    assert rds.pass_groups(names, 2) == rds.RT_SEGMENTS + 2
    groups = 4 * rds.pass_groups(names, 2)
    bits = stream.transmitted(0, groups * rds.GROUP_BITS - 1)[0]
    data = np.concatenate((bits[:1], bits[1:] ^ bits[:-1]))

    seen: dict[int, str] = {}
    position, order = 0, []
    while position + 4 * rds.GROUP_BITS <= data.size:
        words = [_bits_to_int(data[position + 26 * i:position + 26 * (i + 1)]) for i in range(4)]
        if [_remainder(word) for word in words] != list(RDS_OFFSETS):
            position += 1
            continue
        block_b, block_d = words[1] >> 10, words[3] >> 10
        if (block_b >> 12) == 0:
            segment = block_b & 0x03
            seen[segment] = chr(block_d >> 8) + chr(block_d & 0xFF)
            order.append(segment)
        position += rds.GROUP_BITS

    assert set(seen) == {0, 1, 2, 3}                  # every segment goes out
    assert ''.join(seen[index] for index in sorted(seen)).strip() == 'BENCH 1'
    assert order[:4] == [0, 1, 2, 3]                  # in order, two per pass
