#!/usr/bin/env python3
"""RDS encoder for the Pluto WFM transmitter.

RDS (Radio Data System, EN 50067) is the data service of a WFM broadcast: a
1187.5 bps biphase BPSK stream on a suppressed 57 kHz subcarrier in the FM
composite. This module builds the group sequence of a station, and returns it as
composite samples at the TX rate. `pluto_analog_tx.py` adds it to the audio.

Implemented: Group 0A (the 8-character PS name), Group 2A (64 RadioText
characters), Group 4A (clock time), PI, PTY, TP, the checkwords, the offset words
and the differential coding. Not implemented: 3A and 8A Open Data, 14A EON,
alternative frequencies and the TA flags.

The subcarrier follows EN 50067 section 1.7: each bit is the impulse pair
delta(t) - delta(t - td/2), shaped by H_T(f) = cos(pi f td/4) below 2/td. The
symbol has no mean, so the subcarrier stays suppressed, and its band is
57 kHz +- 2.4 kHz. The symbol matches the published PiFmRds and mpxgen table to
2e-6. The tests pin the symbol shape, the carrier null and the group layout.

References: EN 50067:1998 sections 1.6, 1.7, 2.1, 2.2, 3.1.5, 3.1.6, Annex E.
"""
from __future__ import annotations

from datetime import datetime, timedelta

import numpy as np

#: The RDS subcarrier in the FM composite.
SUBCARRIER_HZ = 57_000.0
#: The 19 kHz pilot: the reference of the subcarrier, and its third.
PILOT_HZ = 19_000.0
#: The pilot's conventional injection, 9% of the total deviation.
DEFAULT_PILOT_FRACTION = 0.09
#: Data rate: the subcarrier divided by 48.
SYMBOL_RATE = 1187.5
#: One group: 4 blocks of 26 bits.
GROUP_BITS = 104
#: Group duration, 87.58 ms.
GROUP_S = GROUP_BITS / SYMBOL_RATE
#: Group 2A carries 16 segments of 4 RadioText characters.
RT_SEGMENTS = 16
RT_CHARS = RT_SEGMENTS * 4
#: Group 0A carries the 8-character PS name, two characters per group.
PS_CHARS = 8
#: A pass is the RadioText burst, then this many PS groups.
PS_GROUPS_PER_PASS = 4
GROUP_PS = 0
GROUP_RT = 2
#: The shortest hold: two passes, the repetition a receiver uses for error correction.
DEFAULT_HOLD_PASSES = 2
#: The usual hold, about the time a station keeps one message. The operator can
#: shorten it to demonstrate a rotation.
DEFAULT_HOLD_S = 10.0
#: An AF code pair of 0xCD 0xCD means "no alternative frequencies".
NO_AF_CODE = 0xCD
#: Group 4A carries the clock time and date, which a radio uses for its own clock.
GROUP_CT = 4
#: Checkword generator polynomial x^10+x^8+x^7+x^5+x^4+x^3+1 (EN 50067 section 2.1).
CHECK_POLY = 0x5B9
#: Block offset words, added to the checkword: A, B, C, (C' for version B), D.
OFFSET_WORDS = {'A': 0x0FC, 'B': 0x198, 'C': 0x168, "C'": 0x350, 'D': 0x1B4}
#: The group type code of RadioText in block B.
GROUP_2A = 2
#: Conventional RDS injection level: about 2 kHz of the 75 kHz WFM deviation.
DEFAULT_DEVIATION_HZ = 2000.0
#: The symbol is three bit periods wide, centred on its impulse pair.
BIPHASE_WINDOW = 1.5


def _symbol_peak() -> float:
    """The symbol peak, to normalize a symbol to +-1."""
    grid = np.linspace(0.25 - BIPHASE_WINDOW, 0.25 + BIPHASE_WINDOW, 60001)
    return float(np.abs(_pulse(grid) - _pulse(grid - 0.5)).max())


def checkword(info: int, offset: int) -> int:
    """Return the 10 check bits of one 16-bit information word.

    Divide the word and ten zero bits by the generator polynomial, then add the
    offset word of the block. A receiver checks the same property.
    """
    code = (info & 0xFFFF) << 10
    for bit in range(25, 9, -1):
        if code & (1 << bit):
            code ^= CHECK_POLY << (bit - 10)
    return (code & 0x3FF) ^ offset


def block(info: int, offset: int) -> np.ndarray:
    """One 26-bit block as MSB-first bits: 16 information + 10 check bits."""
    value = ((info & 0xFFFF) << 10) | checkword(info, offset)
    return np.array([(value >> (25 - i)) & 1 for i in range(26)], dtype=np.uint8)


