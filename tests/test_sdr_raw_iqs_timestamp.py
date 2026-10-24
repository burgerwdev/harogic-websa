from tools.bench.sdr_raw_iqs_check import timestamp_summary


def test_adaptive_iqs_timestamp_detects_large_steps_not_sdk_status():
    points = 16_240
    clock = [1000 + i * 51_968 for i in (0, 1, 2, 3, 4)]
    ok = timestamp_summary([(t, points) for t in clock], 31_250_000)
    assert ok['timestamp_usable']
    assert ok['estimated_timer_hz'] == 100_000_000
    assert ok['device_window_sps'] == 31_250_000
    assert abs(ok['device_duration_s'] - 5 * points / 31_250_000) < 1e-6
    assert ok['timestamp_gap_candidates'] == 0
    gap = timestamp_summary([(clock[i], points) for i in (0, 1, 2, 4)], 31_250_000)
    assert gap['timestamp_gap_candidates'] == 1
    assert gap['step_ratio_max'] == 2


def test_unavailable_hardware_counter_is_not_invented():
    assert timestamp_summary([(0, 16240)] * 4, 31_250_000) == {
        'timestamp_usable': False, 'timestamp_packets': 4}
