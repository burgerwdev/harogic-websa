#!/usr/bin/env python3
"""Recorded IQ offset tuning, independent tones/clocks/SigMF and real file-player UI."""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import shutil
import struct
import subprocess
import tarfile
import zlib
from pathlib import Path

import numpy as np
from native_multichannel import api
from native_multichannel_demod import browser_gate, fixture, pressure_gate, url, verify
from native_multichannel_processing import assert_receivers_continue, setup
from native_multichannel_recording import finish, read_archive, start, task, until
from native_multichannel_stream_export import get
from rust_iq_browser import free_port, start_service, stop
from scipy.io import wavfile


def split_volumes(path):
    """Repackage independently checked original blocks into two valid source volumes."""
    blocks, complete = read_archive(path / 'iq-000000.wbiq')
    assert complete
    volumes = []
    for number, group in enumerate([blocks[:2], blocks[2:]]):
        page, digest = [], 0
        name = f'iq-{number:06d}.wbiq'
        with (path / name).open('wb') as file:
            file.write(b'WBIQ0002')

            def record(kind, meta, payload=b''):
                nonlocal digest
                encoded = json.dumps(meta, separators=(',', ':')).encode()
                head = struct.pack('<4sII', kind, len(encoded), len(payload))
                crc = zlib.crc32(head + encoded + payload)
                at = file.tell()
                file.write(head + struct.pack('<I', crc) + encoded + payload)
                if kind != b'END2':
                    digest = zlib.crc32(struct.pack('<I', crc), digest)
                return at

            for row, raw in group:
                header = {key: row[key] for key in ['meta', 'codec', 'raw_crc32']}
                at = record(b'BLK2', header, raw)
                page.append({'offset': str(at), **header, 'raw_bytes': len(raw), 'stored_bytes': len(raw)})
            record(b'IDX2', page)
            record(b'END2', {'blocks': str(len(group)), 'samples': str(sum(len(raw) // 8 for _, raw in group)), 'records_crc32': digest})
        assert read_archive(path / name)[1]
        volumes.append({'file': name, 'index': name.replace('.wbiq', '.jsonl'), 'complete': True})
    value = json.loads((path / 'task.json').read_text())
    value['tracks'][0]['volumes'] = volumes
    value['tracks'][0]['stored_bytes'] = str(sum((path / v['file']).stat().st_size for v in volumes))
    (path / 'task.json').write_text(json.dumps(value))


def official_sigmf(result, raw, out, name, python):
    meta = out / (name + '.sigmf-meta')
    meta.write_text(json.dumps(result['source']))
    meta.with_suffix('.sigmf-data').write_bytes(raw[137 * 8:20137 * 8])
    checked = subprocess.run([str(python), '-c',
        'import sigmf,sys,hashlib,json; r=sigmf.fromfile(sys.argv[1]); r.validate(); '
        'assert hashlib.sha256(r.read_samples().tobytes()).hexdigest()==sys.argv[2]; '
        'print(json.dumps({"reader":sigmf.__version__,"samples":len(r)}))', str(meta), result['source_sha256']],
        check=True, capture_output=True, text=True)
    return json.loads(checked.stdout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-file-tuning'))
    parser.add_argument('--sigmf-python', type=Path, default=Path('/tmp/websa-sigmf-venv/bin/python'))
    args = parser.parse_args(); out = args.artifacts.resolve(); out.mkdir(parents=True, exist_ok=True)
    root = out / 'recordings'; os.environ['WEBSA_RECORDING_DIR'] = str(root)
    seeds = []
    for mode in ['am', 'usb', 'lsb', 'nfm']:
        for offset in [-2000.25, 2000.25]:
            name = f'{mode}-{"negative" if offset < 0 else "positive"}'
            task_id, raw = fixture(root, mode, name=name, carrier_offset=offset, passband_hz=12000)
            split_volumes(root / task_id)
            seeds.append((mode, offset, task_id, raw))
    long_id, _ = fixture(root, 'am', rate=6028.163580246914, count=365000, name='pressure', carrier_offset=100.25)
    small_id, small_raw = fixture(root, 'usb', name='sub-hz', carrier_offset=0.001, passband_hz=12000)
    prefix_id = 'file-recovered-offset'
    shutil.copytree(root / seeds[0][2], root / prefix_id)
    value = json.loads((root / prefix_id / 'task.json').read_text()); value['task_id'] = prefix_id
    (root / prefix_id / 'task.json').write_text(json.dumps(value))
    with (root / prefix_id / 'iq-000001.wbiq').open('r+b') as file:
        file.truncate(file.seek(0, 2) - 64)
    proc = None
    with (out / 'service.log').open('w') as log:
        try:
            proc, origin = start_service(free_port(), log=log)
            setup(origin)
            assert api(origin)['capabilities']['iq_file_demodulate_offset'] is True
            uid = api(origin)['receivers'][0]['receiver_uid']
            active_id = start(origin, uid, 'live-during-offset-tuning')
            until(lambda: int(task(origin, active_id)['tracks'][0]['raw_bytes']) > 48000 * 8)
            before = api(origin)
            originals = {p: hashlib.sha256(p.read_bytes()).hexdigest() for p in root.glob('file-*/*')}
            results = []
            for mode, offset, task_id, raw in seeds:
                saved = task(origin, task_id)
                assert len(saved['tracks'][0]['volumes']) == 2
                with get(origin, url(saved, mode=mode, offset_hz=offset)) as response:
                    data = response.read(); assert len(data) == int(response.headers['Content-Length'])
                (out / f'{task_id}.tar').write_bytes(data)
                result = verify(data, raw, saved, tuning_offset=offset)
                result['independent_sigmf'] = official_sigmf(result, raw, out, task_id, args.sigmf_python)
                assert result['source']['annotations'][0]['websa:clock']['first_input_index'] == '18446744073700000000'
                assert result['source']['annotations'][0]['websa:clock']['fraction_num'] == '1'
                assert result['source']['annotations'][0]['websa:clock']['fraction_den'] == '3'
                results.append(result)
            saved = task(origin, seeds[0][2])
            for offset, status in [('NaN', 400), ('inf', 400), ('', 400), ('bad', 400), (1e300, 422), (48840.130763116205, 422), (-48840.130763116205, 422)]:
                get(origin, url(saved, offset_hz=offset), status).close()
            # Test full translated bands, including the asymmetric sideband edges.
            for mode, boundary, outside in [('am', 5250, 5250.01), ('nfm', -5250, -5250.01), ('usb', 4500, 4500.01), ('lsb', -4500, -4500.01)]:
                with get(origin, url(saved, mode=mode, offset_hz=boundary)) as response:
                    assert response.read()
                get(origin, url(saved, mode=mode, offset_hz=outside), 422).close()
            with get(origin, url(saved)) as response:
                implicit_zero = response.read()
            with get(origin, url(saved, offset_hz=0)) as response:
                assert response.read() == implicit_zero
            recovered = task(origin, prefix_id); assert recovered['state'] == 'failed'
            with get(origin, url(recovered, offset_hz=-2000.25)) as response:
                prefix_data = response.read()
            (out / 'recovered-offset.tar').write_bytes(prefix_data)
            report = {'source_kind': 'synthetic known-tone IQ at original noninteger rate; independently repackaged across volumes',
                'modes': results, 'zero_offset_identical': True, 'recovered_prefix': verify(prefix_data, seeds[0][3], recovered, tuning_offset=-2000.25)}
            # This must fail if the file path accidentally calls the live NCO's
            # near-zero bypass: the two actual WAV payloads would be identical.
            small = task(origin, small_id)
            waves = []
            for offset in [0, 0.001]:
                with get(origin, url(small, mode='usb', offset_hz=offset)) as response:
                    data = response.read()
                with tarfile.open(fileobj=io.BytesIO(data)) as archive:
                    rate, samples = wavfile.read(io.BytesIO(archive.extractfile('audio.wav').read()))
                assert rate == 48000 and samples.dtype == np.float32
                waves.append(samples)
            assert waves[0].shape == waves[1].shape
            delta = float(np.max(np.abs(waves[0] - waves[1])))
            assert 1e-5 < delta < 0.005, delta
            report['sub_hz'] = {'pcm_peak_difference_from_zero': delta, 'render': verify(data, small_raw, small, tuning_offset=0.001)}
            report['pressure'] = pressure_gate(origin, task(origin, long_id), offset_hz=100.25)
            browser = seeds[3]  # USB positive offset: audible pitch establishes the mixing sign.
            report['browser'] = browser_gate(origin, task(origin, browser[2]), browser[3], out, tuning_offset=browser[1], mode='usb')
            assert_receivers_continue(before['receivers'], api(origin)['receivers'])
            old = next(t for t in before['recordings'] if t['task_id'] == active_id)
            assert int(task(origin, active_id)['tracks'][0]['raw_bytes']) > int(old['tracks'][0]['raw_bytes'])
            report['healthy_recording'] = finish(origin, active_id)
            assert all(hashlib.sha256(p.read_bytes()).hexdigest() == digest for p, digest in originals.items())
            report['original_files_unchanged'] = len(originals)
            with (root / seeds[-1][2] / 'iq-000001.wbiq').open('r+b') as file:
                file.seek(30); file.write(b'X')
            get(origin, url(task(origin, seeds[-1][2]), mode='nfm', offset_hz=2000.25), 422).close()
            with get(origin, url(saved, offset_hz=-2000.25)) as response:
                assert response.read()
            report['snapshot'] = api(origin)
            (out / 'report.json').write_text(json.dumps(report, indent=2))
            print(json.dumps({'passed': True, 'shifted_renders': len(results), 'artifacts': str(out)}))
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
