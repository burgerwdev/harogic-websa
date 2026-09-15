"""Supervisor restart policy: restart a native crash, but not forever.

A worker whose SDK calls crash deterministically cannot be repaired by another restart.
Measured before this guard existed: while the SAN-90 was unplugged the worker crashed in
``Device_Close`` on a stale handle and the supervisor restarted it into the same crash,
writing one core dump per attempt (7 in under a minute).
"""
from __future__ import annotations

import pytest

from web_sa import supervisor
from web_sa.web.recovery import EXIT_FATAL


class Clock:
    """Fake wall clock so a test runtime needs no real sleeping."""

    def __init__(self, t=1000.0):
        self.t = t

    def monotonic(self):
        return self.t

    def jump(self, dt):
        self.t += dt


class FakeChild:
    """A worker process that runs for ``runtime`` seconds and exits with ``code``."""

    def __init__(self, code, clock, runtime=0.0):
        self.code = code
        self.clock = clock
        self.runtime = runtime

    def wait(self):
        self.clock.jump(self.runtime)
        return self.code

    def poll(self):
        return self.code

    def send_signal(self, signum):
        pass


@pytest.fixture
def harness(monkeypatch):
    """Patch logging/signals/sleep and give back a spawn driver."""

    clock = Clock()
    monkeypatch.setattr(supervisor, 'setup_logging', lambda *a, **k: None)
    monkeypatch.setattr(supervisor.signal, 'signal', lambda *a, **k: None)
    monkeypatch.setattr(supervisor.time, 'sleep', lambda seconds: clock.jump(seconds))
    monkeypatch.setattr(supervisor.time, 'monotonic', clock.monotonic)

    def drive(codes, runtimes):
        spawned = []

        def fake_popen(argv):
            spawned.append(argv)
            i = min(len(spawned) - 1, len(codes) - 1)
            return FakeChild(codes[i], clock, runtimes[min(i, len(runtimes) - 1)])

        monkeypatch.setattr(supervisor.subprocess, 'Popen', fake_popen)
        try:
            supervisor.main()
            code = None                       # a normal stop (worker exited 0)
        except SystemExit as exc:
            code = exc.code
        return spawned, code

    return drive


def test_should_restart_only_for_native_crashes_and_fatal():
    assert supervisor.should_restart(-11) is True          # SIGSEGV
    assert supervisor.should_restart(EXIT_FATAL) is True
    assert supervisor.should_restart(0) is False
    assert supervisor.should_restart(2) is False


def test_crash_loop_gives_up_instead_of_spinning(harness, monkeypatch):
    """Immediate crashes in a row: stop, do not keep spawning workers."""
    monkeypatch.setattr(supervisor, 'FAST_EXIT_LIMIT', 5)
    spawned, code = harness([-11] * 20, [0.0])
    assert len(spawned) == 5
    assert code == -11


def test_a_worker_that_ran_usefully_resets_the_counter(harness, monkeypatch):
    """fast, fast, long (counter resets), fast, fast, fast -> stop at six spawns.

    Without the reset the third crash would already have ended it, so reaching six proves
    the counter was cleared by the long run.
    """
    monkeypatch.setattr(supervisor, 'FAST_EXIT_S', 30.0)
    monkeypatch.setattr(supervisor, 'FAST_EXIT_LIMIT', 3)
    spawned, _ = harness([-11] * 20, [1.0, 1.0, 60.0, 1.0, 1.0, 1.0])
    assert len(spawned) == 6


def test_normal_exit_stops_the_supervisor(harness):
    spawned, code = harness([0], [1.0])
    assert len(spawned) == 1
    assert code is None                       # returns normally, no SystemExit


def test_non_recoverable_status_is_not_restarted(harness):
    spawned, code = harness([2], [1.0])
    assert len(spawned) == 1
    assert code == 2
