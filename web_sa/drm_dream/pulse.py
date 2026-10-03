"""Thin PulseAudio/PipeWire helpers for the Dream feed and audio capture.

The SDR pipeline stays in-process; Dream is an external decoder that only reads
from a sound card. A private ``module-null-sink`` per direction gives each side a
stable device without touching the user's real audio routing:

    Python SDR --pacat--> [feed sink] --> Dream (-I <sink>.monitor)
    Dream (-O <audio sink>) --> [audio sink] --parec--> Python (decoded PCM)

Every sink name is namespaced / configurable so this worktree can never collide
with another instance.
"""
from __future__ import annotations

import subprocess


class PulseError(RuntimeError):
    pass


class PulseSink:
    """A private null sink, created with ``pactl`` and removed on :meth:`destroy`."""

    def __init__(self, name: str, rate: int = 48000, channels: int = 2,
                 pactl: str = 'pactl', description: str | None = None):
        self.name = name
        self.rate = int(rate)
        self.channels = int(channels)
        self.pactl = pactl
        self.description = description or name
        self.module: str | None = None

    @property
    def monitor(self) -> str:
        return f'{self.name}.monitor'

    def create(self) -> str:
        if self.module is not None:
            return self.module
        proc = subprocess.run(
            [self.pactl, 'load-module', 'module-null-sink', f'sink_name={self.name}',
             f'rate={self.rate}', f'channels={self.channels}',
             f'sink_properties=device.description={self.description}'],
            capture_output=True, text=True)
        if proc.returncode != 0:
            raise PulseError(f'failed to create sink {self.name}: {proc.stderr.strip()}')
        self.module = proc.stdout.strip()
        # Best-effort: the sink defaults to full volume, but state can be restored
        # by a session manager after creation.
        subprocess.run([self.pactl, 'set-sink-volume', self.name, '100%'],
                       capture_output=True)
        subprocess.run([self.pactl, 'set-sink-mute', self.name, '0'], capture_output=True)
        return self.module

    def destroy(self) -> None:
        if self.module is None:
            return
        subprocess.run([self.pactl, 'unload-module', self.module], capture_output=True)
        self.module = None
