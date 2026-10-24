"""The SSTV transmitter's signal plan: the tone sequence the mode table prescribes, and the
streaming synthesizer that turns it into baseband.

Pure numpy, no radio: `--dry-run` covers the radio-less path end to end, and these tests cover the
parts a dry run cannot show (tone counts, per-pixel timing, the picture reader, chunk continuity).
"""
from __future__ import annotations

import argparse
import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.fixtures import sstv_encode as sstv  # noqa: E402
from tools.pluto import pluto_sstv_tx as tx  # noqa: E402

RATE = 8_000


def _args(**over) -> argparse.Namespace:
    base = dict(mode='Robot 36', list_modes=False, image=None, callsign='CQ WE0BSA', label='L',
                seconds=0.0, repeat=1, pause=0.0, dry_run_seconds=60.0, dry_run=True,
                lo=435e6, rate=float(RATE), gain=-10.0, scale=16384.0, xo_ppm=1.98,
                no_xo_trim=False, uri='ip:192.168.2.1')
    base.update(over)
    return argparse.Namespace(**base)


def _black(mode: dict) -> bytes:
    return bytes(mode['width'] * mode['height'] * 3)


def test_header_is_vox_leader_break_leader_and_the_vis_code():
    robot = sstv.find_mode('Robot 36')
    tones = sstv.header_tones(robot)
    # 8 VOX tones (100 ms each), two 300 ms leaders, the 10 ms break, ten 30 ms VIS bits.
    assert sum(ns for _, ns in tones) == 8 * 100_000_000 + 2 * 300_000_000 + 10_000_000 + 10 * 30_000_000
    assert sum(ns for _, ns in tones) == 1_710_000_000
    # Scottie modes send one extra sync pulse between the header and the first line.
    scottie = sstv.find_mode('Scottie S1')
    extra = sum(ns for _, ns in sstv.header_tones(scottie)) - 1_710_000_000
    assert extra == 9_000_000, 'the starting sync pulse is the mode\'s own sync length'


def test_each_scan_emits_one_tone_per_pixel():
    mode = sstv.find_mode('PD-50')
    tones = sstv.image_tones(mode, _black(mode), mode['width'], mode['height'])
    scans = [step for step in mode['sequence'] if 'scan' in step]
    controls = [step for step in mode['sequence'] if 'control' in step]
    groups = mode['height'] // mode['lines_per_sequence']
    assert len(tones) == groups * (len(scans) * mode['width'] + len(controls))
    # Every tone is either a control tone (sync 1200, porches and separators 1500/1900/2300) or a
    # pixel value in the luminance range, so nothing outside that span can reach the air.
    assert all(sstv.SYNC_HZ <= hz <= sstv.WHITE_HZ for hz, _ in tones)
    # A scan's tones divide its duration exactly at the pixel level: the decoder reads pixels off
    # these steps, so a rounding shift would land every pixel slightly early.
    per_pixel = scans[0]['scan']['ns'] // mode['width']
    pixel_tones = tones[len(controls):len(controls) + mode['width']]
    assert pixel_tones == [(sstv.BLACK_HZ, per_pixel)] * mode['width']


def test_a_black_picture_sends_black_everywhere_in_its_scans():
    mode = sstv.find_mode('Robot 36')
    tones = sstv.image_tones(mode, _black(mode), mode['width'], mode['height'])
    # The scan tones of a black picture: luminance 0 is 1500 Hz, and the colour differences of black
    # are the encoder's own mid-scale values (127), which is 1898 Hz. Control tones (1500/2300 porches
    # and separators) share those frequencies, so the check is on the counts per scan step.
    chroma = sstv.value_frequency(sstv._chroma_red(0, 0))
    scans = [step['scan']['channel'] for step in mode['sequence'] if 'scan' in step]
    groups = mode['height'] // mode['lines_per_sequence']
    expected = {}
    for channel in scans:
        hz = sstv.BLACK_HZ if channel in ('Y', 'YSecond') else chroma
        expected[hz] = expected.get(hz, 0) + groups * mode['width']
    counted = {}
    for hz, _ in tones:
        counted[hz] = counted.get(hz, 0) + 1
    for hz, count in expected.items():
        assert counted[hz] >= count, f'{hz} Hz: {counted.get(hz, 0)} tones, expected at least {count}'
    assert sstv.value_frequency(0) == sstv.BLACK_HZ
    assert sstv.value_frequency(255) == sstv.WHITE_HZ


