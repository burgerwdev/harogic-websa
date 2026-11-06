"""Exercise the real fixture daemon through the portable root launchers."""
from __future__ import annotations

import json
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
    assert 'HTTP: healthy' in status.stdout
    assert 'Device:' in status.stdout and 'RF: center' in status.stdout
    assert '"actual"' not in status.stdout and '"preset_defaults"' not in status.stdout
    assert len(status.stdout.splitlines()) <= 10
    quiet = invoke('status.sh', '--quiet')
    assert quiet.returncode == 0 and not quiet.stdout and not quiet.stderr
    duplicate = invoke('run.sh')
    assert duplicate.returncode != 0 and 'already running' in duplicate.stderr
    assert invoke('stop.sh').returncode == 0
    assert invoke('stop.sh').returncode == 0
    assert invoke('status.sh', '--quiet').returncode != 0
    assert invoke('run.sh').returncode == 0


def test_status_summary_keeps_faults_and_http_health_contract(launcher, tmp_path):
    invoke, env, _, _, _ = launcher
    assert invoke('run.sh').returncode == 0
    # Preserve real process/listener ownership checks, replacing only HTTP data.
    commands = tmp_path / 'status commands'
    commands.mkdir()
    body = tmp_path / 'response.json'
    curl = commands / 'curl'
    curl.write_text('#!/bin/sh\ncat "$STATUS_RESPONSE"\nexit "${STATUS_EXIT:-0}"\n')
    curl.chmod(0o755)
    status_env = {**env, 'PATH': f'{commands}:{env["PATH"]}', 'STATUS_RESPONSE': str(body)}
    body.write_text(json.dumps({
        'connected': True, 'mode': 'sdr', 'caps': {'name': 'SAN-90'},
        'center': 14_270_000, 'span': 781_250, 'ref': -37,
        'sdr': {'listen': 14_275_001, 'demod': 'usb', 'if_bw': 2400},
        'actual': {'iq_rate': 3_906_250, 'receiver_workers': {'branches': [
            {'channel': 1, 'lifecycle': 'active', 'fault': None},
            {'channel': 2, 'lifecycle': 'retiring', 'fault': {'error': 'sequence failure'}},
        ]}, 'iqs_events': {'large_diagnostic': 'hidden' * 1000}},
        'last_error': 'source timeout\nretry pending', 'status_warning': -12,
    }))
    result = invoke('status.sh', env=status_env)
    assert result.returncode == 0, result.stderr
    assert 'SAN-90 | connected | mode SDR' in result.stdout
    assert 'center 14.27 MHz | span 781.25 kHz | ref -37.0 dBm' in result.stdout
    assert 'listen 14.275001 MHz | USB | IF 2.4 kHz | IQ 3.90625 MS/s' in result.stdout
    assert 'Decoders: 1 active | 1 failed' in result.stdout
    assert 'Fault C2: sequence failure' in result.stdout
    assert 'Device error: source timeout retry pending' in result.stdout
    assert 'Device warning: -12' in result.stdout
    assert 'large_diagnostic' not in result.stdout and 'hidden' not in result.stdout
    # Device failures must not change the launcher health exit code used by make.
    body.write_text(json.dumps({'connected': False, 'mode': 'std', 'actual': None}))
    result = invoke('status.sh', env=status_env)
    assert result.returncode == 0 and 'disconnected' in result.stdout
    assert 'Decoders:' not in result.stdout and 'center unknown' in result.stdout
    body.write_text(json.dumps({'mode': 'server', 'server': {
        'state': 'failed', 'bind_address': '127.0.0.1', 'port': 55132, 'error': 'bind failed',
    }}))
    result = invoke('status.sh', env=status_env)
    assert result.returncode == 0 and 'Server: failed | 127.0.0.1:55132' in result.stdout
    assert 'Server error: bind failed' in result.stdout and 'RF:' not in result.stdout
    body.write_text('not JSON')
    result = invoke('status.sh', env=status_env)
    assert result.returncode == 0 and 'summary unavailable' in result.stdout
    body.write_text('')
    for args in [(), ('--quiet',)]:
        result = invoke('status.sh', *args, env={**status_env, 'STATUS_EXIT': '22'})
        assert result.returncode == 22
        assert 'HTTP: healthy' not in result.stdout
        if args:
            assert not result.stdout and not result.stderr


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
