"""Browser diagnostics must restore SWP too, even when started in SDR."""
from tools.bench import sdr_network_diagnostics as diag


def test_starvation_summary_separates_startup_from_steady_state():
    rows = [{'dsp_starved': str(n)} for n in (12, 20, 20, 25)]
    summary = diag.starvation_summary({'dsp_starved': '10'}, rows, [0.2, 0.6, 1.1, 1.8], 2.0)
    assert summary == {'last_half_starved_delta': 5,
                       'starve_events': [(0.2, 2), (0.6, 8), (1.8, 5)]}


def test_restore_replays_swp_and_original_mode(monkeypatch):
    calls = []
    monkeypatch.setattr(diag, 'command', lambda url, data: calls.append(data))
    monkeypatch.setattr(diag, 'state', lambda url: {'mode': 'std'})
    initial = {
        'mode': 'sdr',
        'sdr': {'center': 100e6, 'decimate': 4, 'listen': 100.1e6,
                'demod': 'wfm', 'if_bw': 180e3, 'squelch': -100,
                'agc': True, 'volume': 0.8},
        'req': {'swp': {
            'center': 4510004000.0, 'span': 9019992000.0, 'points': 4000,
            'rbw_mode': 'auto', 'rbw': 250000, 'vbw_mode': 'bypass', 'vbw': 300000,
            'ref_mode': 'manual', 'ref': 0, 'sweep_time_mode': 0, 'sweep_time': 0,
            'spur': 'standard', 'window': 1, 'detector': 'auto',
            'frame_detector': 'pos_peak', 'trace_align': 'native',
            'points_strategy': 'speed',
        }},
    }
    diag.restore('http://127.0.0.1:8080', initial)
    assert calls[0] == {'cmd': 'SET_MODE', 'mode': 'sdr'}
    assert calls[4] == {'cmd': 'SET_MODE', 'mode': 'std'}
    assert calls[5] == {'cmd': 'SET_FREQ', 'center': 4510004000.0, 'span': 9019992000.0}
    assert calls[-1] == {'cmd': 'SET_MODE', 'mode': 'sdr'}
    assert {c['cmd'] for c in calls} >= {
        'SET_RBW', 'SET_VBW', 'SET_REF', 'SET_SWEEP', 'SET_POINTS', 'SET_FRAMEDET',
        'SET_TRACEGRID', 'SET_DETECTOR', 'SET_SPUR', 'SET_WINDOW',
    }