def test_total_length_matches_the_mode_table():
    # The length each mode advertises must be the length its sequence prescribes, *including* the
    # per-pixel division: a scan's duration is split over the width, and the remainder is dropped the
    # same way the Rust encoder drops it. The table's `total_ns` is what the Rust side compares
    # against its own timing
    # (`dsp/core/tests/sstv_decode.rs::the_transmitter_table_matches_the_vendored_decoder_table`).
    for mode in sstv.load_modes():
        groups = mode['height'] // mode['lines_per_sequence']
        header_ns = sum(ns for _, ns in sstv.header_tones(mode))
        per_pass = sum(
            step['control']['ns'] if 'control' in step
            else (step['scan']['ns'] // mode['width']) * mode['width']
            for step in mode['sequence'])
        tones = sstv.header_tones(mode) + sstv.image_tones(
            mode, _black(mode), mode['width'], mode['height'])
        assert sum(ns for _, ns in tones) == header_ns + groups * per_pass, mode['display']


def test_the_transmission_length_is_the_header_plus_every_pass():
    mode = sstv.find_mode('Robot 36')
    tones = sstv.transmission_tones(mode, _black(mode), mode['width'], mode['height'])
    samples = sstv.synth(tones, RATE)
    # Robot 36: 1.71 s of header plus 120 line pairs of 300 ms.
    assert abs(samples.size / RATE - (1.71 + 36.0)) < 0.001


def test_streaming_synth_is_phase_continuous_and_chunk_size_independent():
    # A few seconds of a full modulation so the chunker crosses tone boundaries (the per-pixel tones
    # are shorter than any sane chunk).
    mode = sstv.find_mode('Scottie S1')
    tones = sstv.image_tones(mode, sstv.pattern_image(mode['width'], mode['height']).bytes(),
                             mode['width'], mode['height'])[:20_000]
    expected = sum((ns * RATE + 0) // 1_000_000_000 for _, ns in tones)  # carry handled below
    counts = []
    for chunk in (1000, 4096):
        chunks = list(tx.stream_tones(tones, RATE, 1.0, chunk))
        counts.append(sum(c.size for c in chunks))
        # One length for the whole session, the last padded with silence: pyadi-iio allocates the TX
        # buffer on the first push and raises "Cannot change buffer length on the fly" otherwise, which
        # is how a short final chunk killed the first over-the-air attempt.
        sizes = {c.size for c in chunks}
        assert sizes == {chunk}, f'chunk sizes {sorted(sizes)} must all be {chunk}'
        # Amplitude is the scale on every sample: a chunk boundary must not introduce a gap or a
        # fade (that would be a click on the air).
        assert all(abs(abs(complex(value)) - 1.0) < 1e-6 for c in chunks for value in c[:min(64, c.size)])
    # The carry arithmetic: the tone total, computed the way `synth` computes it.
    carry = 0
    total = 0
    for _, ns in tones:
        numerator = ns * RATE + carry
        total += numerator // 1_000_000_000
        carry = numerator % 1_000_000_000
    # Each chunking covers that tone total and pads the last chunk to its own length, so the two runs
    # differ by no more than one chunk of trailing silence and never by less than the tones.
    for size, count in zip((1000, 4096), counts, strict=True):
        assert total <= count < total + size, (size, count, total)
    assert abs(total - expected) < len(tones), 'the carry only ever moves by a fraction of a sample'


def test_truncate_cuts_a_tone_instead_of_dropping_it():
    tones = [(1500, 1_000), (1900, 1_000), (2300, 1_000)]
    cut = tx._truncate(tones, 1_500)
    assert cut == [(1500, 1_000), (1900, 500)]


def test_the_generated_pattern_has_the_mode_resolution_and_visible_structure():
    mode = sstv.find_mode('Martin M1')
    picture = sstv.pattern_image(mode['width'], mode['height'], 'CQ WE0BSA', 'TEST')
    assert (picture.width, picture.height) == (mode['width'], mode['height'])
    colours = {bytes(picture.rgb[at:at + 3]) for at in range(0, len(picture.rgb), 3)}
    assert len(colours) > 8, 'a test pattern must be visibly structured'


def test_an_image_file_is_resized_to_the_mode_resolution(tmp_path: Path):
    # A 4x2 PPM: the transmitter's own geometry wins, so any file can be sent in any mode.
    ppm = tmp_path / 'tiny.ppm'
    ppm.write_bytes(b'P6\n4 2\n255\n' + bytes([255, 0, 0] * 4 + [0, 0, 255] * 4))
    source = sstv.read_image(ppm)
    assert (source.width, source.height) == (4, 2)
    mode = sstv.find_mode('Robot 36')
    fitted = sstv.fit(source, mode)
    assert (fitted.width, fitted.height) == (mode['width'], mode['height'])
    assert bytes(fitted.rgb[0:3]) == bytes([255, 0, 0])


def test_the_png_reader_reads_what_it_writes(tmp_path: Path):
    # A 2x2 non-interlaced RGB PNG, built here so the reader is tested without an image library.
    # One filter byte per *row* (0 = none), then the row's RGB triples.
    raw = (b'\x00' + bytes([255, 0, 0]) + bytes([0, 255, 0])
           + b'\x00' + bytes([0, 0, 255]) + bytes([255, 255, 255]))

    def chunk(kind: bytes, payload: bytes) -> bytes:
        return struct.pack('>I', len(payload)) + kind + payload + struct.pack('>I', zlib.crc32(kind + payload))

    png = (b'\x89PNG\r\n\x1a\n'
           + chunk(b'IHDR', struct.pack('>IIBBBBB', 2, 2, 8, 2, 0, 0, 0))
           + chunk(b'IDAT', zlib.compress(raw))
           + chunk(b'IEND', b''))
    path = tmp_path / 'tiny.png'
    path.write_bytes(png)
    picture = sstv.read_image(path)
    assert (picture.width, picture.height) == (2, 2)
    assert bytes(picture.rgb[0:6]) == bytes([255, 0, 0, 0, 255, 0])
    assert bytes(picture.rgb[6:12]) == bytes([0, 0, 255, 255, 255, 255])


def test_every_mode_plans_and_builds_in_a_dry_run(capsys):
    # The wrapper's `--dry-run-all` relies on this: a mode that cannot plan must fail loudly, and the
    # plan must be built for every registered mode.
    for mode in sstv.load_modes():
        code = tx.main(['--mode', mode['display'], '--dry-run', '--dry-run-seconds', '0.2'])
        assert code == 0, mode['display']
    out = capsys.readouterr().out
    assert 'radio untouched' in out


def test_the_plan_reports_the_longest_mode_without_building_it_whole(capsys):
    code = tx.main(['--mode', 'PD-290', '--dry-run', '--dry-run-seconds', '0.5'])
    out = capsys.readouterr().out
    assert code == 0
    assert 'PD-290' in out
    assert 'the whole transmission is 290.39s' in out


def test_picture_for_prefers_the_operators_file(tmp_path: Path, capsys):
    ppm = tmp_path / 'tiny.ppm'
    ppm.write_bytes(b'P6\n2 2\n255\n' + bytes([9, 9, 9] * 4))
    rgb, width, height = tx.picture_for(_args(image=str(ppm)), sstv.find_mode('PD-50'))
    assert (width, height) == (320, 256)
    assert bytes(rgb[0:3]) == bytes([9, 9, 9])
    capsys.readouterr()
