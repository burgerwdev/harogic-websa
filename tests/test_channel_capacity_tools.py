"""Bench scenarios must distinguish their selected channels from service capacity."""
import copy

import pytest

from tools.bench.sdr_pipeline_controls import Probe


@pytest.mark.parametrize('capacity', [3, 8, 16])
def test_mixed_probe_accepts_extra_disabled_slots_and_rejects_unexpected_receivers(capacity):
    expected = {1: ('wfm', 180000), 2: ('am', 6000), 3: ('cw', 500)}
    rows = [dict(channel=slot, frequency_hz=100e6, channel_id=slot, epoch=1,
                 output_rate=rate, input_rate=62500000, group_delay_samples=0, state='active') for slot, rate in [(1, 192000), (2, 48000), (3, 8000)]]
    channels = [dict(channel=slot, enabled=True, mode=mode, if_bw=bw, freq=100e6,
                     channel_id=slot, epoch=1, actual_iq_rate=rows[slot - 1]['output_rate'])
                for slot, (mode, bw) in expected.items()]
    channels.extend(dict(channel=slot, enabled=False) for slot in range(4, capacity + 1))
    state = {'channel_registry': rows, 'actual': {'channel_registry': rows},
             'sdr': {'channels': channels, 'actual': {'channel_registry': rows}}}
    assert Probe.mixed_channels(state, expected)
    missing = copy.deepcopy(state)
    missing['sdr']['channels'].pop(1)
    assert not Probe.mixed_channels(missing, expected)
    if capacity > len(expected):
        state['sdr']['channels'][-1]['enabled'] = True
        assert not Probe.mixed_channels(state, expected)


def test_continuity_gate_accepts_startup_progress_but_rejects_restarts_and_failures(monkeypatch):
    import importlib
    from pathlib import Path

    monkeypatch.syspath_prepend(str(Path(__file__).resolve().parents[1] / 'tools/e2e'))
    check = importlib.import_module('native_multichannel_processing').assert_receivers_continue
    old = {'receiver_uid': 'uid', 'confirmed': {'enabled': True}, 'revision': '1',
           'stream': {'epoch': '1', 'channel_id': '1'}, 'clock': None,
           'processing_state': 'waiting', 'processing_runtime': {'processing_generation': '3'},
           'processing_fault': None, 'processing_statistics': []}
    current = copy.deepcopy(old)
    current['processing_state'] = 'active'
    check([old], [current])
    backlogged = copy.deepcopy(current)
    backlogged['processing_state'] = 'backlogged'
    check([current], [backlogged])
    for field, value in [('processing_state', 'failed'), ('processing_state', 'retiring'),
                         ('processing_fault', {'error': 'overflow'}),
                         ('processing_runtime', {'processing_generation': '4'}),
                         ('confirmed', {'enabled': False})]:
        broken = copy.deepcopy(current)
        broken[field] = value
        with pytest.raises(AssertionError):
            check([current], [broken])
