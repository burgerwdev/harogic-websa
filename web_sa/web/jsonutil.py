"""JSON helpers for the WebSocket/REST boundary.

``finite_json`` replaces NaN/Inf with ``null``: one non-finite measurement value must not
drop a whole status message (``json.dumps`` would raise, and the client would lose the
update). It used to exist twice (client_stream and http_api); the publisher serializes a
message once for every client instead of once per client (report finding P1-11).
"""
from __future__ import annotations

import json
import math


def finite_json(value):
    """Recursively replace non-finite floats with None so the payload is JSON-safe."""
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, dict):
        return {key: finite_json(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [finite_json(item) for item in value]
    return value


def dumps_json(obj: dict) -> str:
    return json.dumps(finite_json(obj), allow_nan=False, separators=(',', ':'))


def is_periodic_status(obj: dict) -> bool:
    """True for the 1 Hz STATUS push (coalesced); a command reply must never be dropped."""
    return obj.get('cmd') == 'STATUS' and not obj.get('response_to')
