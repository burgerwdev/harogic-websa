"""Exercise the real fixture daemon through the portable root launchers."""
from __future__ import annotations

import asyncio
import copy
import json
import math
import os
import shutil
import signal
import socket
import struct
import subprocess
import time
from contextlib import AsyncExitStack
from pathlib import Path

import aiohttp
import pytest
from jsonschema import Draft202012Validator

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
    for name in ('run.sh', 'stop.sh', 'status.sh', 'lifecycle.sh', 'supervise.py'):
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
               WEBSA_FIXTURES=str(ROOT / 'tests/fixtures'),
               WEBSA_RECORDING_DIR=str(tmp_path / 'recordings'))
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
    assert 'Binary: current' in status.stdout
    assert 'Device:' in status.stdout and 'RF: center' in status.stdout
    assert '"actual"' not in status.stdout and '"preset_defaults"' not in status.stdout
    assert len(status.stdout.splitlines()) <= 11
    quiet = invoke('status.sh', '--quiet')
    assert quiet.returncode == 0 and not quiet.stdout and not quiet.stderr
    duplicate = invoke('run.sh')
    assert duplicate.returncode != 0 and 'already running' in duplicate.stderr
    assert invoke('stop.sh').returncode == 0
    assert invoke('stop.sh').returncode == 0
    assert invoke('status.sh', '--quiet').returncode != 0
    assert invoke('run.sh').returncode == 0


def test_status_identifies_a_replaced_binary_until_restart(launcher, tmp_path):
    invoke, env, _, _, _ = launcher
    binary = tmp_path / 'websad'
    replacement = tmp_path / 'replacement'
    shutil.copy2(env['WEBSA_BIN'], binary)
    assert invoke('run.sh', '--bin', str(binary)).returncode == 0
    shutil.copy2(env['WEBSA_BIN'], replacement)
    replacement.replace(binary)
    status = invoke('status.sh')
    assert status.returncode == 0, status.stderr
    assert 'Binary: changed on disk; restart' in status.stdout
    assert invoke('stop.sh').returncode == 0
    assert invoke('run.sh', '--bin', str(binary)).returncode == 0
    assert 'Binary: current' in invoke('status.sh').stdout


def test_local_decoders_are_discovered_without_sourcing_shell_files(launcher, tmp_path):
    invoke, env, _, _, _ = launcher
    prefix = tmp_path / 'data with spaces/websa/decoders'
    (prefix / 'bin').mkdir(parents=True)
    multimon = prefix / 'bin/multimon-ng'
    multimon.write_text('#!/bin/sh\necho "multimon-ng test"\necho "Available demodulators: DTMF"\nexit 2\n')
    multimon.chmod(0o755)
    launch_env = {**env, 'XDG_DATA_HOME': str(prefix.parents[1])}
    started = invoke('run.sh', env=launch_env)
    assert started.returncode == 0, started.stderr
    status = invoke('status.sh', '--decoders', env={**env, 'WEBSA_PORT': '1'})
    assert status.returncode == 0, status.stderr
    assert 'multimon: ready | dtmf' in status.stdout
    assert 'does not advertise POCSAG512' in status.stdout


def test_enabled_receivers_process_without_browser_or_output_subscriptions(launcher):
    invoke, env, _, _, _ = launcher
    started = invoke('run.sh')
    assert started.returncode == 0, started.stderr
    origin = f'http://127.0.0.1:{env["WEBSA_PORT"]}'

    async def exercise():
        async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=10)) as client:
            async def request(path, payload=None):
                method = client.get if payload is None else client.post
                async with method(origin + path, **({} if payload is None else {'json': payload})) as response:
                    value = await response.json()
                    assert response.status == 200, value
                    return value

            async def active(count):
                deadline = asyncio.get_running_loop().time() + 10
                while True:
                    snapshot = await request('/api/sdr/receivers')
                    if len(snapshot['receivers']) == count and all(row['processing_state'] == 'active' for row in snapshot['receivers']):
                        return snapshot
                    assert asyncio.get_running_loop().time() < deadline, snapshot
                    await asyncio.sleep(0.05)

            await request('/api/config', {'cmd': 'SET_MODE', 'mode': 'sdr'})
            first = await active(1)
            current = first['receivers'][0]
            config = copy.deepcopy(current['confirmed'])
            config.update(name='API reception')
            config['baseband'].update(passband_hz=6000, transition_hz=1000, minimum_rate_hz=48000)
            reply = await request('/api/sdr/receivers', {'type': 'create', 'request_id': 'without-view',
                'service_id': first['service_id'], 'config': config})
            assert reply['fault'] is None, reply['fault']
            before = await active(2)
            await asyncio.sleep(0.2)
            after = await active(2)
            for old, new in zip(before['receivers'], after['receivers'], strict=True):
                assert old['stream'] == new['stream']
                assert int(old['clock']['input_index']) < int(new['clock']['input_index'])
                assert new['fault'] is None and new['processing_fault'] is None
    asyncio.run(exercise())


