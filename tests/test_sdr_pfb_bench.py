from pathlib import Path

import numpy as np
import pytest

from tools.bench.sdr_pfb_bench import fixture, percentile_ms, run


def test_fixture_is_repeatable_and_tail_stats_are_explicit():
    a = fixture(31_250_000.0, 3, 64, 42)
    assert a.shape == (3, 128) and a.dtype == np.int16
    np.testing.assert_array_equal(a, fixture(31_250_000.0, 3, 64, 42))
    assert not np.array_equal(a, fixture(31_250_000.0, 3, 64, 43))
    stats = percentile_ms([1000, 2000, 3000, 4000, 5000])
    assert stats == {'p50_ms': 0.003, 'p95_ms': 0.0048,
                     'p99_ms': 0.005, 'max_ms': 0.005}


def test_raw_replay_rejects_wrong_geometry(tmp_path):
    library = Path(__file__).resolve().parents[1] / 'native-ddc/target/release/libwebsa_channelizer.so'
    if not library.exists():
        pytest.skip('build native-ddc first')
    raw = tmp_path / 'iq.i16'
    raw.write_bytes(b'\x00' * 16)
    with pytest.raises(ValueError, match='recorded fixture'):
        run(library, 62_500_000.0, 1, 1, raw)
