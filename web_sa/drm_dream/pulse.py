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

import re
import subprocess


class PulseError(RuntimeError):
    pass


def unload_stale_sinks(pactl: str, prefixes: tuple[str, ...]) -> list[str]:
    """Unload private null sinks left behind by a killed decoder process.

    The worker exits with ``os._exit`` on SIGTERM, which skips the decoder's own cleanup; the
    child processes are killed via PR_SET_PDEATHSIG, but the sink modules are created by
    ``pactl`` and would otherwise accumulate. Names are namespaced (``drm_dream_*``), so only
    our own leftover modules can match.
    """
    proc = subprocess.run([pactl, 'list', 'short', 'modules'], capture_output=True, text=True)
    unloaded: list[str] = []
    for line in proc.stdout.splitlines():
        parts = line.split('\t')
        if len(parts) < 2 or parts[1] != 'module-null-sink':
            continue
        args = parts[2] if len(parts) > 2 else ''
        match = re.search(r'sink_name=(\S+)', args)
        if match and any(match.group(1).startswith(p) for p in prefixes):
            subprocess.run([pactl, 'unload-module', parts[0]], capture_output=True)
            unloaded.append(match.group(1))
    return unloaded


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