def test_aux_channel_alias_http_and_websocket_match_canonical_contract(launcher):
    invoke, env, _, _, _ = launcher
    started = invoke('run.sh')
    assert started.returncode == 0, started.stderr
    base = f'http://127.0.0.1:{env["WEBSA_PORT"]}'

    async def exercise():
        async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=5)) as session:
            async def post(body, status=200):
                async with session.post(base + '/api/config', json=body) as response:
                    value = await response.json()
                    assert response.status == status, value
                    return value

            initial = await post({'cmd': 'SET_MODE', 'mode': 'sdr'})
            frequency = initial['sdr']['channels'][0]['freq'] + 100_000
            enabled = await post({'cmd': 'SET_SDR_AUX', 'channel': 2, 'enabled': True,
                                  'freq': frequency, 'mode': 'am', 'ifbw': 6000,
                                  'request_id': 'alias-http-enable'})
            assert enabled['response_to'] == 'SET_SDR_AUX'
            assert enabled['request_id'] == 'alias-http-enable'
            auxiliary = enabled['sdr']['channels'][1]
            assert auxiliary['enabled'] and auxiliary['freq'] == frequency
            primary = enabled['sdr']['channels'][0]

            async with session.ws_connect(base.replace('http://', 'ws://') + '/ws') as ws:
                async def correlated(request):
                    await ws.send_json(request)

                    async def receive():
                        async for message in ws:
                            if message.type == aiohttp.WSMsgType.TEXT:
                                value = json.loads(message.data)
                                if value.get('request_id') == request['request_id']:
                                    return value
                        raise AssertionError('WebSocket closed before its command reply')

                    return await asyncio.wait_for(receive(), timeout=5)

                invalid = await correlated({'cmd': 'SET_SDR_AUX', 'channel': 1,
                                            'enabled': False, 'request_id': 'alias-bad-slot'})
                assert invalid['cmd'] == 'ERROR' and invalid['code'] == 'bad_parameter', invalid
                disabled = await correlated({'cmd': 'SET_SDR_AUX', 'channel': 2,
                                             'enabled': False, 'request_id': 'alias-ws-disable'})
                assert disabled['cmd'] == 'STATUS', disabled
                assert disabled['response_to'] == 'SET_SDR_AUX'
                assert not disabled['sdr']['channels'][1]['enabled']
                for field in ('enabled', 'freq', 'mode', 'channel_id', 'epoch'):
                    assert disabled['sdr']['channels'][0][field] == primary[field]

    asyncio.run(exercise())


def test_aux_channel_alias_is_advertised_in_schema(launcher):
    invoke, env, _, _, _ = launcher
    started = invoke('run.sh')
    assert started.returncode == 0, started.stderr
    base = f'http://127.0.0.1:{env["WEBSA_PORT"]}'

    async def exercise():
        async with (
            aiohttp.ClientSession() as session,
            session.get(base + '/api/schema') as response,
        ):
            response.raise_for_status()
            commands = (await response.json())['commands']
            assert 'SET_SDR_AUX' in commands
            assert commands['SET_SDR_AUX'] == commands['SET_SDR_DDC']

    asyncio.run(exercise())


