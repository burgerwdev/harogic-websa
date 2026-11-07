"""Fixture construction must size bit sequences from duration, not sample count."""
import numpy as np

from tools.fixtures import gen_dsp_fixtures as fixtures


def test_stereo_rds_fixture_generates_only_the_bits_its_samples_reference(monkeypatch):
    tile = np.tile
    allocations = []
    def bounded_tile(data, repetitions):
        allocations.append(data.size * repetitions)
        assert data.size * repetitions <= 1664, f'unnecessary RDS bit allocation: {allocations}'
        return tile(data, repetitions)
    monkeypatch.setattr(fixtures.np, 'tile', bounded_tile)
    iq, rate, _deviation = fixtures._stereo_rds_baseband()
    assert iq.size == 345600 and rate == 288000
    assert np.isfinite(iq).all()
