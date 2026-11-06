#!/usr/bin/env python3
"""Owned eight-frequency RF bench helpers; restore the live receiver and Pluto settings."""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import struct
import subprocess
import sys
import threading
import time
import traceback
from pathlib import Path
from types import SimpleNamespace

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools/e2e'))
sys.path.insert(0, str(ROOT / 'tools/pluto'))
from native_multichannel import api  # noqa: E402
from native_multichannel_recording import until  # noqa: E402
from pluto_radio import Radio, read_xo_correction  # noqa: E402
from rust_iq_browser import free_port, post, start_service, state, stop  # noqa: E402
from websockets.sync.client import connect  # noqa: E402

CENTER = 435_000_000
FREQUENCIES = [CENTER - 70000 + index * 20000 for index in range(8)]
TONES = [400 + index * 200 for index in range(8)]
MODES = ['am', 'am', 'nfm', 'nfm', 'usb', 'usb', 'lsb', 'lsb']
MIXED_MODES = ['am', 'am', 'nfm', 'nfm', 'usb', 'lsb', 'ft8', 'iq']
MIXED_TONES = TONES[:6] + [None, None]


def save(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + '\n')


def file_hash(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as source:
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def carrier_frequency(samples, rate, expected=2000):
    """Measure the source's isolated eighth-channel tone at the actual IQ rate."""
    assert len(samples) >= rate / 20 and np.isfinite(samples).all()
    spectrum = np.abs(np.fft.fft((samples - samples.mean()) * np.hanning(len(samples))))
    frequencies = np.fft.fftfreq(len(samples), 1 / rate)
    indices = np.flatnonzero((np.abs(frequencies) > 100) & (np.abs(frequencies - expected) < 2000))
    peak = int(indices[np.argmax(spectrum[indices])])
    assert spectrum[peak] > 30 * np.median(spectrum[indices]), 'No isolated RF reference tone'
    left, middle, right = np.log(np.maximum(spectrum[peak - 1:peak + 2], 1e-30))
    fraction = .5 * (left - right) / (left - 2 * middle + right)
    assert abs(fraction) <= .5
    return frequencies[peak] + fraction * rate / len(samples)


class DisplayConsumer:
    """Own a normal display subscription while configuring the demand-driven capture."""

    def __init__(self, origin):
        self.socket = connect(origin.replace('http', 'ws') + '/ws', max_size=4 * 1024 * 1024)
        self.finished = threading.Event()
        self.thread = threading.Thread(target=self.consume, daemon=True)
        self.thread.start()

    def consume(self):
        while not self.finished.is_set():
            try:
                self.socket.recv(timeout=.5)
            except TimeoutError:
                pass
            except Exception:
                break

    def close(self):
        self.finished.set()
        self.socket.close()
        self.thread.join(timeout=3)


class BenchRadio(Radio):
    """Keep the current sample rate/FIR and restore every setting changed by this source."""

    def __init__(self, artifacts, gain=-30, lo_correction=0, source_rate=None):
        super().__init__(SimpleNamespace(uri='ip:192.168.2.1', lo=CENTER + lo_correction, rate=0,
            gain=gain, xo_ppm=1.98, no_xo_trim=False), preserve_rate=True)
        self.artifacts, self.before = artifacts, None
        self.source_rate = source_rate
        self.stream_thread = None
        self.stream_stop = threading.Event()
        self.stream_error = None
        self.source_cycle_path = None
        self.carrier_thread = None
        self.carrier_stop = threading.Event()
        self.carrier_correction_hz = 0.0

    def set_sampling_clock(self, rate):
        # Bypass pyadi's sample_rate setter: it replaces the user's FIR taps.
        # The device clock attribute changes the rate with those taps retained.
        self.sdr._ctrl.find_channel('voltage0', False).attrs['sampling_frequency'].value = str(int(rate))
        self.rate = int(self.sdr.sample_rate)

    def restore_settings(self):
        if not self.before:
            return
        if int(self.sdr.sample_rate) != self.before['sample_rate']:
            self.set_sampling_clock(self.before['sample_rate'])
        self.sdr.tx_lo = self.before['tx_lo']
        self.sdr.tx_hardwaregain_chan0 = self.before['tx_hardwaregain_chan0']
        for key in ['tx_rf_bandwidth', 'rx_rf_bandwidth']:
            if getattr(self.sdr, key) != self.before[key]:
                setattr(self.sdr, key, self.before[key])
        restored = self.settings()
        save(self.artifacts / 'pluto-restored.json', restored)
        assert restored == self.before, (restored, self.before)

    def __enter__(self):
        try:
            return super().__enter__()
        except BaseException:
            self.restore_settings()
            raise

    def settings(self):
        return {**{key: getattr(self.sdr, key) for key in ['sample_rate', 'tx_lo',
            'tx_hardwaregain_chan0', 'tx_rf_bandwidth', 'rx_rf_bandwidth']},
            'xo_correction': read_xo_correction(self.sdr),
            'powerdown': self.sdr._ctrl.find_channel('altvoltage1', True).attrs['powerdown'].value,
            'fir': self.sdr._ctrl.attrs['filter_fir_config'].value}

    def configure(self):
        self.before = self.settings()
        save(self.artifacts / 'pluto-before.json', self.before)
        try:
            super().configure()
            if self.source_rate:
                self.set_sampling_clock(self.source_rate)
                assert self.settings()['fir'] == self.before['fir'], 'Source clock change replaced the saved FIR'
        except BaseException:
            self.sdr.tx_lo = self.before['tx_lo']
            self.sdr.tx_hardwaregain_chan0 = self.before['tx_hardwaregain_chan0']
            raise

    def __exit__(self, *exc):
        if self.carrier_thread:
            self.carrier_stop.set()
            self.carrier_thread.join(timeout=5)
            assert not self.carrier_thread.is_alive(), 'Source reference consumer did not stop'
        if self.stream_thread:
            self.stream_stop.set()
            self.stream_thread.join(timeout=15)
            assert not self.stream_thread.is_alive(), 'RF source push did not stop before buffer release'
        try:
            return super().__exit__(*exc)
        finally:
            try:
                self.restore_settings()
            finally:
                if self.source_cycle_path:
                    self.source_cycle_path.unlink(missing_ok=True)

    def check_stream(self):
        assert self.stream_error is None, self.stream_error

    def stream_cycle(self, signal):
        """Use the existing four-buffer transmitter at the entering source clock.

        The long cycle lives on disk, avoiding a device-sized cyclic allocation
        and a source clock/FIR change solely to fit that allocation.
        """
        self.open_stream(kernel_buffers=4)
        # Short blocks keep transmitter feedback latency below an FT8 symbol.
        chunk = round(self.rate / 100)
        ready = threading.Event()
        def transmit():
            try:
                position = submitted = 0
                phase = 0.0
                indices = np.arange(chunk, dtype=np.float64)
                previous_step, rotation = None, None
                while not self.stream_stop.is_set():
                    end = position + chunk
                    part = signal[position:end] if end <= len(signal) else np.concatenate(
                        (signal[position:], signal[:end - len(signal)]))
                    step = 2 * np.pi * self.carrier_correction_hz / self.rate
                    if phase or step:
                        # Feedback changes at IQ-reference cadence, slower than
                        # the TX buffers. Reuse the same phase ramp between
                        # adjustments instead of recomputing millions of exp()
                        # values each second on the receiver's benchmark host.
                        if step != previous_step:
                            rotation = np.exp(1j * (step * indices))
                            previous_step = step
                        part = (part * (np.exp(1j * phase) * rotation)).astype(np.complex64)
                    phase = (phase + step * chunk) % (2 * np.pi)
                    self.push_chunk(part)
                    position = (position + chunk) % len(signal)
                    submitted += 1
                    if submitted >= 4:
                        ready.set()
            except Exception as error:
                self.stream_error = repr(error)
                ready.set()
        self.stream_thread = threading.Thread(target=transmit, daemon=True)
        self.stream_thread.start()
        assert ready.wait(15), 'RF source stream did not start'
        self.check_stream()

    def track_carrier(self, origin, snapshot, *, reference_hz=2000):
        """Reference transmitted frequency to the acquired IQ8 tone, before RF decoding.

        Correct the source waveform with continuous phase, retaining all modulation
        and the device sample clock. Record every adjustment; decoder output never
        participates in this loop. The independent RF tone/text gates still apply.
        """
        assert not self.carrier_thread
        streamed = self.stream_thread is not None
        entering_lo = int(self.sdr.tx_lo)
        receiver = snapshot['receivers'][-1]
        assert receiver['display_number'] == 8
        ready = threading.Event()
        def track():
            try:
                route = origin.replace('http', 'ws') + '/ws/iq?receiver_uid=' + receiver['receiver_uid']
                with connect(route, max_size=4 * 1024 * 1024, max_queue=16, open_timeout=3, close_timeout=1) as socket, \
                        (self.artifacts / 'source-frequency-lock.jsonl').open('w') as log:
                    chunks, count, last_sequence = [], 0, None
                    while not self.carrier_stop.is_set():
                        packet = socket.recv(timeout=2)
                        if isinstance(packet, str):
                            event = json.loads(packet)
                            assert event['type'] == 'stream_start' and event['stream'] == receiver['stream']
                            continue
                        assert packet[:4] in [b'IQBF', b'IQAX']
                        sequence, size = struct.unpack_from('<II', packet, 8)
                        assert len(packet) == 56 + size * 8
                        if not size:
                            continue
                        identity = struct.unpack_from('<II', packet, 32)
                        assert identity == (int(receiver['stream']['channel_id']), int(receiver['stream']['epoch']))
                        assert last_sequence is None or sequence == last_sequence + 1
                        last_sequence = sequence
                        rate = struct.unpack_from('<d', packet, 16)[0]
                        chunks.append(np.frombuffer(packet, dtype='<c8', offset=56)); count += size
                        if count < rate / 10:
                            continue
                        measured = carrier_frequency(np.concatenate(chunks), rate, reference_hz)
                        error = measured - reference_hz
                        correction = self.carrier_correction_hz - .4 * error
                        assert abs(error) < 2000 and abs(correction) < 2000, 'Source frequency escaped the reference range'
                        self.carrier_correction_hz = correction
                        if not streamed:
                            # The short analog cyclic source needs no host TX
                            # arithmetic. Its hardware LO supplies the reference
                            # correction; source phase is not claimed continuous.
                            corrected_lo = round(entering_lo + correction)
                            if int(self.sdr.tx_lo) != corrected_lo:
                                self.sdr.tx_lo = corrected_lo
                        log.write(json.dumps({'monotonic_seconds': time.monotonic(), 'rate_hz': rate,
                            'measured_hz': measured, 'reference_hz': reference_hz,
                            'error_hz': error, 'source_correction_hz': correction,
                            'last_sequence': sequence, 'stream': receiver['stream'],
                            'correction_kind': 'continuous_phase_waveform' if streamed else 'source_hardware_lo',
                            'tx_lo_hz': int(self.sdr.tx_lo)}) + '\n'); log.flush()
                        chunks, count = [], 0
                        ready.set()
            except Exception as error:
                if not self.carrier_stop.is_set():
                    self.stream_error = 'Source frequency reference: ' + repr(error)
                    save(self.artifacts / 'source-frequency-error.json', {
                        'error': repr(error), 'traceback': traceback.format_exc(),
                        'monotonic_seconds': time.monotonic()})
                ready.set()
        self.carrier_thread = threading.Thread(target=track, daemon=True)
        self.carrier_thread.start()
        assert ready.wait(10), 'Source frequency reference did not start'
        self.check_stream()
        source = json.loads((self.artifacts / 'source.json').read_text())
        source['frequency_reference'] = ('phase-continuous transmit correction' if streamed else
            'cyclic analog source hardware LO correction, without phase continuity guarantee') + ' from acquired IQ8 tone; all adjustments logged'
        source['reference_hz'] = reference_hz
        save(self.artifacts / 'source.json', source)

    def narrow(self, *, streaming=False):
        # The short period is continuous in either the hardware cyclic buffer or
        # the existing streaming path, which permits transmit-only carrier trim.
        count = round(self.rate / 10)
        phase = np.arange(count, dtype=np.float64) * (2 * np.pi / count)
        signal = np.zeros(count, dtype=np.complex64)
        actual = []
        for frequency, tone, mode in zip(FREQUENCIES, TONES, MODES, strict=True):
            rf_cycles = round((frequency - CENTER) * count / self.rate)
            tone_cycles = round(tone * count / self.rate)
            modulation = tone_cycles * phase
            carrier = np.exp(1j * rf_cycles * phase)
            if mode == 'am':
                component = (1 + .5 * np.cos(modulation)) * carrier
            elif mode == 'nfm':
                component = np.exp(1j * (rf_cycles * phase + 2500 / tone * np.sin(modulation)))
            else:
                component = np.exp(1j * (rf_cycles + (tone_cycles if mode == 'usb' else -tone_cycles)) * phase)
            signal += (1536 * component).astype(np.complex64)
            actual.append({'frequency_hz': CENTER + rf_cycles * self.rate / count,
                'commanded_frequency_hz': self.sdr.tx_lo + rf_cycles * self.rate / count,
                'tone_hz': tone_cycles * self.rate / count, 'mode': mode})
        assert max(np.max(np.abs(signal.real)), np.max(np.abs(signal.imag))) < 32767
        if streaming:
            self.stream_cycle(signal)
        else:
            self.push_cyclic(signal)
        save(self.artifacts / 'source.json', {'source_kind': 'Pluto RF', 'settings': self.settings(),
            'lo_correction_hz': self.args.lo - CENTER,
            'channels': actual, 'samples': count, 'streaming': streaming,
            'peak': float(np.max(np.abs(signal))),
            'sha256_cf32': hashlib.sha256(signal.tobytes()).hexdigest()})

    def mixed(self):
        from pluto_ft8_tx import load_encoder, tones_for_message
        message = 'CQ WE0BSA PM95'
        tones = np.asarray(tones_for_message(load_encoder(), message))
        symbol_samples = round(self.rate * .160)
        steps = 2 * np.pi * (1500 + tones * 6.25) / self.rate
        starts = np.r_[0, np.cumsum(steps[:-1] * symbol_samples)]
        count = round(self.rate * 15)
        self.source_cycle_path = self.artifacts / 'mixed-source.cf32'
        signal = np.memmap(self.source_cycle_path, mode='w+', dtype='<c8', shape=(count,))
        peak = 0.0
        digest = hashlib.sha256()
        for begin in range(0, count, 250000):
            end = min(count, begin + 250000)
            indices = np.arange(begin, end, dtype=np.float64)
            phase = indices * (2 * np.pi / self.rate)
            block = signal[begin:end]
            for slot, mode in enumerate(MIXED_MODES):
                offset = FREQUENCIES[slot] - CENTER
                if mode == 'ft8':
                    valid = indices < len(tones) * symbol_samples
                    at = indices[valid]
                    symbols = (at // symbol_samples).astype(int)
                    waveform = np.exp(1j * (offset * phase[valid] + starts[symbols]
                        + steps[symbols] * (at - symbols * symbol_samples)))
                    ramp = np.minimum(1, np.minimum(at, len(tones) * symbol_samples - 1 - at) / (.005 * self.rate))
                    block[valid] += (1536 * waveform * (.5 - .5 * np.cos(np.pi * ramp))).astype(np.complex64)
                else:
                    tone = TONES[slot] if mode != 'iq' else 2000
                    modulation = tone * phase
                    if mode == 'am':
                        waveform = (1 + .5 * np.cos(modulation)) * np.exp(1j * offset * phase)
                    elif mode == 'nfm':
                        waveform = np.exp(1j * (offset * phase + 2500 / tone * np.sin(modulation)))
                    else:
                        waveform = np.exp(1j * (offset + (tone if mode in ['usb', 'iq'] else -tone)) * phase)
                    block += (1536 * waveform).astype(np.complex64)
            peak = max(peak, float(np.max(np.abs(block.real))), float(np.max(np.abs(block.imag))))
            digest.update(block.tobytes())
        assert peak < 32767
        source = {'source_kind': 'Pluto RF mixed streaming', 'settings': self.settings(),
            'channels': [{'frequency_hz': frequency, 'mode': mode} for frequency, mode in zip(FREQUENCIES, MIXED_MODES, strict=True)],
            'expected_ft8': message, 'tones': tones.tolist(), 'samples': count, 'seconds': count / self.rate,
            'peak': peak, 'sha256_cf32': digest.hexdigest()}
        time.sleep(15 - time.time() % 15)
        source['push_started_utc_seconds'] = time.time()
        signal.flush()
        self.stream_cycle(signal)
        source['push_returned_utc_seconds'] = time.time()
        save(self.artifacts / 'source.json', source)


class HardwareSession:
    """Stop the recorded SDK owner and restore its original executable and configuration."""

    def __init__(self, artifacts, origin, service_id, *, launch=True, binary=None):
        self.artifacts, self.original, self.service_id = artifacts, origin, service_id
        self.launch = launch
        self.binary = binary
        self.process, self.log, self.before, self.before_state = None, None, None, None
        self.released = False
        self.saved_env = None
        self.consumer = None

    def __enter__(self):
        self.before, self.before_state = api(self.original), post(self.original, {'cmd': 'STATUS'})
        assert self.before['service_id'] == self.service_id
        assert self.before_state['mode'] == 'sdr' and self.before_state['server']['state'] == 'stopped'
        assert len(self.before['receivers']) == 1, 'This restoration path requires the saved single-receiver setup'
        assert not [r for r in self.before['recordings'] if r['state'] not in ['completed', 'failed']]
        save(self.artifacts / 'original-receivers.json', self.before)
        save(self.artifacts / 'original-state.json', self.before_state)
        try:
            pid = int(self.service_id.split('-')[0])
            runtime = Path('/run/user') / str(os.getuid()) / 'harogic-websa'
            owners = list(runtime.glob('*/websad.state'))
            assert len(owners) == 1 and owners[0].read_text().splitlines()[0] == str(pid)
            self.saved_env = dict(item.decode().split('=', 1) for item in
                Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if item)
            shim = os.environ.get('WEBSA_RUST_IQS_SHIM') or self.saved_env.get('WEBSA_RUST_IQS_SHIM')
            assert shim and Path(shim).is_file(), 'The current SDK adapter must exist before stopping its owner'
            os.environ['WEBSA_RUST_IQS_SHIM'] = shim
            self.old_binary = self.artifacts / 'original-bin/websad'
            self.old_binary.parent.mkdir()
            shutil.copyfile(f'/proc/{pid}/exe', self.old_binary)
            self.old_binary.chmod(0o700)
            subprocess.run([str(ROOT / 'stop.sh')], cwd=ROOT, check=True)
            self.released = True
            if not self.launch:
                return self
            os.environ['WEBSA_RECORDING_DIR'] = str((self.artifacts / 'recordings').resolve())
            self.log = (self.artifacts / 'service.log').open('w')
            self.process, self.origin = start_service(free_port(), hardware=True, log=self.log, binary=self.binary)
            self.consumer = DisplayConsumer(self.origin)
            until(lambda: state(self.origin)['connected'], timeout=30)
            libraries = sorted({line.split()[-1] for line in Path(f'/proc/{self.process.pid}/maps').read_text().splitlines()
                if any(name in line for name in ['htraapi', 'libusb', 'libliquid'])})
            save(self.artifacts / 'build.json', {
                'git_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
                'tracked_diff_sha256': hashlib.sha256(subprocess.check_output(['git', 'diff', '--binary'], cwd=ROOT)).hexdigest(),
                'backend_sha256': file_hash(f'/proc/{self.process.pid}/exe'),
                'adapter': {'path': shim, 'sha256': file_hash(shim)},
                'libraries': [{'path': path, 'sha256': file_hash(path)} for path in libraries],
                'preload': os.environ.get('LD_PRELOAD')})
            return self
        except BaseException:
            self.__exit__(*sys.exc_info())
            raise

    def __exit__(self, *exc):
        # STATUS builds a fresh snapshot; /api/state can still contain the last
        # healthy publication when the capture has already terminated.
        if self.process and self.process.poll() is None:
            for name, read in [('receivers', lambda: api(self.origin)),
                    ('state', lambda: post(self.origin, {'cmd': 'STATUS'}))]:
                try:
                    save(self.artifacts / f'test-final-{name}.json', read())
                except Exception as error:
                    save(self.artifacts / f'test-final-{name}-error.json', {'error': str(error)})
        if exc and exc[0]:
            save(self.artifacts / 'test-exception.json', {'type': exc[0].__name__, 'message': str(exc[1])})
        if self.consumer:
            self.consumer.close()
        stop(self.process)
        if self.log:
            self.log.close()
        if self.released:
            restore_env = {**self.saved_env, 'WEBSA_BIN': str(self.old_binary)}
            subprocess.run([str(ROOT / 'run.sh'), '--port', str(self.saved_env['WEBSA_PORT'])],
                cwd=ROOT, env=restore_env, check=True)
            # The original binary and startup environment are restored, with a new service generation.
            consumer = DisplayConsumer(self.original)
            until(lambda: state(self.original)['connected'], timeout=30)
            post(self.original, {'cmd': 'SET_MODE', 'mode': self.before_state['mode']})
            sdr = self.before_state['req']['sdr']
            post(self.original, {'cmd': 'SET_SDR', **{k: sdr[k] for k in
                ['center', 'listen', 'decimate', 'pfb_taps_per_phase']}})
            post(self.original, {'cmd': 'SET_REF', 'ref': sdr['ref']})
            post(self.original, {'cmd': 'SET_AMP', **{k: self.before_state['amp'][k] for k in
                ['atten', 'preamp', 'ifgain', 'gain_strategy']}})
            post(self.original, {'cmd': 'SET_REFCK', 'mode': self.before_state['ref_clock']})
            post(self.original, {'cmd': 'SET_REFCKOUT', 'on': self.before_state['refclk_out']})
            until(lambda: api(self.original)['capture']['state'] == 'running', timeout=30)
            current = api(self.original)
            assert len(current['receivers']) == len(self.before['receivers']) == 1
            old, new = self.before['receivers'][0], current['receivers'][0]
            reply = api(self.original, payload={'type': 'configure', 'request_id': 'restore-original',
                'receiver_uid': new['receiver_uid'], 'expected_revision': new['revision'], 'config': old['confirmed']})
            assert reply['fault'] is None, reply['fault']
            # Disabled compatibility slots retain user settings despite having
            # no logical receiver definition. Restore those too.
            for channel in self.before_state['sdr']['channels']:
                if channel['channel'] == old['display_number']:
                    continue
                assert not channel['enabled'], 'Single-receiver restoration cannot replace an active auxiliary UID'
                data = {'cmd': 'SET_SDR_CHANNEL', **channel}
                data.pop('am_sync_generation', None)
                result = post(self.original, data)
                assert result.get('cmd') != 'ERROR', result
            restored = api(self.original)
            save(self.artifacts / 'original-restored.json', restored)
            assert [r['confirmed'] for r in restored['receivers']] == [r['confirmed'] for r in self.before['receivers']]
            restored_state = post(self.original, {'cmd': 'STATUS'})
            save(self.artifacts / 'original-state-restored.json', restored_state)
            assert restored_state['req']['sdr'] == self.before_state['req']['sdr']
            assert restored_state['ref_clock'] == self.before_state['ref_clock']
            assert restored_state['refclk_out'] == self.before_state['refclk_out']
            assert {row['task_id'] for row in restored['recordings']} == {row['task_id'] for row in self.before['recordings']}, 'Historical recordings changed during test ownership'
            consumer.close()
        return False