def test_leaving_sdr_ends_iq_audio_and_decode_subscriptions_normally(launcher):
    invoke, env, _, _, _ = launcher
    started = invoke('run.sh')
    assert started.returncode == 0, started.stderr
    origin = f'http://127.0.0.1:{env["WEBSA_PORT"]}'

    async def exercise():
        async with aiohttp.ClientSession() as client, AsyncExitStack() as stack:
            async with client.post(origin + '/api/config', json={'cmd': 'SET_MODE', 'mode': 'sdr'}) as response:
                assert response.status == 200
                await response.read()
            async with asyncio.timeout(10):
                while True:
                    async with client.get(origin + '/api/sdr/receivers') as response:
                        receiver = (await response.json())['receivers'][0]
                    if receiver['processing_state'] == 'active':
                        uid = receiver['receiver_uid']
                        break
                    await asyncio.sleep(0.02)
            streams = [await stack.enter_async_context(client.ws_connect(origin + path, params={'receiver_uid': uid}))
                       for path in ['/ws/iq', '/ws/audio', '/ws/decode']]
            async with client.post(origin + '/api/config', json={'cmd': 'SET_MODE', 'mode': 'rta'}) as response:
                assert response.status == 200
                await response.read()
            for stream in streams:
                while True:
                    message = await stream.receive(timeout=5)
                    assert message.type not in (aiohttp.WSMsgType.ERROR, aiohttp.WSMsgType.CLOSED)
                    if message.type == aiohttp.WSMsgType.CLOSE:
                        assert message.data == 1000 and message.extra == 'SDR inactive'
                        break

    asyncio.run(exercise())


def test_advertised_sdr_ranges_include_values_accepted_by_typed_controls(launcher):
    invoke, env, _, _, _ = launcher
    started = invoke('run.sh')
    assert started.returncode == 0, started.stderr
    base = f'http://127.0.0.1:{env["WEBSA_PORT"]}'

    async def exercise():
        async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=5)) as session:
            async def post(body, status=200):
                async with session.post(base + '/api/config', json=body) as response:
                    value = await response.json()
                    assert response.status == status, value
                    return value

            await post({'cmd': 'SET_MODE', 'mode': 'sdr'})
            accepted = []
            for command in ('SET_SDR_DEMOD', 'SET_SDR_CHANNEL'):
                for mode, field, value in (('am', 'squelch', -200), ('am', 'squelch', 0),
                                           ('am', 'pitch', 0.5), ('am', 'pitch', 20000),
                                           ('cw', 'pitch', 100), ('cw', 'pitch', 1600)):
                    response = await post({'cmd': command, 'mode': mode, field: value})
                    assert response['sdr']['channels'][0][field] == value
                    accepted.append((command, field, value))
            for mode, field, value in (('am', 'squelch', -201), ('am', 'squelch', 1),
                                       ('am', 'pitch', 0), ('am', 'pitch', 20001),
                                       ('cw', 'pitch', 99), ('cw', 'pitch', 1601)):
                await post({'cmd': 'SET_SDR_CHANNEL', 'mode': mode, field: value}, status=400)
            async with session.get(base + '/api/schema') as response:
                response.raise_for_status()
                commands = (await response.json())['commands']
            # Check metadata against actual successful requests, rather than
            # copying the server's declared min/max into the expected values.
            for command, field, value in accepted:
                spec = next(item for item in commands[command]['params'] if item['name'] == field)
                assert spec['min'] <= value <= spec['max'], (command, field, value, spec)

    asyncio.run(exercise())


