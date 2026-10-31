#!/usr/bin/env python3
"""Known-content FT4, WSPR, APRS, AIS and DTMF Pluto bench signals.

Build the external OnAir encoder once with tools/pluto/build-digital-source.sh.
All modes default to the 435 MHz bench carrier. AIS uses a -25 kHz channel
relative to that carrier; receiving it requires the explicit AIS bench mapping.
FT4/WSPR start on UTC slots, including their 0.5/1 s protocol lead-in. Pluto
has no timestamped TX: transport startup uncertainty is reported, and --tx-delay
can compensate a measured delay. Keep the host UTC clock synchronized.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pluto_radio as radio

MODES = ('ft4', 'wspr', 'aprs', 'ais', 'dtmf')
PERIODS = {'ft4': 7.5, 'wspr': 120.0, 'aprs': 3.0, 'ais': 3.0}
MESSAGES = {'ft4': 'CQ K1ABC FN42', 'wspr': 'K1ABC FN42 30',
            'aprs': '!4903.50N/07201.75W>WEBSA APRS TEST'}
DTMF_KEYS = ('123A', '456B', '789C', '*0#D')


def dtmf_source(args):
    """FM of equal-amplitude standard DTMF pairs, with continuous carrier phase."""
    rate = 48000
    tone_n = round(args.tone_ms * rate / 1000)
    pause_n = round(args.pause_ms * rate / 1000)
    period = len(args.digits) * (tone_n + pause_n) / rate
    audio = np.zeros(round(period * rate), dtype=np.float32)
    t = np.arange(tone_n) / rate
    for i, digit in enumerate(args.digits):
        row = next(r for r, keys in enumerate(DTMF_KEYS) if digit in keys)
        col = DTMF_KEYS[row].index(digit)
        a = 0.5 * (np.sin(2 * np.pi * (697, 770, 852, 941)[row] * t)
                   + np.sin(2 * np.pi * (1209, 1336, 1477, 1633)[col] * t))
        start = i * (tone_n + pause_n)
        audio[start:start + tone_n] = a
    phase = 2 * np.pi * args.deviation_hz * np.cumsum(audio, dtype=np.float64) / rate
    # Include the net modulation phase on each repetition; the frequency shift
    # is applied separately at the actual DAC rate, never reset at a chunk edge.
    return np.exp(1j * phase).astype(np.complex64), rate, period, args.digits, float(phase[-1])


def encoded_source(args):
    if args.mode == 'dtmf':
        return dtmf_source(args)
    helper = Path(args.encoder).expanduser().resolve()
    if not helper.is_file() or not os.access(helper, os.X_OK):
        raise ValueError(f'encoder unavailable: {helper}; build it with '
                         'bash tools/pluto/build-digital-source.sh /path/to/OnAir')
    message = str(args.mmsi) if args.mode == 'ais' else args.message
    base = args.base_hz if args.mode in ('ft4', 'wspr') else (-25000 if args.mode == 'ais' else 0)
    period = PERIODS[args.mode]
    with tempfile.TemporaryDirectory(prefix='pluto-digital-') as directory:
        output = Path(directory) / 'source.cf32'
        result = subprocess.run([str(helper), str(output), args.mode, message,
                                 str(base), str(period)], check=True, capture_output=True, text=True)
        metadata = json.loads(result.stdout)
        rate = int(metadata['rate'])
        actual_period = float(metadata['period'])
        if actual_period != period or rate <= 0 or output.stat().st_size != round(rate * period) * 8:
            raise ValueError('encoder returned inconsistent source rate, period or file size')
        samples = np.fromfile(output, dtype=np.dtype('<c8'))
    if not np.all(np.isfinite(samples)) or not np.any(samples):
        raise ValueError('encoder produced nonfinite or silent IQ')
    peak = float(np.max(np.abs(samples)))
    samples /= peak
    return samples, rate, period, str(metadata['expected']), 0.0


class PeriodicResampler:
    """Windowed-sinc interpolation with a global sample clock and bounded memory.

    A 1024-phase, 32-tap Kaiser bank suppresses source-rate images. The source
    position derives from the absolute DAC sample number, so chunk boundaries
    neither reset filter history nor round the sample-rate ratio. Only one low
    rate period is retained (7.68 MB for WSPR), never a 120 s DAC-rate buffer.
    """
    def __init__(self, source, source_rate, output_rate, scale, phase_per_period=0.0):
        self.source = source
        self.ratio = source_rate / output_rate
        self.scale = scale
        self.phase_per_period = phase_per_period
        self.taps = np.arange(-15, 17, dtype=np.int64)
        d = np.arange(1024)[:, None] / 1024 - self.taps
        bank = 0.9 * np.sinc(0.9 * d) * np.kaiser(32, 8.6)
        bank /= bank.sum(axis=1)[:, None]
        self.bank = bank.astype(np.float32)

    def chunk(self, start, count, delay_samples=0):
        if start + count <= delay_samples:
            return np.zeros(count, dtype=np.complex64)
        position = (np.arange(count, dtype=np.float64) + (start - delay_samples)) * self.ratio
        whole = np.floor(position).astype(np.int64)
        fraction = np.minimum(1023, ((position - whole) * 1024).astype(np.int64))
        # Prepare the small input-rate window once. In particular, DTMF's
        # continuous FM phase advances once per source period, not once per
        # output sample and FIR tap. Each window spans only a few periods.
        first = int(whole[0]) + int(self.taps[0])
        last = int(whole[-1]) + int(self.taps[-1]) + 1
        index = np.arange(first, last, dtype=np.int64)
        window = self.source[index % self.source.size].copy()
        if self.phase_per_period:
            first_period = first // self.source.size
            last_period = (last - 1) // self.source.size
            for period in range(first_period, last_period + 1):
                left = max(first, period * self.source.size) - first
                right = min(last, (period + 1) * self.source.size) - first
                window[left:right] *= np.complex64(np.exp(1j * self.phase_per_period * period))
        local = whole - first
        out = np.zeros(count, dtype=np.complex64)
        for k, tap in enumerate(self.taps):
            out += window[local + tap] * self.bank[fraction, k]
        # Initial preroll is real silence, including the filter's left wing.
        out[position < 0] = 0
        out *= self.scale
        return out


def transmit(args, source, source_rate, period, phase_per_period):
    with radio.Radio(args) as tx:
        if tx.rate < source_rate or abs(args.offset_hz) + 40000 >= tx.rate / 2:
            raise ValueError(f'accepted hardware rate {tx.rate} cannot carry this source/offset')
        count = max(1, round(tx.rate * args.chunk))
        chunk_s = count / tx.rate
        resampler = PeriodicResampler(source, source_rate, tx.rate, args.scale, phase_per_period)
        zero = np.zeros(count, dtype=np.complex64)
        tx.open_stream(kernel_buffers=4)
        # Prime the interpolation path before starting the hardware queue.
        # Its temporary allocations must not delay the first active DMA block.
        resampler.chunk(0, count)
        start = tx.prime_stream(zero)
        sent = start.submitted_samples
        mono0, utc0 = start.monotonic_start, start.utc_start
        startup = start.activation_seconds
        # Hardware has no timed-start API. Use the activation call's midpoint
        # for sample zero and report the observed interval as uncertainty.
        epoch = utc0 + startup / 2 + args.tx_delay
        future = time.time() + 6 * chunk_s
        first_slot = (math.floor(future / period) + 1) * period if args.mode in ('ft4', 'wspr') else future
        delay_samples = round((first_slot - epoch) * tx.rate)
        stop = None if args.slots == 0 else delay_samples + round(args.slots * period * tx.rate)
        print(f'TX on: LO={tx.sdr.tx_lo} rate={tx.rate} gain={args.gain}; '
              f'{count} samples/chunk, four queued DMA buffers', flush=True)
        print(f'first period: {time.strftime("%Y-%m-%d %H:%M:%S", time.gmtime(first_slot))} UTC '
              f'(epoch={first_slot:.6f}); startup uncertainty ±{startup / 2:.3f}s; '
              f'tx-delay={args.tx_delay:+.3f}s', flush=True)
        announced = -1
        while stop is None or sent < stop:
            chunk = resampler.chunk(sent, count, delay_samples)
            if args.offset_hz:
                t = (np.arange(count, dtype=np.float64) + sent) / tx.rate
                chunk *= np.exp(2j * np.pi * args.offset_hz * t).astype(np.complex64)
            if stop is not None and sent + count > stop:
                chunk[max(0, stop - sent):] = 0
            tx.push_chunk(chunk)
            sent += count
            elapsed = time.monotonic() - mono0
            ahead = sent / tx.rate - elapsed
            if ahead < -2 * chunk_s:
                raise RuntimeError(f'TX stream fell {abs(ahead):.3f}s behind; '
                                   'UTC phase and uninterrupted waveform no longer guaranteed')
            if abs((time.time() - utc0) - elapsed) > 0.1:
                raise RuntimeError('UTC clock stepped during TX; restart after clock synchronization')
            slot = math.floor((sent - delay_samples) / (period * tx.rate))
            if slot >= 0 and slot != announced:
                announced = slot
                print(f'  period {slot + 1}; queue lead {ahead:+.3f}s', flush=True)
        tx.drain_stream(zero)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('mode', choices=MODES)
    parser.add_argument('--message', help='FT4/WSPR encodable text, or full APRS information field')
    parser.add_argument('--mmsi', type=int, default=367000001, help='AIS type 1 identity, nine digits')
    parser.add_argument('--base-hz', type=float, default=1500, help='lowest FT4/WSPR tone, Hz above LO')
    parser.add_argument('--offset-hz', '--offset', type=float, default=0,
                        help='additional RF carrier offset in Hz (use 1000 for DTMF offset testing)')
    parser.add_argument('--slots', '--repeat', type=int, default=1, help='complete periods to transmit (0 = forever)')
    parser.add_argument('--chunk', type=float, default=0.25, help='stream buffer seconds, 0.05..0.5')
    parser.add_argument('--tx-delay', type=float, default=0,
                        help='measured transport/DAC delay correction in seconds; positive means RF starts later')
    parser.add_argument('--encoder', default=os.environ.get(
                            'PLUTO_DIGITAL_SOURCE', str(Path(__file__).with_name('pluto_digital_source'))),
                        help='built OnAir protocol encoder executable (default: PLUTO_DIGITAL_SOURCE or sibling helper)')
    parser.add_argument('--digits', default='123A#', help='DTMF sequence, symbols 0-9 A-D * #')
    parser.add_argument('--tone-ms', type=float, default=200, help='DTMF tone duration in ms')
    parser.add_argument('--pause-ms', type=float, default=100, help='DTMF interdigit pause in ms')
    parser.add_argument('--deviation-hz', type=float, default=2500, help='DTMF FM peak deviation in Hz')
    radio.add_radio_args(parser, default_lo=435e6, default_gain=-40,
                         dry_run_help='encode the complete known-content source and preview DAC-rate chunks; no hardware')
    args = parser.parse_args(argv)
    args.message = args.message if args.message is not None else MESSAGES.get(args.mode, '')
    args.digits = args.digits.upper()
    for key in ('rate', 'lo', 'gain', 'scale', 'chunk', 'base_hz', 'offset_hz', 'tx_delay',
                'tone_ms', 'pause_ms', 'deviation_hz', 'xo_ppm'):
        if not math.isfinite(getattr(args, key)):
            parser.error(f'--{key.replace("_", "-")} must be finite')
    if args.rate < radio.MIN_TX_RATE_HZ or args.rate > 61440000:
        parser.error('--rate must be between 521000 and 61440000 Hz')
    if not 0 < args.scale <= radio.DAC_FULL_SCALE or not -89.75 <= args.gain <= 0:
        parser.error('--scale must be in (0,16384] and --gain in [-89.75,0]')
    if args.slots < 0 or not 0.05 <= args.chunk <= 0.5:
        parser.error('--slots must be nonnegative; --chunk must be 0.05..0.5 seconds')
    if args.mode in ('ft4', 'wspr') and not 100 <= args.base_hz <= 3000:
        parser.error('--base-hz must be 100..3000 for the 8 kHz FTX source')
    if not 100000000 <= args.mmsi <= 999999999:
        parser.error('--mmsi must contain nine digits')
    if args.mode == 'dtmf' and (not args.digits or len(args.digits) > 256
            or any(c not in '0123456789ABCD*#' for c in args.digits)
            or not 20 <= args.tone_ms <= 2000 or not 20 <= args.pause_ms <= 2000
            or not 0 < args.deviation_hz <= 5000):
        parser.error('DTMF needs 1..256 valid symbols, tone/pause 20..2000 ms, deviation 0..5000 Hz')
    if abs(args.offset_hz) + 40000 >= args.rate / 2:
        parser.error('--offset-hz places the waveform outside the DAC Nyquist band')
    try:
        source, source_rate, period, expected, phase_per_period = encoded_source(args)
        print(f'mode           : {args.mode}\nexpected decode: {expected}\n'
              f'source         : {source_rate} Hz, {source.size} complex samples, {source.nbytes / 1e6:.3f} MB\n'
              f'period         : {period:g}s; count={args.slots or "continuous"}\n'
              f'bench LO       : {args.lo / 1e6:.6f} MHz; gain={args.gain:g} dB; scale={args.scale:g}')
        if args.mode == 'ais':
            print(f'AIS channel A  : {(args.lo - 25000 + args.offset_hz) / 1e6:.6f} MHz; '
                  'type 1, lat=37.7749 lon=-122.4194; receiver needs AIS bench frequency mapping')
        if args.mode in ('ft4', 'wspr'):
            print(f'UTC phase      : {period:g}s slots; protocol lead-in '
                  f'{0.5 if args.mode == "ft4" else 1.0:g}s; tone zero {args.base_hz + args.offset_hz:g} Hz')
        if args.dry_run:
            # Include active samples, not merely the slot's initial silence.
            preview = PeriodicResampler(source, source_rate, args.rate, args.scale, phase_per_period)
            active = int(np.flatnonzero(np.abs(source) > 0.01)[0] * args.rate / source_rate)
            count = round(args.rate * args.chunk)
            chunk = preview.chunk(active, count)
            print(f'DAC preview    : {count} samples, peak={np.max(np.abs(chunk)):.1f}; '
                  f'chunk memory={chunk.nbytes / 1e6:.3f} MB')
            print('--dry-run: complete source encoded; radio untouched.')
            return 0
        return transmit(args, source, source_rate, period, phase_per_period)
    except KeyboardInterrupt:
        print('interrupted')
        return 130
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as exc:
        print(f'{args.mode}: {exc}', file=sys.stderr)
        if isinstance(exc, subprocess.CalledProcessError) and exc.stderr:
            print(exc.stderr.strip(), file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
