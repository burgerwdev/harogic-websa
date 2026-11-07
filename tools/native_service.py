"""Launch an owned native test service and reject unrelated listeners on its port."""
from __future__ import annotations

import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path


def _owns_listener(pid: int, port: int) -> bool:
    """Match the Linux listening socket to this process, including bind races."""
    process = Path(f"/proc/{pid}")
    try:
        sockets = set()
        for descriptor in (process / "fd").iterdir():
            try:
                target = descriptor.readlink().as_posix()
            except OSError:
                continue  # Descriptors can close while the service starts.
            if target.startswith("socket:[") and target.endswith("]"):
                sockets.add(target[8:-1])
        for line in (process / "net/tcp").read_text().splitlines()[1:]:
            fields = line.split()
            if len(fields) < 10 or fields[3] != "0A" or fields[9] not in sockets:
                continue
            address, encoded_port = fields[1].split(":")
            if address in ("0100007F", "00000000") and int(encoded_port, 16) == port:
                return True
    except (OSError, ValueError):
        pass
    return False


def _reap_failed_start(process) -> None:
    if process.poll() is None:
        try:
            process.terminate()
        except ProcessLookupError:
            pass
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        try:
            process.kill()
        except ProcessLookupError:
            pass
        process.wait(timeout=3)


def start_owned_service(command, *, port, cwd, env, stdout, stderr,
                        startup_timeout=10.0, preexec_fn=None):
    """Return only after this child owns the answering IPv4 loopback listener.

    Callers choose fixture/hardware settings and retain normal shutdown ownership.
    This helper owns failure cleanup until startup succeeds. The optional child
    setup preserves resource limits used by existing recording fault tests.
    """
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as available:
        available.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            available.bind(("127.0.0.1", port))
        except OSError as exc:
            raise RuntimeError(f"native service port {port} is unavailable") from exc
    process = subprocess.Popen(command, cwd=cwd, env=env, stdout=stdout, stderr=stderr,
                               preexec_fn=preexec_fn)
    origin = f"http://127.0.0.1:{port}"
    try:
        deadline = time.monotonic() + startup_timeout
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError(f"native service exited during startup: {process.returncode}")
            if _owns_listener(process.pid, port):
                try:
                    with urllib.request.urlopen(origin + "/api/state", timeout=1):
                        if process.poll() is None and _owns_listener(process.pid, port):
                            return process, origin
                except OSError:
                    pass
            time.sleep(0.1)
        raise RuntimeError("native service did not become ready; inspect the service log")
    except BaseException:
        try:
            _reap_failed_start(process)
        except Exception as cleanup_error:
            print(f"native service startup cleanup failed: {cleanup_error}", file=sys.stderr)
        raise
