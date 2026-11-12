"""A failed IQ recording must not strand the bench's audio recording."""
import io
import json
import runpy
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

import pytest


@pytest.mark.parametrize('failure', ['failed_iq', 'stop_iq'])
def test_scan_bench_cleans_every_owned_recording_and_preserves_primary_error(monkeypatch, tmp_path, failure):
    state = dict(service_id='fixture', capture=dict(state='running', center_hz=100e6),
                 recordings=[], receivers=[dict(receiver_uid='primary', display_number=1,
                                               confirmed=dict(baseband=dict(frequency_hz=100e6), tags=[]))])
    stopped, deleted = [], []

    def request(req, **_kwargs):
        payload = json.loads(req.data) if req.data else None
        if '/api/sdr/scan' in req.full_url:
            result = dict(service_id='fixture', scan_id='scan-fixture', revision='1', capture=dict(epoch='1'))
        elif urlsplit(req.full_url).path.endswith('/index'):
            query = parse_qs(urlsplit(req.full_url).query)
            offset = int(query['offset'][0])
            positions = range(129) if query['volume'][0] == 'iq-000000.jsonl' else range(129, 132)
            entries = [dict(meta=dict(clock=dict(first_output_index=str(i), samples='1'))) for i in positions]
            result = dict(offset=str(offset), next_offset=str(min(offset + 128, len(entries))), entries=entries[offset:offset + 128])
        elif payload is None:
            result = state
        else:
            kind = payload['type']
            task_id = payload.get('task_id')
            if kind == 'record_start':
                track = payload['kinds'][0]
                state['recordings'].append(dict(task_id=track, receiver_uid='primary', state='recording',
                    tracks=[dict(blocks='132', volumes=[dict(index='iq-000000.jsonl'), dict(index='iq-000001.jsonl')],
                                 segments=[dict(stream=dict(epoch='1'), samples='132')])]))
            elif kind == 'record_stop':
                stopped.append(task_id)
                if task_id == 'iq' and failure == 'stop_iq':
                    raise urllib.error.URLError('fixture stop failure')
                next(t for t in state['recordings'] if t['task_id'] == task_id)['state'] = (
                    'failed' if task_id == 'iq' else 'completed')
            elif kind == 'record_delete':
                deleted.append(task_id)
                state['recordings'] = [t for t in state['recordings'] if t['task_id'] != task_id]
            else:
                raise AssertionError(payload)
            result = dict(snapshot=state, fault=None)
        return io.BytesIO(json.dumps(result).encode())

    def fail_measurement(_seconds):
        raise RuntimeError('fixture measurement failure')

    original_iterdir, original_read = Path.iterdir, Path.read_text
    monkeypatch.setattr(Path, 'iterdir', lambda p: iter([Path('/proc/424242')]) if p == Path('/proc') else original_iterdir(p))
    monkeypatch.setattr(Path, 'read_text', lambda p, *a, **kw: 'websad' if str(p) == '/proc/424242/comm' else original_read(p, *a, **kw))
    monkeypatch.setattr(urllib.request, 'urlopen', request)
    monkeypatch.setattr(time, 'sleep', fail_measurement)
    monkeypatch.setenv('WEBSA_SCAN_LIVE', '1')
    monkeypatch.setattr(sys, 'argv', ['bench-signal-scan.py', '--artifacts', str(tmp_path)])
    with pytest.raises(RuntimeError, match='fixture measurement failure'):
        runpy.run_path(str(Path(__file__).resolve().parents[1] / 'tools/bench-signal-scan.py'), run_name='__main__')
    assert stopped == ['iq', 'audio']
    assert deleted == ['iq', 'audio']
    assert not state['recordings']
    report = json.loads((tmp_path / 'performance-summary.json').read_text())
    assert report['audio_recording']['state'] == 'completed'
    assert report['cleanup_errors']
    if failure == 'failed_iq':
        recorded = json.loads((tmp_path / 'iq-recording-continuity.json').read_text())
        assert len(recorded['index']['entries']) == 132
        assert len(recorded['index']['volumes']) == 2
        assert recorded['index']['samples'] == '132'
        assert recorded['index']['complete']
