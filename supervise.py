#!/usr/bin/env python3
"""Checkout-scoped daemon monitor; restart failures without requiring systemd."""
from __future__ import annotations

import fcntl
import os
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path


def identity(pid: int) -> str:
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(') ', 1)[1].split()
    if fields[0] in ('Z', 'X'):
        raise ProcessLookupError(pid)
    return fields[19]


def inode(path: Path) -> str:
    stat = path.stat()
    return f'{stat.st_dev}:{stat.st_ino}'


def atomic_lines(path: Path, values: list[str]) -> None:
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, prefix='.monitor-', delete=False) as out:
        temporary = Path(out.name)
        try:
            out.write('\n'.join(values) + '\n')
            out.close()
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


def stop_child(child: subprocess.Popen) -> None:
    if child.poll() is not None:
        return
    child.terminate()
    try:
        child.wait(timeout=4)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()


def main() -> None:
    state = Path(sys.argv[1])
    binary, host, port, fake = sys.argv[2:]
    root = str(Path.cwd())
    boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    stopped = threading.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_: stopped.set())
    monitor = state.with_suffix('.monitor')
    atomic_lines(monitor, [str(os.getpid()), identity(os.getpid()), boot,
                           inode(Path('/proc/self/exe')), root])
    child = None
    delay = 1
    try:
        while not stopped.is_set():
            started = time.monotonic()
            try:
                # Lifecycle commands hold this lock: never race stop or another
                # launch when replacing the child and its ownership record.
                with state.with_suffix('.lock').open('a') as lock:
                    while not stopped.is_set():
                        try:
                            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                            break
                        except BlockingIOError:
                            stopped.wait(.05)
                    if stopped.is_set():
                        break
                    child = subprocess.Popen([binary], stdin=subprocess.DEVNULL)
                    atomic_lines(state, [str(child.pid), identity(child.pid), boot,
                                         binary, inode(Path(f'/proc/{child.pid}/exe')),
                                         root, host, port, fake])
                while child.poll() is None and not stopped.wait(.2):
                    pass
                if stopped.is_set():
                    break
                status = child.wait()
                child = None
                if status == 0:
                    break
                if time.monotonic() - started >= 60:
                    delay = 1
                print(f'websad monitor: child exited status={status}; retry in {delay}s', flush=True)
            except (OSError, ValueError, MemoryError) as error:
                # exec/allocation failures under resource pressure must remain
                # retryable too, with no accumulating child processes.
                if child is not None:
                    stop_child(child)
                    child = None
                print(f'websad monitor: {error}; retry in {delay}s', flush=True)
            if stopped.wait(delay):
                break
            delay = min(delay * 2, 30)
    finally:
        if child is not None:
            stop_child(child)
        monitor.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