def test_receiver_schema_covers_idle_processing_and_disabled_responses(launcher):
    invoke, env, _, _, _ = launcher
    started = invoke('run.sh')
    assert started.returncode == 0, started.stderr
    schema = json.loads((ROOT / 'tests/fixtures/multichannel/draft-v1.schema.json').read_text())
    Draft202012Validator.check_schema(schema)
    snapshot_validator = Draft202012Validator({'$defs': schema['$defs'], '$ref': '#/$defs/snapshot'})
    reply_validator = Draft202012Validator({'$defs': schema['$defs'], '$ref': '#/$defs/reply'})
    base = f'http://127.0.0.1:{env["WEBSA_PORT"]}'

    async def exercise():
        async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=5)) as session:
            async def request(path, body=None):
                async with session.request('GET' if body is None else 'POST', base + path,
                                           json=body) as response:
                    response.raise_for_status()
                    return await response.json()

            async def snapshot():
                value = await request('/api/sdr/receivers')
                snapshot_validator.validate(value)
                return value

            await snapshot()
            await request('/api/config', {'cmd': 'SET_MODE', 'mode': 'sdr'})
            value = await snapshot()
            receiver = value['receivers'][0]
            uid = receiver['receiver_uid']
            config = copy.deepcopy(receiver['confirmed'])
            config['processors'][0]['pcm_gain'] = 0.75
            config['processors'][0]['rf_snr'] = True
            async with session.ws_connect(base + '/ws/audio', params={'receiver_uid': uid}) as audio:
                reply = await request('/api/sdr/receivers', {
                    'type': 'configure', 'request_id': 'schema-processing',
                    'receiver_uid': uid, 'expected_revision': receiver['revision'], 'config': config,
                })
                reply_validator.validate(reply)
                assert reply['fault'] is None, reply['fault']
                while True:
                    message = await audio.receive(timeout=5)
                    assert message.type not in (aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.ERROR)
                    active = await snapshot()
                    receiver = next(row for row in active['receivers'] if row['receiver_uid'] == uid)
                    measured = any(row['packets']['successful_samples'] > 0
                                   for row in receiver['processing_statistics'] or [])
                    applied = any(row['request_id'] == 'schema-processing' and row['status'] == 'applied'
                                  for row in receiver['control_receipts'])
                    if measured and applied:
                        break
                assert receiver['processing_runtime']['observed_receiver'] is not None
                config['enabled'] = False
                reply = await request('/api/sdr/receivers', {
                    'type': 'configure', 'request_id': 'schema-disabled',
                    'receiver_uid': uid, 'expected_revision': receiver['revision'], 'config': config,
                })
                reply_validator.validate(reply)
                assert reply['fault'] is None, reply['fault']
                disabled = await snapshot()
                assert next(row for row in disabled['receivers'] if row['receiver_uid'] == uid)['state'] == 'stopped'
                return active

    async def bounded():
        return await asyncio.wait_for(exercise(), timeout=15)

    active = asyncio.run(bounded())
    # Mutate real observed objects so invalid types, enums and unknown fields
    # cannot pass by falling through to an unconstrained diagnostic object.
    for path, invalid in [
        (('action_token',), 7),
        (('capabilities', 'control_capacity'), '64'),
        (('capabilities', 'resource_policy'), 'unlimited'),
        (('host_resources', 'available_memory_bytes'), -1),
        (('model_preparation', 'queued'), -1),
        (('receivers', 0, 'processing_state'), 'unknown'),
        (('receivers', 0, 'processing_runtime', 'processing_generation'), 1),
        (('receivers', 0, 'processing_runtime', 'unexpected'), True),
        (('receivers', 0, 'processing_runtime', 'observed_receiver', 'rf_measurement', 'value'), '0'),
        (('receivers', 0, 'processing_runtime', 'observed_receiver', 'rf_measurement', 'unit'), 'dB-Hz'),
        (('receivers', 0, 'processing_runtime', 'observed_receiver', 'rf_measurement', 'averaging_seconds'), -1),
        (('receivers', 0, 'confirmed', 'continuity'), 'discard'),
        (('receivers', 0, 'confirmed', 'processors', 0, 'audio_rate'), '48000'),
        (('receivers', 0, 'confirmed', 'processors', 0, 'sstv_mode'), 18),
        (('receivers', 0, 'confirmed', 'processors', 0, 'radiofax_lpm'), '999'),
        (('receivers', 0, 'control_receipts', 0, 'status'), 'success'),
        (('receivers', 0, 'processing_statistics', 0, 'packets', 'accepted'), -1),
    ]:
        changed = copy.deepcopy(active)
        target = changed
        for part in path[:-1]:
            target = target[part]
        target[path[-1]] = invalid
        assert not snapshot_validator.is_valid(changed), f'accepted invalid {path}: {invalid}'


