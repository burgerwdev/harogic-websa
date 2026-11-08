"""Automatic setup respects explicit paths and verifies the cached encoder."""
import json
from pathlib import Path

import pytest

from tools.pluto import source_setup


def test_default_encoder_is_built_once_and_revalidated_after_corruption(tmp_path, monkeypatch):
    binary = tmp_path / 'encoder'
    monkeypatch.setattr(source_setup, 'digital_helper', lambda: binary)
    calls = []

    def install(command, **_kwargs):
        calls.append(command)
        binary.write_bytes(b'#!/bin/sh\nexit 0\n')
        binary.chmod(0o755)
        Path(str(binary) + '.json').write_text(json.dumps({
            'revision': source_setup.ONAIR_REVISION, 'encoder_sha256': source_setup.digest(binary),
            'wrapper_sha256': source_setup.digest(source_setup.HERE / 'pluto_digital_source.cpp'),
            'build_sha256': source_setup.digest(source_setup.HERE / 'build-digital-source.sh'),
        }))

    monkeypatch.setattr(source_setup.subprocess, 'run', install)
    assert source_setup.ensure_digital(binary) == binary
    assert len(calls) == 1
    source_setup.ensure_digital(binary)
    assert len(calls) == 1
    binary.write_bytes(b'truncated binary')
    source_setup.ensure_digital(binary)
    assert len(calls) == 2 and source_setup.prepared_digital(binary)


def test_offline_or_explicit_missing_encoder_does_not_download(tmp_path, monkeypatch):
    monkeypatch.setattr(source_setup, 'digital_helper', lambda: tmp_path / 'default')
    calls = []
    monkeypatch.setattr(source_setup.subprocess, 'run', lambda *args, **kwargs: calls.append(args))
    for path, setup in ((tmp_path / 'default', False), (tmp_path / 'custom', True)):
        with pytest.raises(ValueError, match='--prepare'):
            source_setup.ensure_digital(path, setup=setup)
    with pytest.raises(ValueError, match='gps --prepare'):
        source_setup.ensure_gps(tmp_path / 'gps', setup=False)
    assert not calls
