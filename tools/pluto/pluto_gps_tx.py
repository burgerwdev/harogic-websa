#!/usr/bin/env python3
"""Stream independent GPS L1 C/A navigation on the exact 435 MHz bench carrier.

The synthetic stationary receiver is at 25.2 N, 55.36 E, 10 m ellipsoidal height.
Signal time starts at gps-sdr-sim's 2022/01/01,00:00:00 scenario epoch and advances
by sample count, independently of the host clock. RINEX is brdc0010.22n (2022).
This tool never transmits at GPS L1. Its only requested RF carrier is 435000000 Hz.
Hardware LO readback may differ by up to 100 Hz (<0.25 ppm) for PLL quantization;
the actual carrier and translation offset are reported before transmission.
Hardware gain defaults to -70 dB and accepts -89.75..-10 dB for the bench setup.

Install: bash tools/setup-gps-simulator.sh
Inspect: python3 tools/pluto_tx.py gps --dry-run
Bench:   python3 tools/pluto_tx.py gps --duration 180 --gain -70
Offline independent fixture, without storing IQ (bash; requires NumPy):
  set -o pipefail
  python3 tools/pluto_tx.py gps --stdout --format cf32 --rate 4000000 | \\
    WEBSA_GPS_BENCH_HZ=435000000 backend/target/release/sdr_decode \\
      --input /dev/stdin --mode gps --input-rate 4000000 \\
      --bandwidth 2046000 --center-hz 435000000 --realtime \\
      --report-json /tmp/gps-bench-pvt.json
Set WEBSA_GNSS_PRE_2009_FILE=false in the receiver environment for this fixture.
RF receiver rate is independently 4 MSps; transmitter defaults to 2.6 MSps.
Before generation, RF mode negotiates a nearby hardware rate (within 20 Hz of
the request) whose readback is divisible by ten, as required by gps-sdr-sim.
The encoder and signal-time accounting both use that exact accepted readback.

A bounded producer queue absorbs CPU jitter. Conservative host sample deadlines
stop the stream if continuity cannot be guaranteed; they do not measure hardware
DMA underrun flags. Radio's shared four-buffer queue carries one uninterrupted waveform.
No cyclic repetition, large IQ file, or whole-scenario allocation is used.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import queue
import shlex
import subprocess
import sys
import threading
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pluto_radio as radio

REVISION = '28ca29a6719475195e3aabd5930c4ed02d67190f'
ARCHIVE_SHA256 = '046209d533e11628ba9cf44abf9717809f981f4de3eff13bd5a6d799f5671560'
BENCH_HZ = 435_000_000
MAX_LO_READBACK_ERROR_HZ = 100
LOGICAL_HZ = 1_575_420_000
SCENARIO_TIME = '2022/01/01,00:00:00'
POSITION = '25.2,55.36,10'
QUEUE_BLOCKS = 16  # 1.6 seconds, 8.32 MB CS8 at the default rate.


def default_source_dir():
    cache = Path(os.environ.get('XDG_CACHE_HOME', str(Path.home() / '.cache')))
    return Path(os.environ.get('WEBSA_GPS_SIM_ROOT', str(cache / 'harogic-websa' / 'gps-sdr-sim' / REVISION)))


def provenance(root):
    """Check the installer manifest before executing its pinned encoder."""
    expected = {'gps-sdr-sim', 'gpssim.c', 'gpssim.h', 'LICENSE', 'brdc0010.22n', 'provenance.json'}
    seen = set()
    for line in (root / 'SHA256SUMS').read_text().splitlines():
        digest, name = line.split(maxsplit=1)
        name = name.lstrip('*')
        if name not in expected or name in seen:
            raise ValueError('invalid GPS simulator installation manifest')
        hasher = hashlib.sha256()
        with (root / name).open('rb') as source:
            for block in iter(lambda: source.read(65536), b''):
                hasher.update(block)
        if hasher.hexdigest() != digest:
            raise ValueError(f'GPS simulator checksum mismatch: {name}; rerun installer')
        seen.add(name)
    if seen != expected:
        raise ValueError('incomplete GPS simulator installation manifest')
    info = json.loads((root / 'provenance.json').read_text())
    if info.get('revision') != REVISION or info.get('archive_sha256') != ARCHIVE_SHA256:
        raise ValueError('GPS simulator installation does not match pinned source')
    if not os.access(root / 'gps-sdr-sim', os.X_OK):
        raise ValueError('GPS simulator executable is not executable')
    return info


def command(root, rate, duration):
    # This pinned revision emits numd-1 blocks of 0.1 s. Add one block so the
    # user duration is the exact number of emitted samples, including at EOF.
    return [str(root / 'gps-sdr-sim'), '-e', str(root / 'brdc0010.22n'),
            '-l', POSITION, '-t', SCENARIO_TIME, '-d', f'{duration + 0.1:.1f}',
            '-s', str(rate), '-b', '8', '-o', '-']


def validate_rate(rate):
    # Upstream silently floors the rate to a multiple of ten samples/second.
    if not 2_046_000 <= rate <= 4_000_000 or rate % 10:
        raise ValueError(f'rate {rate} must be 2046000..4000000 Hz and divisible by 10; '
                         'cannot preserve scenario timing at this hardware rate')


def negotiate_rate(tx, requested):
    """Find a nearby representable clock before creating a waveform or TX buffer."""
    initial = int(tx.sdr.sample_rate)
    writes = [{'requested_hz': requested, 'readback_hz': initial}]
    if (2_046_000 <= initial <= 4_000_000 and initial % 10 == 0
            and abs(initial - requested) <= 20):
        tx.rate = initial
        return writes
    # Correct the observed clock rounding first, then search adjacent integer
    # requests. Never label a rounded readback as the requested simulator rate.
    corrected = requested + (requested - initial)
    candidates = [corrected]
    candidates.extend(requested + sign * step
                      for step in range(1, 21) for sign in (1, -1))
    attempted = {requested}
    last_readback = initial
    for candidate in candidates:
        if (candidate in attempted or abs(candidate - requested) > 20
                or not 2_046_000 <= candidate <= 4_000_000):
            continue
        attempted.add(candidate)
        try:
            tx.sdr.sample_rate = candidate
        except (OSError, ValueError) as exc:
            writes.append({'requested_hz': candidate, 'error': str(exc)})
            continue
        actual = int(tx.sdr.sample_rate)
        last_readback = actual
        writes.append({'requested_hz': candidate, 'readback_hz': actual})
        if (2_046_000 <= actual <= 4_000_000 and actual % 10 == 0
                and abs(actual - requested) <= 20):
            tx.rate = actual
            print('GPS sample-rate negotiation: ' + json.dumps(writes),
                  file=sys.stderr, flush=True)
            return writes
    print('GPS sample-rate negotiation failed: ' + json.dumps(writes),
          file=sys.stderr, flush=True)
    raise ValueError(f'no GPS-compatible hardware sample rate within 20 Hz of '
                     f'{requested}; initial readback={initial}, last readback={last_readback}; '
                     'a readback divisible by ten is required before transmission')


class Producer:
    """One subprocess and one bounded reader; termination always reaps the child."""
    def __init__(self, argv, block_bytes, blocks):
        self.argv, self.block_bytes, self.blocks = argv, block_bytes, blocks
        self.items = queue.Queue(maxsize=QUEUE_BLOCKS)
        self.stop = threading.Event()
        self.process = None
        self.thread = None

    def put(self, item):
        while not self.stop.is_set():
            try:
                self.items.put(item, timeout=0.1)
                return
            except queue.Full:
                pass

    def read(self):
        try:
            for _ in range(self.blocks):
                data = bytearray()
                while len(data) < self.block_bytes:
                    if self.stop.is_set():
                        return
                    part = self.process.stdout.read(self.block_bytes - len(data))
                    if not part:
                        raise RuntimeError('GPS encoder ended before the requested sample count')
                    data.extend(part)
                self.put(data)
            if self.process.stdout.read(1):
                raise RuntimeError('GPS encoder emitted more samples than requested')
            status = self.process.wait()
            if status:
                raise RuntimeError(f'GPS encoder exited with status {status}')
            self.put(None)
        except Exception as exc:
            self.put(exc)

    def __enter__(self):
        self.process = subprocess.Popen(self.argv, stdout=subprocess.PIPE, bufsize=0)
        self.thread = threading.Thread(target=self.read, name='gps-encoder-reader', daemon=True)
        self.thread.start()
        return self

    def get(self, deadline=None):
        timeout = 120.0 if deadline is None else max(0.0, deadline - time.monotonic())
        try:
            item = self.items.get(timeout=timeout)
        except queue.Empty:
            raise RuntimeError('GPS producer missed its sample deadline; stopping to avoid '
                               'a discontinuous navigation signal') from None
        if isinstance(item, Exception):
            raise item
        return item

    def finish(self):
        if self.get() is not None:
            raise RuntimeError('GPS encoder supplied unexpected trailing samples')

    def __exit__(self, *_exc):
        self.stop.set()
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.thread.join(timeout=3)
        self.process.stdout.close()


def sample_stats(data, scale):
    iq = np.frombuffer(data, dtype=np.int8).astype(np.float32).reshape(-1, 2)
    return {'complex_samples': len(iq), 'cs8_component_peak': float(np.max(np.abs(iq))),
            'normalized_complex_rms': float(np.sqrt(np.mean(np.sum(iq * iq, axis=1))) / 128),
            'dac_component_peak': float(np.max(np.abs(iq)) * scale / 128),
            'dac_scale_per_cs8_count': scale / 128,
            'cs8_sha256': hashlib.sha256(data).hexdigest()}


def to_dac(data, output, scale):
    # pyadi casts each component to int16 without scaling. Default scale=8192
    # maps CS8 -128..127 exactly to -8192..8128 (64 DAC counts per CS8 count).
    components = output.view(np.float32)
    np.multiply(np.frombuffer(data, dtype=np.int8), scale / 128,
                out=components, casting='unsafe')
    return output


def dry_run(args, info):
    rate = int(args.rate)
    argv = command(args.source_dir, rate, 0.5)
    collected = bytearray()
    with Producer(argv, rate // 10 * 2, 5) as source:
        for _ in range(5):
            collected.extend(source.get())
        source.finish()
    print(json.dumps({'synthetic': True, 'hardware_opened': False, 'provenance': info,
                      'source_hz': BENCH_HZ, 'logical_hz': LOGICAL_HZ,
                      'translation_hz': BENCH_HZ - LOGICAL_HZ,
                      'scenario_time': SCENARIO_TIME, 'position': [25.2, 55.36, 10],
                      'requested_signal_seconds': args.duration, 'sample_rate': rate,
                      'gain_db': args.gain, 'sample_probe_seconds': 0.5,
                      'sample_probe': sample_stats(collected, args.scale),
                      'transmit_encoder_command': command(args.source_dir, rate, args.duration)}, indent=2))
    return 0


def offline(args):
    rate = int(args.rate)
    count = rate // 10
    converted = np.empty(count * 2, dtype='<f4') if args.format == 'cf32' else None
    with Producer(command(args.source_dir, rate, args.duration), count * 2, args.duration * 10) as source:
        for _ in range(args.duration * 10):
            data = source.get()
            if converted is not None:
                np.multiply(np.frombuffer(data, dtype=np.int8), 1 / 128,
                            out=converted, casting='unsafe')
                sys.stdout.buffer.write(memoryview(converted).cast('B'))
            else:
                sys.stdout.buffer.write(data)
        source.finish()
        sys.stdout.buffer.flush()
    return 0


def transmit(args):
    with radio.Radio(args) as tx:
        rate_writes = negotiate_rate(tx, int(args.rate))
        validate_rate(tx.rate)
        actual_lo = int(tx.sdr.tx_lo)
        if abs(actual_lo - BENCH_HZ) > MAX_LO_READBACK_ERROR_HZ:
            raise ValueError(f'hardware LO {actual_lo} Hz differs from the 435 MHz bench '
                             f'carrier by more than {MAX_LO_READBACK_ERROR_HZ} Hz')
        if tx.rate != int(args.rate):
            print(f'Hardware accepted {tx.rate} Hz; generating at that exact rate before TX', file=sys.stderr)
        tx.sdr.tx_rf_bandwidth = 2_500_000
        count = tx.rate // 10
        samples = np.empty(count, dtype=np.complex64)
        zero = np.zeros(count, dtype=np.complex64)
        tx.open_stream(kernel_buffers=4)
        blocks = args.duration * 10
        with Producer(command(args.source_dir, tx.rate, args.duration), count * 2, blocks) as source:
            # Prefetch one second before RF starts. The reader can hold at most
            # sixteen more blocks; memory stays bounded regardless of duration.
            prefetched = [source.get() for _ in range(10)]
            print(f'SYNTHETIC GPS BENCH: TX {actual_lo} Hz (requested {BENCH_HZ} Hz), '
                  f'logical L1 {LOGICAL_HZ} Hz; '
                  f'rate={tx.rate}, gain={args.gain} dB, duration={args.duration}s; '
                  f'epoch={SCENARIO_TIME}, position={POSITION}; four DMA buffers', file=sys.stderr, flush=True)
            print('RF provenance: ' + json.dumps({
                'synthetic': True, 'requested_source_hz': BENCH_HZ,
                'actual_source_hz': actual_lo, 'logical_hz': LOGICAL_HZ,
                'actual_translation_hz': actual_lo - LOGICAL_HZ,
                'lo_readback_error_hz': actual_lo - BENCH_HZ,
                'sample_rate': tx.rate,
                'requested_sample_rate': int(args.rate),
                'sample_rate_writes': rate_writes,
            }), file=sys.stderr, flush=True)
            print('First block: ' + json.dumps(sample_stats(prefetched[0], args.scale)), file=sys.stderr, flush=True)
            start = tx.prime_stream(zero, minimum_blocks=2)
            started = start.monotonic_start
            # Exclude libiio1's pending writable block from continuity deadlines.
            submitted = start.queued_samples // count
            for index in range(blocks):
                deadline = started + submitted * 0.1
                data = prefetched[index] if index < len(prefetched) else source.get(deadline)
                to_dac(data, samples, args.scale)
                if time.monotonic() >= deadline:
                    raise RuntimeError(f'GPS stream missed its continuity deadline at '
                                       f'{index / 10:.1f}s; possible underrun, stopping RF')
                tx.push_chunk(samples)
                submitted += 1
                # Includes blocking transport time: a stall cannot silently turn
                # the scenario into wall-clock-paced fragments.
                if time.monotonic() >= deadline:
                    raise RuntimeError(f'GPS transport exceeded its queue deadline at '
                                       f'{index / 10:.1f}s; continuity unverified, stopping RF')
            source.finish()
            tx.drain_stream(zero)
    print(f'Completed {args.duration}s synthetic scenario; RF continuity still requires receiver evidence.', file=sys.stderr)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    radio.add_radio_args(parser, default_lo=BENCH_HZ, default_gain=-70,
                         default_rate=2_600_000, default_scale=radio.DAC_FULL_SCALE / 2,
                         dry_run_help='generate a bounded 0.5 s probe and report provenance/RMS/peak; no hardware')
    parser.add_argument('--source-dir', type=Path, default=default_source_dir(), help='pinned simulator installation prefix')
    parser.add_argument('--duration', type=int, default=180, help='exact signal seconds, 60..3600 (default 180)')
    parser.add_argument('--stdout', action='store_true', help='offline IQ on stdout; never open hardware')
    parser.add_argument('--format', choices=('cs8', 'cf32'), default='cf32', help='offline stdout format (CF32 is little-endian, normalized CS8/128)')
    args = parser.parse_args(argv)
    if args.lo != BENCH_HZ:
        parser.error('this synthetic bench tool permits only --lo 435000000; GPS L1 RF transmission is refused')
    if not math.isfinite(args.rate) or args.rate != int(args.rate):
        parser.error('--rate must be a finite integer sample rate')
    if not 60 <= args.duration <= 3600:
        parser.error('--duration must be 60..3600 seconds')
    if not -89.75 <= args.gain <= -10:
        parser.error('--gain must be -89.75..-10 dB for this bench tool')
    if not 0 < args.scale <= radio.DAC_FULL_SCALE or not math.isfinite(args.xo_ppm):
        parser.error('--scale must be >0 and <=16384; --xo-ppm must be finite')
    if args.dry_run and args.stdout:
        parser.error('--dry-run and --stdout are separate offline operations')
    radio.install_signal_handlers()
    try:
        validate_rate(int(args.rate))
        args.source_dir = args.source_dir.expanduser().resolve()
        info = provenance(args.source_dir)
        print('SYNTHETIC GPS BENCH ONLY: 435 MHz transport, 2022 RINEX scenario at '
              '25.2,55.36,10; navigation time is sample-count based, not host UTC.', file=sys.stderr)
        if args.dry_run:
            return dry_run(args, info)
        if args.stdout:
            return offline(args)
        return transmit(args)
    except KeyboardInterrupt:
        print('GPS bench interrupted; encoder stopped and radio cleanup requested.', file=sys.stderr)
        return 130
    except (OSError, ValueError, RuntimeError) as exc:
        print(f'GPS bench: {exc}\nInstall source with: bash tools/setup-gps-simulator.sh '
              f'--prefix {shlex.quote(str(args.source_dir))}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