def test_receiver_schema_checks_nonempty_faults_and_recovery_history():
    schema = json.loads((ROOT / 'tests/fixtures/multichannel/draft-v1.schema.json').read_text())
    # Independent typed examples follow BranchFault, receiver_controls::Command
    # and reception_recovery::Interruption serializers, including null clocks.
    clock = {'input_index': '9007199254740993', 'input_rate': 62500000.0,
             'channel_id': '3', 'epoch': '9', 'actual_rate': 15625.0,
             'sample_count': 4096, 'delay_input_samples': 12.5, 'precision': 'block'}
    fault = {'channel': 1, 'generation': 7, 'error': 'injected failure', 'kind': 'overload',
             'receiver_uid': 'receiver-1', 'service_id': 'service-1', 'last_success': clock,
             'last_delivered': None, 'first_unavailable': clock, 'gap_end': None}
    position = {'channel_id': 3, 'epoch': 9, 'input_index': '9007199254740993'}
    interruption = {'number': 1, 'failed_identity': '3/9', 'resumed_processing_stream': None,
                    'kind': 'overload', 'scope': 'processing', 'reason': 'injected failure',
                    'at_ms': 1000, 'before': position, 'after': None, 'missing_input_samples': None,
                    'recovered_at_ms': None, 'precision': 'unknown_gap'}
    reception = {'policy': 'continuous', 'state': 'recovering', 'interruptions': 1,
                 'attempts': 0, 'retry_limit': 3, 'retry_in_ms': 500,
                 'manual_recovery_allowed': False, 'last_error': None,
                 'last_interruption': interruption, 'history': [], 'startup': 'fresh state'}
    receipt = {'receiver_uid': 'receiver-1', 'service_id': 'service-1', 'revision': '12',
               'processing_generation': '7', 'request_id': 'control-1', 'action_id': None,
               'status': 'applied', 'reason': None, 'boundary': {
                   'precision': 'block', 'channel_id': '3', 'epoch': '9',
                   'input_index': '9007199254740993', 'fraction_num': '0', 'fraction_den': '1',
                   'input_rate': 62500000.0, 'delay_input_samples': 12.5}}
    cases = [
        ('processing_fault', fault, ('generation',), '7'),
        ('processing_fault', fault, ('last_success', 'input_index'), 9007199254740993),
        ('processing_fault', fault, ('kind',), 'normal'),
        ('reception', reception, ('last_interruption', 'before', 'input_index'), 1),
        ('reception', reception, ('state',), 'active'),
        ('reception', reception, ('last_interruption', 'missing_input_samples'), 100),
        ('control_receipt', receipt, ('boundary', 'fraction_den'), '0'),
        ('control_receipt', receipt, ('boundary', 'delay_input_samples'), -1),
        ('control_receipt', receipt, ('status',), 'accepted'),
    ]
    for definition, value, path, invalid in cases:
        validator = Draft202012Validator({'$defs': schema['$defs'], '$ref': f'#/$defs/{definition}'})
        validator.validate(value)
        changed = copy.deepcopy(value)
        target = changed
        for part in path[:-1]:
            target = target[part]
        target[path[-1]] = invalid
        assert not validator.is_valid(changed), f'accepted invalid {definition}.{path}: {invalid}'


