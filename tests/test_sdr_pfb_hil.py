import threading
from dataclasses import replace
from pathlib import Path

import pytest

from tools.bench.sdr_pfb_hil import FixtureSource, PfbApi, run_native_pool, run_pipeline


@pytest.fixture
def api():
    path = Path(__file__).resolve().parents[1] / 'native-ddc/target/release/libwebsa_channelizer.so'
    if not path.exists():
        pytest.skip('build native-ddc first')
    instance = PfbApi(path, 62_500_000.0, [0, 5, 31, 127])
    try:
        yield instance
    finally:
        instance.close()


@pytest.mark.parametrize('strategy', [0, 1])
def test_native_pool_fixture_is_ordered_and_faster_than_packet_clock(strategy):
    path = Path(__file__).resolve().parents[1] / 'native-ddc/target/release/libwebsa_channelizer.so'
    if not path.exists():
        pytest.skip('build native-ddc first')
    src = FixtureSource(62_500_000.0, 16240, 120)
    result = run_native_pool(src.get, src.fs, src.n, 100e6, path, [0, 5, 31, 127], 120,
                             strategy=strategy)
    assert result['packets'] == result['pool']['submitted'] == result['pool']['completed']
    assert result['timestamps']['timestamp_gap_candidates'] == 0
    capacity = result['pool']['task_capacity']
    assert capacity == result['pool']['result_capacity'] == 6
    assert result['pool']['max_pending'] <= capacity
    assert result['pool']['max_ready'] <= capacity


@pytest.mark.parametrize('strategy', [0, 1])
def test_native_pool_aborts_on_sdk_error_without_leaking_handles(strategy):
    path = Path(__file__).resolve().parents[1] / 'native-ddc/target/release/libwebsa_channelizer.so'
    if not path.exists():
        pytest.skip('build native-ddc first')
    src = FixtureSource(62_500_000.0, 16240, 20)
    def failing():
        packet = src.get()
        return replace(packet, status=-10) if src.at == 5 else packet
    with pytest.raises(RuntimeError, match='IQS fetch failed'):
        run_native_pool(failing, src.fs, src.n, 100e6, path, [0, 5, 31, 127], 20,
                        strategy=strategy)
    # A new pool can start immediately after the aborted one's destructor joined its workers.
    another = FixtureSource(62_500_000.0, 16240, 12)
    assert run_native_pool(another.get, another.fs, another.n,
                           100e6, path, [0, 5, 31, 127], 12,
                           strategy=strategy)['packets'] == 12


def test_native_pool_strategies_match_on_same_iq():
    path = Path(__file__).resolve().parents[1] / 'native-ddc/target/release/libwebsa_channelizer.so'
    if not path.exists():
        pytest.skip('build native-ddc first')
    def run(strategy):
        src = FixtureSource(62_500_000.0, 16240, 240)
        return run_native_pool(src.get, src.fs, src.n, 100e6, path, list(range(32)), 240,
                               strategy=strategy)
    baseline, overlap = run(0), run(1)
    assert (baseline['packets'], baseline['pfb_groups'], baseline['checksum']) == (
        overlap['packets'], overlap['pfb_groups'], overlap['checksum'])
    assert baseline['timestamps'] == overlap['timestamps']
    assert overlap['pool']['task_capacity'] == overlap['pool']['result_capacity'] == 6


def test_fixture_matches_serial_and_closes_workers(api):
    src = FixtureSource(62_500_000.0, 16240, 24)
    report = run_pipeline(src.get, src.fs, src.n, 100e6, api, 24)
    assert report['packets'] == 24
    assert report['timestamps']['timestamp_gap_candidates'] == 0
    assert report['queue_task_max'] <= 2 and report['queue_result_max'] <= 2
    assert not any(t.name == 'pfb-only-worker' and t.is_alive() for t in threading.enumerate())


def test_sdk_error_aborts_and_does_not_leave_worker_threads(api):
    src = FixtureSource(62_500_000.0, 16240, 20)
    def failing():
        packet = src.get()
        return replace(packet, status=-10) if src.at == 5 else packet
    with pytest.raises(RuntimeError, match='IQS fetch failed'):
        run_pipeline(failing, src.fs, src.n, 100e6, api, 20)
    assert not any(t.name == 'pfb-only-worker' and t.is_alive() for t in threading.enumerate())
