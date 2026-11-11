"""Check first-run orchestration without downloading or compiling dependencies."""
from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]


@pytest.fixture
def setup_checkout(tmp_path):
    checkout = tmp_path / 'checkout with spaces'
    checkout.mkdir()
    shutil.copy2(ROOT / 'setup.sh', checkout / 'setup.sh')
    (checkout / 'runtime/linux-x86_64').mkdir(parents=True)
    (checkout / 'runtime/linux-x86_64/libonnxruntime.so.1.23.2').touch()
    calls = tmp_path / 'calls'
    commands = tmp_path / 'commands'
    commands.mkdir()
    prefix = tmp_path / 'data with spaces/websa/decoders'
    env = {k: v for k, v in os.environ.items() if not k.startswith('WEBSA_')}
    env.update(PATH=f'{commands}:{env["PATH"]}', XDG_DATA_HOME=str(prefix.parents[1]),
               SDK_ROOT=str(tmp_path / 'no-sdk'), SETUP_CALLS=str(calls),
               CXX='test-cxx', WEBSA_BUILD_PYTHON='test-python')

    def command(name, body):
        path = commands / name
        path.write_text('#!/bin/bash\nset -eu\n' + body + '\n')
        path.chmod(0o755)

    command('rustc', 'echo "rustc 1.95.0"')
    for name in ('node', 'pkg-config', 'test-python', 'test-cxx'):
        command(name, 'exit 0')
    command('make', 'printf "make %s\\n" "$*" >> "$SETUP_CALLS"')
    command('cargo', '''printf 'cargo %s\n' "$*" >> "$SETUP_CALLS"
mkdir -p backend/target/release
printf '#!/bin/sh\nexit 0\n' > backend/target/release/websa-gnss
chmod +x backend/target/release/websa-gnss''')
    tools = checkout / 'tools'
    tools.mkdir()
    for name in ('sdr-decoders', 'rtty-decoder', 'gnss-decoder'):
        (tools / f'setup-{name}.sh').write_text(
            f'printf "{name} %s\\n" "$*" >> "$SETUP_CALLS"\n')

    def invoke(*args):
        return subprocess.run([str(checkout / 'setup.sh'), *args], env=env,
                              text=True, capture_output=True, timeout=10)

    return invoke, prefix, calls, command


def test_full_setup_uses_shared_directory_and_keeps_service_launch_explicit(setup_checkout):
    invoke, prefix, calls, _ = setup_checkout
    result = invoke()
    assert result.returncode == 0, result.stderr
    lines = calls.read_text().splitlines()
    assert lines[0] == 'make frontend native'
    assert lines[1] == 'sdr-decoders multimon rtl433 onair'
    assert lines[2] == 'rtty-decoder '
    assert '--bin websa-gnss' in lines[3]
    assert lines[4] == 'gnss-decoder '
    assert (prefix / 'bin/websa-gnss').is_file()
    assert 'WEBSA_FAKE=1 make run' in result.stdout
    assert 'make restart' in result.stdout
    assert not any('make run' in line or 'make restart' in line for line in lines)


def test_installed_decoders_are_reused_without_gps_build_dependencies(setup_checkout):
    invoke, prefix, calls, command = setup_checkout
    (prefix / 'bin').mkdir(parents=True)
    for name in ('multimon-ng', 'rtl_433', 'onair-server', 'websa-rtty', 'gnss-sdr', 'websa-gnss'):
        executable = prefix / 'bin' / name
        executable.write_text('#!/bin/sh\nexit 0\n')
        executable.chmod(0o755)
    command('test-python', 'exit 1')
    result = invoke()
    assert result.returncode == 0, result.stderr
    assert calls.read_text().splitlines() == ['make frontend native']


def test_preflight_and_missing_dependencies_do_not_start_installation(setup_checkout):
    invoke, prefix, calls, command = setup_checkout
    checked = invoke('--check')
    assert checked.returncode == 0, checked.stderr
    assert not calls.exists() and not prefix.exists()
    command('pkg-config', 'exit 1')
    command('test-python', 'exit 1')
    failed = invoke()
    assert failed.returncode != 0
    assert 'fdk-aac' in failed.stderr and 'gnuradio-runtime' in failed.stderr
    assert 'Mako' in failed.stderr
    assert not calls.exists() and not prefix.exists()