def text_codes(text: str, length: int = RT_CHARS) -> list[int]:
    """The RadioText characters as RDS codes, truncated and padded to `length`.

    The G0 set matches ASCII in the printable range. Any other character becomes a
    space, and a CR in the text ends the message. Unused characters are spaces, the
    convention of the deployed PiFmRds encoder: a radio replaces the characters it
    receives, and a CR does not replace one, so space padding writes the whole
    field and leaves no tail of the previous message.
    """
    codes = [(ord(c) if 0x20 <= ord(c) <= 0x7E or c == '\r' else 0x20)
             for c in text[:length]]
    return codes + [0x20] * (length - len(codes))


def group_2a(pi: int, pty: int, segment: int, codes: list[int], text_flag: int = 0,
             tp: bool = True) -> np.ndarray:
    """One Group 2A message as 104 MSB-first bits.

    Block A carries the PI code. Block B carries the group type, version, TP, PTY,
    the text A/B flag and the segment address. Blocks C and D carry two characters
    each.
    """
    block_b = ((GROUP_2A << 12) | (0 << 11) | (int(tp) << 10) | ((pty & 0x1F) << 5)
               | ((text_flag & 1) << 4) | (segment & 0x0F))
    block_c = (codes[0] << 8) | codes[1]
    block_d = (codes[2] << 8) | codes[3]
    return np.concatenate([block(pi, OFFSET_WORDS['A']), block(block_b, OFFSET_WORDS['B']),
                           block(block_c, OFFSET_WORDS['C']), block(block_d, OFFSET_WORDS['D'])])


def group_4a(pi: int, pty: int, when: datetime, tp: bool = True, offset_half_hours: int = 0
             ) -> np.ndarray:
    """One Group 4A message: the UTC clock time and date, as a station sends it.

    The date is the modified Julian day. Then follow the hour, the minute and the
    local offset in half hours, with bit 5 as the sign. A radio sets its clock from
    this group. Layout of PiFmRds and NanoRDS; EN 50067 section 3.2.4.
    """
    # The day number of EN 50067 Annex G: 14956, the day, the days since 1900 and a
    # month term. January and February count as the previous year.
    leap = 1 if when.month <= 2 else 0
    mjd = (14956 + when.day + int((when.year - 1900 - leap) * 365.25)
           + int((when.month + 1 + 12 * leap) * 30.6001))
    block_b = ((GROUP_CT << 12) | (0 << 11) | (int(tp) << 10) | ((pty & 0x1F) << 5)
               | ((mjd >> 15) & 0x1F))
    block_c = ((mjd << 1) & 0xFFFF) | ((when.hour >> 4) & 0x01)
    block_d = (((when.hour & 0x0F) << 12) | ((when.minute & 0x3F) << 6)
               | (abs(offset_half_hours) & 0x1F) | (0x20 if offset_half_hours < 0 else 0))
    return np.concatenate([block(pi, OFFSET_WORDS['A']), block(block_b, OFFSET_WORDS['B']),
                           block(block_c, OFFSET_WORDS['C']), block(block_d, OFFSET_WORDS['D'])])


def group_0a(pi: int, pty: int, segment: int, codes: list[int], tp: bool = True
             ) -> np.ndarray:
    """One Group 0A message: two PS name characters, in block D.

    Block B carries the group type, the version, TP, PTY and the two-bit segment
    address. Block C carries the AF codes, both 0xCD for "no alternative
    frequencies", as a single-frequency station fills them. Four groups spell the
    whole 8-character name.
    """
    block_b = ((GROUP_PS << 12) | (0 << 11) | (int(tp) << 10) | ((pty & 0x1F) << 5)
               | (segment & 0x03))
    block_c = (NO_AF_CODE << 8) | NO_AF_CODE
    block_d = (codes[0] << 8) | codes[1]
    return np.concatenate([block(pi, OFFSET_WORDS['A']), block(block_b, OFFSET_WORDS['B']),
                           block(block_c, OFFSET_WORDS['C']), block(block_d, OFFSET_WORDS['D'])])


def ps_names(ps: str | list[str]) -> list[str]:
    """The station name, or several names that rotate on the RadioText hold.

    A real station keeps its name fixed. A bench rotates names to see how a
    receiver refreshes an eight-character field.
    """
    return [ps] if isinstance(ps, str) else list(ps)


def ps_codes(name: str) -> list[int]:
    """The station name as PS_CHARS codes, space-padded, the broadcast convention."""
    codes = [(ord(c) if 0x20 <= ord(c) <= 0x7E else 0x20) for c in name[:PS_CHARS]]
    return codes + [0x20] * (PS_CHARS - len(codes))


