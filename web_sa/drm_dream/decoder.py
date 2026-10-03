"""Background Dream DRM decoder: subprocess lifecycle + baseband feed + status/audio.

The decoder owns three cooperating processes and two private PulseAudio sinks:

    SDR DDC (i, q) --feed()--> queue -> pacat -> [feed sink] -> Dream -I monitor
    Dream -O [audio sink] -> parec -> decoded PCM (mono int16)
    Dream --status-socket -> newline-delimited JSON -> metadata

Dream's own ``-w/--writewav`` was not usable (the file stayed at 0 bytes even
while the receiver decoded audio), so decoded audio is captured from Dream's
output device with ``parec`` instead. See ``tools/drm_dream/SPIKE.md``.

``feed()`` is non-blocking on purpose: the SDR step loop must never stall on a
full pipe, so the queue drops the oldest block and counts the loss.
"""
from __future__ import annotations

import logging
import os
import queue
import socket
import subprocess
import threading
import time
from collections import deque

import numpy as np

from .pulse import PulseSink
from .status import extract_metadata, parse_status_line

log = logging.getLogger(__name__)

#: Dream I/Q channel select: positive I/Q with the DRM signal at 0 Hz IF.
IQ_CHANNEL_SELECT = '6'


class DecoderError(RuntimeError):
    """Raised when the decoder cannot be started or the input cannot be fed."""


class DreamProcessError(DecoderError):
    """The dream subprocess exited unexpectedly."""


def _silence_proc(proc: subprocess.Popen | None, timeout: float = 3.0) -> None:
    if proc is None or proc.poll() is not None:
        return
    try:
        proc.terminate()
        proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        try:
            proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            log.error('process %s did not die after SIGKILL', proc.pid)


