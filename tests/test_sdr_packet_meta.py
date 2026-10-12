"""Per-packet IF-overflow surfacing on the SDR/IQS path (L1, findings/level-atten.md).

The device sets DeviceState.IFOverflow inside every IQStream packet (htra_api.h:1118);
the SWP path reports the same condition as the -12 warning, which the auto-reference
protective raise and the client warning stack already key on. The SDR loop now maps the
bit to that same status instead of ignoring it.
"""
from __future__ import annotations

from web_sa.hardware import sdk_bindings as sb
from web_sa.measurements.sdr import _packet_overflows


def _stream_with_overflow(value: int):
    stream = sb.IQStream_TypeDef()
    stream.DeviceState.IFOverflow = value
    return stream


def test_overflow_bit_sets_the_status_the_escape_chain_keys_on():
    assert _packet_overflows(_stream_with_overflow(1)) is True
    assert _packet_overflows(_stream_with_overflow(0xFFFF)) is True


def test_clean_packet_reports_no_overflow():
    assert _packet_overflows(_stream_with_overflow(0)) is False
    assert _packet_overflows(sb.IQStream_TypeDef()) is False   # zero-initialized


def test_odd_payloads_degrade_to_no_overflow():
    class Weird:
        DeviceState = None

    assert _packet_overflows(Weird()) is False
    class StrState:
        IFOverflow = 'noise'
    w2 = Weird()
    w2.DeviceState = StrState()
    assert _packet_overflows(w2) is False


def test_peak_hold_rises_with_the_packet_and_decays_over_time():
    from web_sa.measurements.sdr import _peak_hold
    assert _peak_hold(-200.0, -30.0, 0.0) == -30.0          # packet lifts the hold
    assert _peak_hold(-30.0, -50.0, 0.0) == -30.0           # quieter packet holds
    assert _peak_hold(-30.0, -50.0, 1.0) == -50.0           # 1 s at 20 dB/s decays past it
    assert _peak_hold(-30.0, -50.0, 0.5) == -40.0           # half second decays 10 dB
    assert _peak_hold(-200.0, -999.0, 0.0) == -200.0        # nonsense packet is ignored
    assert _peak_hold(-20.0, -999.0, 0.0) == -20.0          # floor never rises the hold
