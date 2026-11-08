"""Private offline-study output and manual UI-probe failure contracts."""
import importlib
import io
import json
import struct
import sys
import tarfile
from pathlib import Path
from types import SimpleNamespace

import pytest


@pytest.fixture
def study(monkeypatch):
    directory = Path(__file__).resolve().parents[1] / 'tools/bench/ssb_nr'
    monkeypatch.syspath_prepend(str(directory))
    return importlib.import_module('study')


def archive(path, entries):
    with tarfile.open(path, 'w') as output:
        for name, data in entries:
            member = tarfile.TarInfo(name)
            member.size = len(data)
            output.addfile(member, io.BytesIO(data))


@pytest.mark.parametrize('link_kind', ['directory', 'file'])
def test_study_extraction_does_not_follow_existing_output_links(study, tmp_path, link_kind):
    original = tmp_path / 'original'
    original.mkdir()
    recording = original / 'recording.iq'
    recording.write_bytes(b'private original')
    destination = tmp_path / 'output'
    destination.mkdir()
    if link_kind == 'directory':
        (destination / 'link').symlink_to(original, target_is_directory=True)
        name = 'link/recording.iq'
    else:
        (destination / 'link').symlink_to(recording)
        name = 'link'
    source = tmp_path / 'input.tar'
    archive(source, [(name, b'replacement')])
    with pytest.raises(ValueError, match='unsafe'):
        study.extract(source, destination)
    assert recording.read_bytes() == b'private original'


def test_study_validates_all_names_before_replacing_existing_copies(study, tmp_path):
    destination = tmp_path / 'output'
    destination.mkdir()
    existing = destination / 'existing.iq'
    existing.write_bytes(b'prior copy')
    source = tmp_path / 'input.tar'
    archive(source, [('existing.iq', b'replacement'), ('../escape', b'bad')])
    with pytest.raises(ValueError, match='unsafe'):
        study.extract(source, destination)
    assert existing.read_bytes() == b'prior copy'


def test_study_rejects_normalized_duplicate_archive_names(study, tmp_path):
    source = tmp_path / 'input.tar'
    archive(source, [('recording.iq', b'one'), ('./recording.iq', b'two')])
    destination = tmp_path / 'output'
    with pytest.raises(ValueError, match='unsafe'):
        study.extract(source, destination)
    assert not (destination / 'recording.iq').exists()


def test_study_can_repeat_a_valid_extraction(study, tmp_path):
    source = tmp_path / 'input.tar'
    archive(source, [('nested/recording.iq', b'samples'), ('clock.json', b'{}')])
    destination = tmp_path / 'output'
    for _ in range(2):
        study.extract(source, destination)
        assert (destination / 'nested/recording.iq').read_bytes() == b'samples'
        assert (destination / 'clock.json').read_bytes() == b'{}'


@pytest.mark.parametrize('existing', ['scores.csv', 'notes.txt'])
def test_speech_focus_preserves_incomplete_previous_outputs(study, tmp_path, monkeypatch, existing):
    module = importlib.import_module('run_speech_focus')
    base = tmp_path / 'study'
    base.mkdir()
    out = tmp_path / 'results'
    out.mkdir()
    previous = out / existing
    previous.write_text('private listening notes')
    monkeypatch.setattr(sys, 'argv', ['run_speech_focus.py', '--study', str(base), '--out', str(out)])
    with pytest.raises(ValueError, match='fresh output directory'):
        module.main()
    assert previous.read_text() == 'private listening notes'


@pytest.mark.parametrize('failure', ['console', 'canvas', 'button', 'pcm', None])
def test_manual_ui_probe_fails_and_closes_generator(monkeypatch, failure):
    module = importlib.import_module('tools.sdr_probe.ui_sdr_test')
    events = []
    callbacks = {}
    canvas = iter([1, 1 if failure == 'canvas' else 2])
    header = struct.Struct('<4sIIIddIIQdIIQdQQ')

    def ws_on(_name, receive):
        receive(header.pack(b'AUD2', 1, 0, 0, 48000, 100200000, 1, 2, 0, 0, 1, 1, 0, 1e6, 0, 1))
        receive(header.pack(b'AUD2', 1, 1, 1, 48000, 100200000, 1, 2, 0, 0, 1, 1, 0, 1e6, 0, 1)
                + struct.pack('<f', 0.1))

    def goto(*_args, **_kwargs):
        if failure == 'console':
            callbacks['pageerror']('injected console failure')
        if failure != 'pcm':
            callbacks['websocket'](SimpleNamespace(url='ws://localhost/ws/audio?channel=1', on=ws_on))

    def noop(*_args, **_kwargs):
        return None
    page = SimpleNamespace(on=lambda name, callback: callbacks.update({name: callback}), goto=goto,
                           wait_for_timeout=noop, click=noop, wait_for_selector=noop,
                           wait_for_function=noop, fill=noop, evaluate=lambda *_args: next(canvas),
                           eval_on_selector=lambda selector, *_args: failure != 'button'
                           if selector == '#btn-mode-sdr' else '-23 dBFS', screenshot=noop)
    browser = SimpleNamespace(new_page=lambda **_kwargs: page, close=lambda: events.append('browser_close'))

    class Playwright:
        def __enter__(self):
            return SimpleNamespace(chromium=SimpleNamespace(launch=lambda **_kwargs: browser))

        def __exit__(self, *_args):
            return False

    generator = SimpleNamespace(enter_low_output=noop, am=noop, cmd=lambda *_args: 'ok',
                                off=lambda: events.append('generator_off'),
                                ser=SimpleNamespace(close=lambda: events.append('serial_close')))
    state = {'mode': 'SDR', 'sdr': {'channels': [{'channel': 1, 'channel_id': 1, 'epoch': 2,
                                               'freq': 100200000}]}}
    monkeypatch.setattr(module, 'TinySA', lambda *_args: generator)
    monkeypatch.setattr(module, 'sync_playwright', Playwright)
    monkeypatch.setattr(module.urllib.request, 'urlopen', lambda *_args, **_kwargs:
                        io.BytesIO(json.dumps(state).encode()))
    if failure is None:
        module.main()
    else:
        with pytest.raises(RuntimeError, match='SDR UI'):
            module.main()
    assert events == ['browser_close', 'generator_off', 'serial_close']