class DreamDecoder:
    def __init__(self, *, dream_bin: str, sample_rate: int = 48000,
                 sink_name: str = 'drm_dream_feed', status_socket: str = '/tmp/drm-dream-status.sock',
                 audio_sink_name: str = 'drm_dream_audio', capture_audio: bool = True,
                 input_file: str | None = None,
                 pactl: str = 'pactl', pacat: str = 'pacat', parec: str = 'parec',
                 manage_pulse: bool = True, queue_blocks: int = 64,
                 audio_buffer_seconds: float = 4.0):
        self.dream_bin = dream_bin
        self.sample_rate = int(sample_rate)
        self.status_socket = status_socket
        self.input_file = input_file
        self.pactl = pactl
        self.pacat = pacat
        self.parec = parec
        self.capture_audio = bool(capture_audio)
        self._manage_pulse = bool(manage_pulse)
        self._sink = PulseSink(sink_name, rate=self.sample_rate, channels=2,
                               pactl=pactl, description='drm-dream-feed')
        self._audio_sink = PulseSink(audio_sink_name, rate=self.sample_rate, channels=2,
                                     pactl=pactl, description='drm-dream-audio')

        self._dream: subprocess.Popen | None = None
        self._feeder: subprocess.Popen | None = None
        self._capture: subprocess.Popen | None = None
        self._stop = threading.Event()
        self._threads: list[threading.Thread] = []
        self._queue: queue.Queue[bytes] = queue.Queue(maxsize=max(1, int(queue_blocks)))
        self._lock = threading.Lock()

        self._status: dict | None = None
        self._status_at = 0.0
        self._audio = deque()
        self._audio_samples = 0
        self._audio_max = int(audio_buffer_seconds * self.sample_rate)
        self.dropped_blocks = 0
        self.status_events = 0
        self.audio_samples = 0
        self.last_error: str | None = None

    # ---------------- lifecycle ----------------
    @property
    def alive(self) -> bool:
        return self._dream is not None and self._dream.poll() is None

    @property
    def pid(self) -> int | None:
        return self._dream.pid if self._dream is not None else None

    def start(self) -> None:
        if self._dream is not None:
            return
        self._stop.clear()
        try:
            if self._manage_pulse:
                if self.input_file is None:
                    self._sink.create()
                if self.capture_audio:
                    self._audio_sink.create()
            try:
                os.unlink(self.status_socket)
            except FileNotFoundError:
                pass

            self._feeder = None
            if self.input_file is None and self._manage_pulse:
                self._feeder = subprocess.Popen(
                    [self.pacat, '--raw', f'--rate={self.sample_rate}', '--channels=2',
                     '--format=s16le', f'--device={self._sink.name}'],
                    stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

            if self.input_file is not None:
                cmd = [self.dream_bin, '-f', self.input_file, '-c', IQ_CHANNEL_SELECT,
                       '--sigsrate', str(self.sample_rate),
                       '-m', '0' if self.capture_audio else '1',
                       '--status-socket', self.status_socket]
            else:
                cmd = [self.dream_bin, '-I', self._sink.monitor, '-c', IQ_CHANNEL_SELECT,
                       '--sigsrate', str(self.sample_rate),
                       '-m', '0' if self.capture_audio else '1',
                       '--status-socket', self.status_socket]
            if self.capture_audio and self._manage_pulse:
                cmd += ['-O', self._audio_sink.name]
            self._dream = subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                                           stderr=subprocess.DEVNULL)

            if self.capture_audio and self._manage_pulse:
                self._capture = subprocess.Popen(
                    [self.parec, f'--device={self._audio_sink.monitor}',
                     f'--rate={self.sample_rate}', '--channels=2', '--format=s16le'],
                    stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

            if self._feeder is not None:
                self._spawn(self._writer_loop, 'drm-dream-writer')
            self._spawn(self._status_loop, 'drm-dream-status')
            if self._capture is not None:
                self._spawn(self._audio_loop, 'drm-dream-audio')
        except Exception:
            self.stop()
            raise
        log.info('DRM decoder started (dream=%s pid=%s sink=%s)', self.dream_bin,
                 self.pid, self._sink.name)

    def _spawn(self, target, name: str) -> None:
        thread = threading.Thread(target=target, name=name, daemon=True)
        thread.start()
        self._threads.append(thread)

    def stop(self) -> None:
        self._stop.set()
        if self._feeder is not None and self._feeder.stdin is not None:
            try:
                self._feeder.stdin.close()
            except OSError:
                pass
        for proc in (self._dream, self._capture, self._feeder):
            _silence_proc(proc)
        self._dream = None
        self._capture = None
        self._feeder = None
        for thread in self._threads:
            thread.join(timeout=2.0)
        self._threads.clear()
        if self._manage_pulse:
            self._audio_sink.destroy()
            self._sink.destroy()
        try:
            os.unlink(self.status_socket)
        except FileNotFoundError:
            pass
        with self._lock:
            self._status = None

    def restart(self) -> None:
        self.stop()
        self._audio.clear()
        self._audio_samples = 0
        self.dropped_blocks = 0
        self.audio_samples = 0
        self.last_error = None
        self.start()

    def close(self) -> None:
        self.stop()

    def __enter__(self) -> DreamDecoder:
        self.start()
        return self

    def __exit__(self, *exc) -> None:
        self.stop()

    # ---------------- feed ----------------
    def feed(self, i, q) -> int:
        """Queue one block of complex baseband (mono float arrays) for Dream.

        Returns the number of complex samples accepted. Never blocks: on a full
        queue the oldest block is dropped, so a slow decoder cannot stall the SDR
        step loop.
        """
        i = np.asarray(i)
        if i.size == 0:
            return 0
        q = np.asarray(q)
        inter = np.empty(i.size * 2, dtype=np.int16)
        inter[0::2] = np.clip(i, -1.0, 1.0) * 32767.0
        inter[1::2] = np.clip(q, -1.0, 1.0) * 32767.0
        payload = inter.tobytes()
        try:
            self._queue.put_nowait(payload)
        except queue.Full:
            try:
                self._queue.get_nowait()
                self._queue.put_nowait(payload)
            except (queue.Empty, queue.Full):
                pass
            self.dropped_blocks += 1
        return int(i.size)

    def _writer_loop(self) -> None:
        while not self._stop.is_set():
            try:
                payload = self._queue.get(timeout=0.2)
            except queue.Empty:
                continue
            stdin = self._feeder.stdin if self._feeder is not None else None
            if stdin is None:
                continue
            try:
                stdin.write(payload)
                stdin.flush()
            except (BrokenPipeError, ValueError, OSError) as exc:
                self.last_error = f'feed pipe closed: {exc}'
                if not self._stop.is_set():
                    log.warning('DRM feed pipe closed: %s', exc)
                return

    # ---------------- status / audio ----------------
    def _status_loop(self) -> None:
        deadline = time.monotonic() + 10.0
        sock: socket.socket | None = None
        while not self._stop.is_set() and sock is None and time.monotonic() < deadline:
            if os.path.exists(self.status_socket):
                try:
                    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                    sock.connect(self.status_socket)
                except OSError:
                    sock = None
            if sock is None:
                time.sleep(0.1)
        if sock is None:
            if not self._stop.is_set():
                self.last_error = 'dream status socket not available'
            return
        sock.settimeout(1.0)
        buf = ''
        try:
            while not self._stop.is_set():
                try:
                    chunk = sock.recv(16384)
                except TimeoutError:
                    continue
                except OSError:
                    break
                if not chunk:
                    break
                buf += chunk.decode('utf-8', 'ignore')
                while '\n' in buf:
                    line, buf = buf.split('\n', 1)
                    parsed = parse_status_line(line)
                    if parsed is not None:
                        with self._lock:
                            self._status = parsed
                            self._status_at = time.monotonic()
                        self.status_events += 1
        finally:
            sock.close()

    def _audio_loop(self) -> None:
        stdout = self._capture.stdout if self._capture is not None else None
        if stdout is None:
            return
        while not self._stop.is_set():
            try:
                chunk = stdout.read(960 * 2 * 2)   # ~20 ms of stereo int16
            except (OSError, ValueError):
                break
            if not chunk:
                break
            pcm = np.frombuffer(chunk, dtype='<i2')
            if pcm.size < 2:
                continue
            pcm = pcm[:pcm.size - (pcm.size % 2)]
            mono = ((pcm[0::2].astype(np.int32) + pcm[1::2].astype(np.int32)) // 2).astype(np.int16)
            self._audio.append(mono)
            self._audio_samples += mono.size
            self.audio_samples = self._audio_samples
            while self._audio_samples > self._audio_max and self._audio:
                dropped = self._audio.popleft()
                self._audio_samples -= dropped.size

    def status(self) -> dict | None:
        with self._lock:
            return self._status

    def status_age(self) -> float:
        with self._lock:
            return time.monotonic() - self._status_at if self._status is not None else -1.0

    def metadata(self) -> dict:
        return extract_metadata(self.status())

    def drain_audio(self) -> np.ndarray:
        """Return and clear the buffered decoded PCM (mono int16)."""
        if not self._audio:
            return np.zeros(0, dtype=np.int16)
        parts = list(self._audio)
        self._audio.clear()
        self._audio_samples = 0
        return np.concatenate(parts) if len(parts) > 1 else parts[0]
