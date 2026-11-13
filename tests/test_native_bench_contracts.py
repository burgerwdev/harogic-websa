"""Native bench evidence must belong to its process, frame axis and message cycle."""
import os
import socket
import struct
import subprocess
from contextlib import nullcontext
from types import SimpleNamespace

import numpy as np
import pytest

from tools import native_service
from tools.bench import rust_harm_pnm_round as harm
from tools.bench import rust_swp_mode_round as mode
from tools.bench import rust_tinysa_pnm_round as tinysa


class Clock:
    now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += max(seconds, 0.1)


class Process:
    def __init__(self, exited=False, stuck=False):
        self.pid = os.getpid()
        self.returncode = 2 if exited else None
        self.stuck = stuck
        self.terminated = self.killed = self.waited = 0

    def poll(self):
        return self.returncode

    def terminate(self):
        self.terminated += 1

    def kill(self):
        self.killed += 1
        self.returncode = -9

    def wait(self, timeout=None):
        self.waited += 1
        if self.stuck and not self.killed:
            raise subprocess.TimeoutExpired("fake-websad", timeout)
        self.returncode = self.returncode if self.returncode is not None else -15
        return self.returncode


@pytest.fixture
def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


@pytest.fixture
def immediate_startup(monkeypatch):
    monkeypatch.setattr(mode, "time", Clock())
    monkeypatch.setattr(native_service, "time", Clock())
    monkeypatch.setattr(mode.urllib.request, "urlopen", lambda *_a, **_k: nullcontext())


def test_bench_start_rejects_already_listening_service(monkeypatch, tmp_path, immediate_startup):
    proc = Process(exited=True)
    spawned = []
    monkeypatch.setattr(mode.subprocess, "Popen", lambda *_a, **_k: spawned.append(proc) or proc)
    with socket.socket() as unrelated:
        unrelated.bind(("127.0.0.1", 0))
        unrelated.listen()
        with pytest.raises(SystemExit):
            mode.start_websad(unrelated.getsockname()[1], tmp_path / "websad.log", hardware=False)
    assert not spawned or proc.waited, "a launched process must be reaped on rejection"


def test_bench_start_rejects_dead_child_even_when_http_answers(
        monkeypatch, tmp_path, free_port, immediate_startup):
    proc = Process(exited=True)
    monkeypatch.setattr(mode.subprocess, "Popen", lambda *_a, **_k: proc)
    with pytest.raises(SystemExit):
        mode.start_websad(free_port, tmp_path / "websad.log", hardware=False)
    assert proc.waited


def test_bench_start_reaps_timed_out_child(monkeypatch, tmp_path, free_port, immediate_startup):
    proc = Process(stuck=True)
    monkeypatch.setattr(mode.subprocess, "Popen", lambda *_a, **_k: proc)

    def refused(*_args, **_kwargs):
        raise ConnectionRefusedError("not ready")

    monkeypatch.setattr(mode.urllib.request, "urlopen", refused)
    with pytest.raises(SystemExit):
        mode.start_websad(free_port, tmp_path / "websad.log", hardware=False)
    assert (proc.terminated, proc.killed, proc.waited) == (1, 1, 2)


@pytest.mark.parametrize("owns_listener", [False, True])
def test_bench_start_requires_child_to_own_the_answering_socket(
        monkeypatch, tmp_path, free_port, immediate_startup, owns_listener):
    proc = Process()
    if not owns_listener:
        proc.pid = 2 ** 30  # Deliberately absent from /proc.
    with socket.socket() as listener:
        def spawn(*_args, **_kwargs):
            listener.bind(("127.0.0.1", free_port))
            listener.listen()
            return proc

        monkeypatch.setattr(mode.subprocess, "Popen", spawn)
        if owns_listener:
            returned, url = mode.start_websad(free_port, tmp_path / "websad.log", hardware=False)
            assert returned is proc and url == f"http://127.0.0.1:{free_port}"
            assert not proc.terminated and not proc.waited
        else:
            with pytest.raises(SystemExit):
                mode.start_websad(free_port, tmp_path / "websad.log", hardware=False)
            assert proc.terminated and proc.waited


@pytest.mark.parametrize("magic", [b"RTAF", b"RTA2", b"RTA3"])
def test_bench_display_uses_the_explicit_vendor_axis(monkeypatch, magic):
    axis = np.array([100_000_000.0, 100_000_250.5, 100_001_000.0], dtype="<f8")
    power = np.array([-90.0, -20.0, -80.0], dtype="<f4")
    frame = (struct.pack("<4sIIHHd", magic, 7, 3, 0, 4095, 99_999_000.0)
             + axis.tobytes() + power.tobytes() + struct.pack("<d", 100_002_000.0))

    class Socket:
        def recv(self, **_kwargs):
            receiver.halt.set()
            return frame

    monkeypatch.setattr(mode.wsc, "connect", lambda *_a, **_k: Socket())
    receiver = mode.DisplayRx("ws://fixture")
    receiver.run()
    assert receiver.error is None
    measured_axis, measured_power = receiver.rtaf[0]
    np.testing.assert_array_equal(measured_axis, axis)
    np.testing.assert_array_equal(measured_power, power)
    assert mode.peak_of(measured_axis, measured_power)[0] == axis[1]


@pytest.mark.parametrize("driver", ["harm", "tinysa"])
@pytest.mark.parametrize("new_cycle", [False, True])
def test_bench_pnm_restart_requires_partial_after_done(monkeypatch, driver, new_cycle):
    clock = Clock()
    monkeypatch.setattr(harm, "time", clock)
    monkeypatch.setattr(tinysa, "time", clock)
    monkeypatch.setattr(harm.wsc, "connect", lambda *_a, **_k: SimpleNamespace())
    carrier = harm.CARRIER if driver == "harm" else tinysa.FREQ
    partial = {"cmd": "PNM", "done": False, "progress": 10}
    done = {"cmd": "PNM", "done": True, "progress": 100, "carrier_freq": carrier,
            "offset": list(range(1, 9)), "pn": [-90.0] * 8}
    receiver = harm.MsgRx("ws://fixture")
    receiver.texts = [{"cmd": "HARM", "list": [{"f": carrier, "amp": -25.0}]}, partial, done]
    if new_cycle:
        receiver.texts.append(dict(partial))
    receiver.start = receiver.finish = lambda: None
    healthy = {"connected": True, "last_error": None, "actual": {"points": 16}}
    for module in (harm, tinysa):
        monkeypatch.setattr(module, "post", lambda *_a, **_k: {})
        monkeypatch.setattr(module, "get_state", lambda *_a, **_k: healthy)
    if driver == "harm":
        monkeypatch.setattr(harm, "MsgRx", lambda *_a, **_k: receiver)
        result, problems = harm.run_round("http://fixture")
    else:
        monkeypatch.setattr(tinysa, "apply_same_config", lambda *_a, **_k: None)
        monkeypatch.setattr(tinysa, "measure", lambda *_a, **_k: {})
        result, problems = tinysa.run_websad_side("http://fixture", receiver, 1, -25.0)
    assert result["pnm_restarted"] is new_cycle
    assert any("did not restart after done" in problem for problem in problems) is not new_cycle
