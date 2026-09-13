"""Pure result-payload builders for the measurement sessions.

The sessions own the device interaction; assembling the payload that goes out over the
WebSocket is pure data shaping, so it lives here where it can be unit tested without the
vendor library (report finding P2-7).
"""
from __future__ import annotations


def pnm_payload(*, carrier_freq, carrier_power, offset, pn, ref, traceavg, done, progress) -> dict:
    """One phase-noise message from the values the DLL-facing step produced."""
    return {
        'cmd': 'PNM',
        'carrier_freq': float(carrier_freq),
        'carrier_power': float(carrier_power),
        'offset': [float(x) for x in offset],
        'pn': [float(x) for x in pn],
        'ref': float(ref),
        'traceavg': float(traceavg),
        'done': bool(done),
        'progress': int(progress),
    }