@pytest.mark.parametrize('operation', ['retire', 'disable', 'remove_processor'])
def test_normal_receiver_end_closes_only_its_subscriptions_and_can_restart(launcher, operation):
    invoke, env, _, _, _ = launcher
    started = invoke('run.sh')
    assert started.returncode == 0, started.stderr
    base = f'http://127.0.0.1:{env["WEBSA_PORT"]}'

    async def exercise():
        async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=5)) as session:
            async def request(path, body=None):
                async with session.request('GET' if body is None else 'POST', base + path,
                                           json=body) as response:
                    response.raise_for_status()
                    value = await response.json()
                    assert value.get('fault') is None, value
                    return value

            async def audio_frame(socket):
                while True:
                    message = await socket.receive(timeout=5)
                    assert message.type == aiohttp.WSMsgType.BINARY, message
                    frame = message.data
                    assert frame[:4] == b'AUD2' and len(frame) >= 96
                    sequence, count = struct.unpack_from('<II', frame, 8)
                    if count:
                        channels = struct.unpack_from('<I', frame, 56)[0]
                        assert len(frame) == 96 + count * channels * 4
                        return (sequence, count, struct.unpack_from('<Q', frame, 64)[0],
                                frame[32:40])

            async def ended(socket):
                while True:
                    message = await socket.receive(timeout=5)
                    if message.type == aiohttp.WSMsgType.CLOSE:
                        reason = 'receiver retired' if operation == 'retire' else 'receiver stopped'
                        assert (message.data, message.extra) == (1000, reason)
                        return
                    assert message.type == aiohttp.WSMsgType.BINARY, message

            state = await request('/api/config', {'cmd': 'SET_MODE', 'mode': 'sdr'})
            frequency = state['sdr']['channels'][0]['freq']
            await request('/api/config', {'cmd': 'SET_SDR_CHANNEL', 'channel': 2,
                                         'enabled': True, 'freq': frequency + 100_000,
                                         'mode': 'am', 'ifbw': 6000})
            snapshot = await request('/api/sdr/receivers')
            healthy, target = snapshot['receivers'][:2]
            uid = target['receiver_uid']
            original = copy.deepcopy(target['confirmed'])
            async with AsyncExitStack() as stack:
                audio = await stack.enter_async_context(session.ws_connect(
                    base + '/ws/audio', params={'receiver_uid': uid}))
                decode = await stack.enter_async_context(session.ws_connect(
                    base + '/ws/decode', params={'receiver_uid': uid}))
                other = await stack.enter_async_context(session.ws_connect(
                    base + '/ws/audio', params={'receiver_uid': healthy['receiver_uid']}))
                await audio_frame(audio)
                event = await decode.receive(timeout=5)
                assert event.type == aiohttp.WSMsgType.BINARY and event.data[:4] == b'SDE2'
                previous = await audio_frame(other)
                command = {'type': 'retire', 'request_id': 'end-target', 'receiver_uid': uid}
                if operation != 'retire':
                    config = copy.deepcopy(original)
                    if operation == 'disable':
                        config['enabled'] = False
                    else:
                        config.update(purpose='iq', processors=[])
                    command.update(type='configure', config=config,
                                   expected_revision=target['revision'])
                reply = await request('/api/sdr/receivers', command)
                await asyncio.gather(ended(audio), ended(decode))
                for _ in range(5):
                    current = await audio_frame(other)
                    assert current[0] == previous[0] + 1
                    assert current[2] == previous[2] + previous[1]
                    assert current[3] == previous[3]
                    previous = current
                observed = await request('/api/sdr/receivers')
                unchanged = next(row for row in observed['receivers']
                                 if row['receiver_uid'] == healthy['receiver_uid'])
                for key in ('confirmed', 'revision', 'stream'):
                    assert unchanged[key] == healthy[key]
                restore = {'type': 'create', 'request_id': 'restart-target', 'config': original}
                if operation != 'retire':
                    stopped = next(row for row in reply['snapshot']['receivers']
                                   if row['receiver_uid'] == uid)
                    restore.update(type='configure', receiver_uid=uid,
                                   expected_revision=stopped['revision'])
                restored = await request('/api/sdr/receivers', restore)
                active = next(row for row in restored['snapshot']['receivers']
                              if row['receiver_uid'] != healthy['receiver_uid'])
                assert (active['receiver_uid'] == uid) == (operation != 'retire')
                async with session.ws_connect(base + '/ws/audio', params={
                    'receiver_uid': active['receiver_uid'],
                }) as resumed:
                    await audio_frame(resumed)

    async def bounded():
        await asyncio.wait_for(exercise(), timeout=20)

    asyncio.run(bounded())


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
    monitor = path.with_suffix('.monitor')
    original_monitor = monitor.read_text()
    monitor.write_text('invalid monitor identity\n')
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
        monitor.write_text(original_monitor)
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


def test_daemon_bind_failure_does_not_open_recording_storage(launcher, tmp_path):
    _, env, _, _, _ = launcher
    recording_root = tmp_path / 'recordings-must-not-be-created'
    direct_env = {key: value for key, value in env.items() if key != 'WEBSA_RUST_IQS_SHIM'}
    direct_env['WEBSA_RECORDING_DIR'] = str(recording_root)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', int(env['WEBSA_PORT'])))
        listener.listen()
        # Bypass the launcher's own port guard to exercise the daemon's startup.
        result = subprocess.run([env['WEBSA_BIN']], env=direct_env, cwd=tmp_path,
                                text=True, capture_output=True, timeout=25)
        assert result.returncode != 0, result.stdout
        assert 'Address already in use' in result.stderr
        assert not recording_root.exists()
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