def station_group(pi: int, pty: int, ps: str, texts: list[str], hold_passes: int,
                  group_index: int, tp: bool = True, ab_toggle: bool = False,
                  clock: Clock | None = None) -> np.ndarray:
    """One 104-bit group of the station sequence, by global group index.

    A pass is the 16 RadioText segments back to back, then the four PS groups. The
    burst lets a receiver collect the whole text in 1.4 s. A message stays up for
    `hold_passes` passes, because a receiver wants the repetition of a segment.

    `clock` puts the first PS group of every new minute in a Group 4A clock group,
    the priority the reference encoders give it. `ab_toggle` moves the Group 2A A/B
    flag with each message, as the standard asks. The deployed PiFmRds keeps the flag
    at 0; some receivers expect that instead.
    """
    pattern = group_pattern(ps)
    names = ps_names(ps)
    position = group_index % len(pattern)
    pass_index = group_index // len(pattern)
    if pattern[position] == GROUP_RT:
        text_index = (pass_index // hold_passes) % len(texts)
        codes = text_codes(texts[text_index])
        flag = (text_index % 2) if ab_toggle else 0
        return group_2a(pi, pty, position, codes[4 * position:4 * position + 4], flag, tp)
    slot = position - RT_SEGMENTS
    if slot == 0 and clock is not None and clock.is_new_minute(pass_index):
        return group_4a(pi, pty, clock.at(pass_index), tp, clock.offset_half_hours)
    ps_segment = slot
    name = names[(pass_index // hold_passes) % len(names)]
    return group_0a(pi, pty, ps_segment, ps_codes(name)[2 * ps_segment:2 * ps_segment + 2], tp)


class Clock:
    """The wall clock of a transmission: the time of a pass, and the new minutes.

    The sequence is a function of the group index, so the caller gives the start time
    and the length of a pass. A streamed transmission and a buffer built for the same
    moment then agree.
    """

    def __init__(self, start: datetime, pass_s: float, offset_half_hours: int = 0):
        self.start = start
        self.pass_s = pass_s
        self.offset_half_hours = offset_half_hours

    def at(self, pass_index: int) -> datetime:
        return self.start + timedelta(seconds=pass_index * self.pass_s)

    def is_new_minute(self, pass_index: int) -> bool:
        if pass_index == 0:
            return True
        return self.at(pass_index).minute != self.at(pass_index - 1).minute


def group_pattern(ps: str) -> tuple[int, ...]:
    """The group sequence: the RadioText burst, then the PS groups.

    A transmission without a PS name has nothing for Group 0A, so it is the burst
    alone.
    """
    burst = (GROUP_RT,) * RT_SEGMENTS
    named = any(name.strip() for name in ps_names(ps))
    return burst + (GROUP_PS,) * PS_GROUPS_PER_PASS if named else burst


def pass_groups(ps: str) -> int:
    """The groups in one complete RadioText pass: the burst, plus the PS groups."""
    return len(group_pattern(ps))


def pass_seconds(ps: str) -> float:
    """One RadioText pass in seconds: 1.75 s in the station cadence."""
    return pass_groups(ps) * GROUP_S


def hold_seconds(cycle_s: float | None, ps: str) -> float:
    """How long each message stays up: whole passes, at least two.

    A receiver wants each segment a second time before it accepts it, so a message
    never occupies one pass only. The default is `DEFAULT_HOLD_S`. A `cycle_s`
    rotates faster, for a demonstration.
    """
    wanted = cycle_s if cycle_s else DEFAULT_HOLD_S
    passes = max(DEFAULT_HOLD_PASSES, int(round(wanted / pass_seconds(ps))))
    return passes * pass_seconds(ps)


def _pulse(tau: np.ndarray) -> np.ndarray:
    """The impulse response of the data-shaping filter, in bit-period units.

    It is the reverse transform of H_T(f) = cos(pi f td/4) below 2/td. The value at
    tau = +-1/8 is the limit 4*pi, where numerator and denominator vanish together.
    """
    den = 0.0625 - 4.0 * tau ** 2
    near = np.abs(den) < 1e-9
    return np.where(near, 4.0 * np.pi, np.cos(4.0 * np.pi * tau) / np.where(near, 1.0, den))


#: The symbol's peak, so `symbol()` comes out normalized to +-1.
SYMBOL_PEAK = _symbol_peak()


def symbol(tau: np.ndarray) -> np.ndarray:
    """One biphase symbol, tau bit periods from its bit start, normalized to +-1.

    s(tau) = h(tau) - h(tau - 1/2). The symbol has a positive lobe at the bit start
    and a negative lobe at the bit middle. It is odd about tau = 1/4 and has no mean.
    The code truncates it to the three-bit window of the standard, which is the window
    the reference encoders tabulate.
    """
    inside = np.abs(tau - 0.25) <= BIPHASE_WINDOW
    centred = np.where(inside, tau, 0.25)
    return np.where(inside, _pulse(centred) - _pulse(centred - 0.5), 0.0) / SYMBOL_PEAK


def baseband(t: np.ndarray, bits: np.ndarray, first_bit: int = 0) -> np.ndarray:
    """The RDS baseband at the sample times `t`, in +-1.

    The symbols overlap over three bit periods, so a sample is the sum of the bit
    under it and its two neighbours. A 1 adds the symbol and a 0 subtracts it.
    `bits[0]` is the global bit `first_bit`. A live transmission never wraps, so
    `bits` holds one bit of overlap on each side of the chunk.
    """
    index = np.floor(t * SYMBOL_RATE).astype(np.int64) - first_bit
    frac = t * SYMBOL_RATE - np.floor(t * SYMBOL_RATE)
    out = np.zeros(t.shape)
    for offset in (-1, 0, 1):
        bit = bits[np.clip(index + offset, 0, bits.size - 1)]
        out += np.where(bit == 1, 1.0, -1.0) * symbol(frac - offset)
    return out


def pilot_wave(t: np.ndarray, level: float = 1.0) -> np.ndarray:
    """The 19 kHz pilot at `level` of the audio scale, as a sine.

    A broadcast carries the pilot at about 9% of the deviation, also for mono audio.
    EN 50067 section 1.6 locks the 57 kHz subcarrier to its third harmonic, so a
    receiver that recovers its RDS clock from the pilot needs it. The pilot is a sine
    and the subcarrier a cosine: that is the 90 degrees of the standard.
    """
    return level * np.sin(2.0 * np.pi * PILOT_HZ * t)


class StationStream:
    """The transmitted bits of a station, made on demand for a live transmission.

    A transmitter sends groups without a loop. There is no wrap for a receiver to
    lose block sync on, and no buffer limit on the hold. The bits come out in order.
    `transmitted` serves the last two bits again, the overlap of the symbol window.
    """

    def __init__(self, pi: int, pty: int, ps: str, texts: list[str],
                 cycle_s: float | None = None, tp: bool = True, ab_toggle: bool = False,
                 clock: Clock | None = None):
        self.pi, self.pty, self.ps = pi, pty, ps
        self.texts = list(texts)
        self.tp, self.ab_toggle, self.clock = tp, ab_toggle, clock
        self.hold_passes = int(round(hold_seconds(cycle_s, ps) / pass_seconds(ps)))
        self._first = 0                 # the global index of self._bits[0]
        self._bits = np.zeros(0, dtype=np.uint8)
        self._group = 0                 # the next group to emit
        self._carry = 0                 # the last transmitted bit, for the chain

    def transmitted(self, first: int, last: int) -> tuple[np.ndarray, int]:
        """The transmitted bits for the global indices `first` to `last`, and the first one.

        An index before the start of the stream is clipped. No bit exists before the
        first one, so the tail of its symbol is the first bit itself.
        """
        first = max(first, self._first)
        while self._first + self._bits.size <= last:
            data = station_group(self.pi, self.pty, self.ps, self.texts, self.hold_passes,
                                 self._group, self.tp, self.ab_toggle, self.clock)
            sent = np.bitwise_xor.accumulate(data) ^ self._carry   # d_n = b_n XOR d_n-1
            self._carry = int(sent[-1])
            self._bits = np.concatenate((self._bits, sent))
            self._group += 1
        if self._first < first - 2:                 # keep the symbol overlap only
            drop = (first - 2) - self._first
            self._bits = self._bits[drop:]
            self._first += drop
        return self._bits[first - self._first:last-self._first + 1], first


def station_wave(t: np.ndarray, stream: StationStream, level: float = 1.0) -> np.ndarray:
    """The RDS subcarrier at the absolute times `t`, from a live stream.

    `level` is the amplitude against the audio. It becomes the RDS deviation when the
    composite drives an FM modulator: pass rds_deviation_hz / wfm_deviation_hz for
    the usual injection.
    """
    first = int(np.floor(t[0] * SYMBOL_RATE)) - 1 if t.size else 0
    last = int(np.floor(t[-1] * SYMBOL_RATE)) + 1
    bits, bits_first = stream.transmitted(first, last)
    return level * baseband(t, bits, bits_first) * np.cos(2.0 * np.pi * SUBCARRIER_HZ * t)


if __name__ == '__main__':  # pragma: no cover - a library module
    raise SystemExit('rds_encode is a library module; run tools/pluto_tx.py wfm --rds-rt TEXT')
