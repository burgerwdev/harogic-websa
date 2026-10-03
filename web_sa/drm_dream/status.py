"""Parsing for the Dream ``--status-socket`` JSON stream and UI-facing metadata.

Dream (console build) listens on a Unix domain socket and broadcasts one JSON
object per line at roughly 2 Hz. See ``tools/drm_dream/SPIKE.md`` for the wire
format and how the fields were verified against the fixture ground truth.

The status integers are ``ETypeRxStatus`` mapped by Dream itself:
``0 = RX_OK, 1 = CRC_ERROR, 2 = DATA_ERROR, -1 = NOT_PRESENT``.
"""
from __future__ import annotations

import json

#: ``Parameters.GetWaveMode()`` -> robustness letter.
ROBUSTNESS_MODES = {0: 'A', 1: 'B', 2: 'C', 3: 'D'}

#: ``CAudioParam::EAudCod`` -> codec name.
AUDIO_CODECS = {0: 'AAC', 1: 'Opus', 2: 'Reserved', 3: 'xHE-AAC', 4: 'None'}

#: The status code Dream reports when a channel decoded cleanly.
RX_OK = 0


def parse_status_line(line: str) -> dict | None:
    """Parse one newline-delimited status object; ``None`` for blank/garbage lines."""
    line = line.strip()
    if not line:
        return None
    try:
        value = json.loads(line)
    except (json.JSONDecodeError, ValueError):
        return None
    return value if isinstance(value, dict) else None


def first_audio_service(status: dict) -> dict | None:
    for svc in status.get('service_list') or []:
        if isinstance(svc, dict) and svc.get('is_audio'):
            return svc
    return None


def extract_metadata(status: dict | None) -> dict:
    """Flatten a raw status object into the fields the UI/API publish."""
    if not status:
        return {}
    st = status.get('status') or {}
    mode = status.get('mode') or {}
    signal = status.get('signal') or {}
    svc = first_audio_service(status) or {}
    language = svc.get('language') or {}
    country = svc.get('country') or {}
    return {
        'station': svc.get('label') or '',
        'service_id': svc.get('id'),
        'robustness': ROBUSTNESS_MODES.get(mode.get('robustness')),
        'bandwidth_khz': mode.get('bandwidth_khz'),
        'bitrate_kbps': svc.get('bitrate_kbps'),
        'audio_codec': AUDIO_CODECS.get(svc.get('audio_coding')),
        'audio_mode': svc.get('audio_mode'),
        'protection': svc.get('protection_mode'),
        'language': language.get('code'),
        'country': country.get('code'),
        'text': svc.get('text'),
        'snr_db': signal.get('snr_db'),
        'mer_db': signal.get('mer_db'),
        'wmer_db': signal.get('wmer_db'),
        'sync': (st.get('io') == RX_OK and st.get('time') == RX_OK
                 and st.get('frame') == RX_OK),
        'status': st,
    }