def test_crashed_process_recovers_automatically_and_stop_cancels_retry(launcher):
    invoke, _, _, state_file, runtime = launcher
    assert invoke('run.sh').returncode == 0
    pid = int(state_file().read_text().splitlines()[0])
    os.kill(pid, signal.SIGKILL)
    deadline = time.monotonic() + 15
    while invoke('status.sh', '--quiet').returncode != 0:
        assert time.monotonic() < deadline
        time.sleep(.1)
    assert int(state_file().read_text().splitlines()[0]) != pid
    replacement = int(state_file().read_text().splitlines()[0])
    os.kill(replacement, signal.SIGKILL)
    assert invoke('stop.sh').returncode == 0
    time.sleep(1.2)
    assert invoke('status.sh', '--quiet').returncode != 0
    assert not list(runtime.glob('harogic-websa/*/websad.monitor'))


def test_stop_escalates_only_the_owned_process(launcher):
    invoke, _, _, state_file, _ = launcher
    assert invoke('run.sh').returncode == 0
    pid = int(state_file().read_text().splitlines()[0])
    os.kill(pid, signal.SIGSTOP)
    result = invoke('stop.sh')
    assert result.returncode == 0, result.stderr
    assert invoke('status.sh', '--quiet').returncode != 0


def test_monitor_retries_exec_failure_until_binary_is_available(launcher, tmp_path):
    invoke, env, _, state_file, _ = launcher
    binary = tmp_path / 'recoverable-websad'
    shutil.copy2(env['WEBSA_BIN'], binary)
    assert invoke('run.sh', '--bin', str(binary)).returncode == 0
    pid = int(state_file().read_text().splitlines()[0])
    hidden = binary.with_suffix('.offline')
    binary.rename(hidden)
    try:
        os.kill(pid, signal.SIGKILL)
        log = state_file().with_suffix('.log')
        deadline = time.monotonic() + 10
        while 'No such file' not in log.read_text():
            assert time.monotonic() < deadline
            time.sleep(.1)
        status = invoke('status.sh')
        assert status.returncode != 0 and 'recovering' in status.stdout
    finally:
        hidden.rename(binary)
    deadline = time.monotonic() + 15
    while invoke('status.sh', '--quiet').returncode != 0:
        assert time.monotonic() < deadline
        time.sleep(.1)
    assert int(state_file().read_text().splitlines()[0]) != pid


