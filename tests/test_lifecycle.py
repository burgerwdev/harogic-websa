"""Exercise the real fixture daemon through the portable root launchers."""
from __future__ import annotations

import os
import shutil
import signal
import socket
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]


@pytest.fixture
def launcher(tmp_path):
    binary = ROOT / 'backend/target/release/websad'
    if not binary.is_file():
        binary = ROOT / 'runtime/linux-x86_64/websad'
    if not binary.is_file():
        pytest.skip('build make native or restore runtime executable for lifecycle integration')
    checkout = tmp_path / 'checkout with spaces'
    checkout.mkdir()
    for name in ('run.sh', 'stop.sh', 'status.sh', 'lifecycle.sh'):
        shutil.copy2(ROOT / name, checkout / name)
    static = tmp_path / 'static'
    (static / 'dist').mkdir(parents=True)
    (static / 'dist/index.html').write_text('<html>fixture</html>')
    runtime = tmp_path / 'runtime with spaces'
    runtime.mkdir(mode=0o700)
    forbidden = tmp_path / 'forbidden'
    forbidden.mkdir()
    marker = tmp_path / 'systemd-called'
    for command in ('systemctl', 'systemd-run', 'journalctl'):
        script = forbidden / command
        script.write_text(f'#!/bin/sh\ntouch "{marker}"\nexit 99\n')
        script.chmod(0o755)
    env = {key: value for key, value in os.environ.items() if not key.startswith('WEBSA_')}
    env.update(WEBSA_BIN=str(binary), WEBSA_STATIC=str(static), WEBSA_FAKE='1',
               WEBSA_HOST='127.0.0.1', XDG_RUNTIME_DIR=str(runtime),
               PATH=f'{forbidden}:{env["PATH"]}', WEBSA_RUST_IQS_SHIM='/must/not/load',
               WEBSA_FIXTURES=str(ROOT / 'tests/fixtures'))
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0))
        env['WEBSA_PORT'] = str(reservation.getsockname()[1])

    def invoke(name, *args, **kwargs):
        return subprocess.run([str(checkout / name), *args], cwd=checkout,
                              env=kwargs.pop('env', env), text=True, capture_output=True,
                              timeout=25, **kwargs)

    def state_file():
        return next(runtime.glob('harogic-websa/*/websad.state'))

    yield invoke, env, checkout, state_file, runtime
    invoke('stop.sh')
    assert not marker.exists(), 'default lifecycle invoked a systemd command'


def test_fixture_lifecycle_uses_saved_endpoint_and_is_idempotent(launcher):
    invoke, env, _, state_file, _ = launcher
    started = invoke('run.sh')
    assert started.returncode == 0, started.stderr
    metadata = state_file().read_text().splitlines()
    status_env = {**env, 'WEBSA_PORT': '1', 'WEBSA_HOST': '192.0.2.1'}
    status = invoke('status.sh', env=status_env)
    assert status.returncode == 0, status.stderr
    assert f'127.0.0.1:{metadata[7]}/api/state' in status.stdout
    duplicate = invoke('run.sh')
    assert duplicate.returncode != 0 and 'already running' in duplicate.stderr
    assert invoke('stop.sh').returncode == 0
    assert invoke('stop.sh').returncode == 0
    assert invoke('status.sh', '--quiet').returncode != 0
    assert invoke('run.sh').returncode == 0


@pytest.mark.parametrize('field,value', [(1, '0'), (3, '/unrelated'), (5, '/unrelated')])
def test_mismatched_identity_never_signals_process(launcher, field, value):
    invoke, _, _, state_file, _ = launcher
    assert invoke('run.sh').returncode == 0
    path = state_file()
    original = path.read_text()
    values = original.splitlines()
    pid = int(values[0])
    values[field] = value
    path.write_text('\n'.join(values) + '\n')
    try:
        assert invoke('status.sh', '--quiet').returncode != 0
        assert invoke('stop.sh').returncode == 0
        os.kill(pid, 0)
    finally:
        path.write_text(original)
    assert invoke('status.sh', '--quiet').returncode == 0


def test_stale_pid_is_replaced_without_signalling(launcher):
    invoke, _, _, state_file, _ = launcher
    assert invoke('run.sh').returncode == 0
    path = state_file()
    values = path.read_text().splitlines()
    assert invoke('stop.sh').returncode == 0
    values[0] = '2147483647'
    path.write_text('\n'.join(values) + '\n')
    restarted = invoke('run.sh')
    assert restarted.returncode == 0, restarted.stderr
    assert state_file().read_text().splitlines()[0] != values[0]


def test_occupied_port_is_left_untouched(launcher):
    invoke, env, _, _, _ = launcher
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', int(env['WEBSA_PORT'])))
        listener.listen()
        result = invoke('run.sh')
        assert result.returncode != 0 and 'already listening' in result.stderr
        with socket.create_connection(listener.getsockname(), timeout=1):
            connection, _ = listener.accept()
            connection.close()


def test_concurrent_starts_have_one_owner(launcher):
    invoke, env, checkout, _, _ = launcher
    processes = [subprocess.Popen([str(checkout / 'run.sh')], cwd=checkout, env=env,
                                  text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                 for _ in range(2)]
    results = [process.communicate(timeout=25) for process in processes]
    assert sorted(process.returncode for process in processes) == [0, 1], results
    assert invoke('status.sh', '--quiet').returncode == 0


def test_state_home_fallback_and_named_instance(launcher, tmp_path):
    invoke, env, _, _, _ = launcher
    fallback = {**env, 'XDG_RUNTIME_DIR': '', 'XDG_STATE_HOME': str(tmp_path / 'state home'),
                'WEBSA_INSTANCE': 'alternate'}
    try:
        result = invoke('run.sh', env=fallback)
        assert result.returncode == 0, result.stderr
        assert invoke('status.sh', '--quiet', env=fallback).returncode == 0
        assert invoke('status.sh', '--quiet').returncode != 0
    finally:
        assert invoke('stop.sh', env=fallback).returncode == 0


def test_crashed_process_can_be_restarted(launcher):
    invoke, _, _, state_file, _ = launcher
    assert invoke('run.sh').returncode == 0
    pid = int(state_file().read_text().splitlines()[0])
    os.kill(pid, signal.SIGKILL)
    # status checks both identity and HTTP; no supervisor silently restarts it.
    assert invoke('status.sh', '--quiet').returncode != 0
    restarted = invoke('run.sh')
    assert restarted.returncode == 0, restarted.stderr
    assert int(state_file().read_text().splitlines()[0]) != pid


def test_stop_escalates_only_the_owned_process(launcher):
    invoke, _, _, state_file, _ = launcher
    assert invoke('run.sh').returncode == 0
    pid = int(state_file().read_text().splitlines()[0])
    os.kill(pid, signal.SIGSTOP)
    result = invoke('stop.sh')
    assert result.returncode == 0, result.stderr
    assert invoke('status.sh', '--quiet').returncode != 0