@pytest.mark.parametrize('repair', [
    pytest.param({'nr_algorithm': 'dfn'}, id='working-neural-model'),
    pytest.param({'nr': False}, id='disable-failed-model'),
    pytest.param({'nr_algorithm': 'wiener'}, id='replace-failed-model'),
])
def test_status_exposes_recovery_permission_and_rejection_preserves_failed_receiver(launcher, repair, tmp_path):
    invoke, env, _, _, _ = launcher
    broken_runtime = tmp_path / 'invalid-onnxruntime.so'
    broken_runtime.write_bytes(b'controlled invalid runtime for model preparation failure')
    env['WEBSA_ORT_DYLIB'] = str(broken_runtime)
    started = invoke('run.sh')
    assert started.returncode == 0, started.stderr
    origin = f'http://127.0.0.1:{env["WEBSA_PORT"]}'

    async def exercise():
        async with aiohttp.ClientSession() as client, AsyncExitStack() as subscriptions:
            async def command(payload, status=200):
                async with client.post(origin + '/api/config', json=payload) as response:
                    assert response.status == status
                    return await response.json()

            async def snapshot(path):
                async with client.get(origin + path) as response:
                    assert response.status == 200
                    return await response.json()

            assert (await command({'cmd': 'SET_MODE', 'mode': 'sdr'}))['cmd'] == 'STATUS'
            accepted = await command({'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True,
                'mode': 'am', 'audio_rate': 24000, 'nr': True, 'nr_algorithm': 'fastenhancer'})
            assert accepted['cmd'] == 'STATUS', accepted
            receiver = (await snapshot('/api/sdr/receivers'))['receivers'][0]
            try:
                async with asyncio.timeout(15):
                    while True:
                        receiver = (await snapshot('/api/sdr/receivers'))['receivers'][0]
                        if receiver['processing_state'] == 'failed' and receiver['reception'] is not None:
                            break
                        await asyncio.sleep(0.02)
            except TimeoutError:
                pytest.fail(f'expected selected-model failure; current receiver: {receiver}')
            assert 'selected fastenhancer model preparation failed' in receiver['processing_fault']['error']
            assert receiver['reception']['manual_recovery_allowed'] is False
            status = await snapshot('/api/state')
            channel = next(row for row in status['sdr']['channels'] if row['channel'] == 1)
            assert 'reception' in channel, 'STATUS omitted the recovery authority'
            # STATUS is published separately from the live UID directory.
            async with asyncio.timeout(5):
                while channel['reception'] is None:
                    await asyncio.sleep(0.02)
                    status = await snapshot('/api/state')
                    channel = next(row for row in status['sdr']['channels'] if row['channel'] == 1)
            assert channel.get('reception') == receiver['reception'], (
                'main-panel STATUS must carry the same recovery authority as the UID directory',
                channel.get('reception'), receiver['reception'])

            before = {key: receiver[key] for key in ('receiver_uid', 'revision', 'confirmed', 'stream')}
            generation = receiver['processing_runtime']['processing_generation']
            refused = await command({'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'recover': True}, status=400)
            assert refused['code'] == 'bad_parameter', refused
            assert 'requires source repair' in refused['error'], refused
            after = (await snapshot('/api/sdr/receivers'))['receivers'][0]
            assert {key: after[key] for key in before} == before
            assert after['processing_state'] == 'failed'
            assert after['processing_runtime']['processing_generation'] == generation

            await command({'cmd': 'SET_SDR_CHANNEL', 'channel': 1, **repair})
            repaired = (await snapshot('/api/sdr/receivers'))['receivers'][0]
            assert repaired['processing_fault'] is None, ('old fault blocked configuration repair', repair, repaired)
            audio = await subscriptions.enter_async_context(client.ws_connect(
                origin + '/ws/audio', params={'receiver_uid': receiver['receiver_uid']}))
            try:
                async with asyncio.timeout(15):
                    while True:
                        recovered = (await snapshot('/api/sdr/receivers'))['receivers'][0]
                        if (recovered['processing_state'] == 'active'
                                and recovered['reception']['state'] == 'receiving'):
                            break
                        await asyncio.sleep(0.02)
            except TimeoutError:
                pytest.fail(f'configuration repair {repair} did not recover: {recovered}')
            assert recovered['receiver_uid'] == before['receiver_uid']
            assert recovered['processing_fault'] is None
            assert recovered['processing_runtime']['processing_generation'] != generation
            assert recovered['reception']['interruptions'] == receiver['reception']['interruptions']
            assert recovered['reception']['last_interruption']['recovered_at_ms'] is not None
            if 'audio_rate' not in repair:
                assert recovered['stream'] == before['stream'], 'NR repair must preserve the IQ stream'
            for key, value in repair.items():
                assert recovered['confirmed']['processors'][0][key] == value

            previous = None
            for _ in range(3):
                while True:
                    message = await audio.receive(timeout=5)
                    assert message.type == aiohttp.WSMsgType.BINARY, message
                    frame = message.data
                    assert frame[:4] == b'AUD2' and len(frame) >= 96
                    sequence, count = struct.unpack_from('<II', frame, 8)
                    if count:
                        break
                channels = struct.unpack_from('<I', frame, 56)[0]
                assert channels == 1 and len(frame) == 96 + count * 4
                assert struct.unpack_from('<d', frame, 16)[0] == repair.get('audio_rate', 24000)
                assert all(math.isfinite(sample[0]) for sample in struct.iter_unpack('<f', frame[96:]))
                sample_index = struct.unpack_from('<Q', frame, 64)[0]
                current = (sequence, count, sample_index, frame[32:40])
                if previous is not None:
                    assert current[0] == previous[0] + 1
                    assert current[2] == previous[2] + previous[1]
                    assert current[3] == previous[3]
                previous = current

    asyncio.run(exercise())
