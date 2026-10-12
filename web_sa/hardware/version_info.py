"""Device model / firmware version display decoding.

Rules reverse-engineered from the official SAStudio4 "About" panel and backed by the
vendor's own Python example (SA_Linux_Python_Examples/Device_GetDeviceInfo.py in the
0.55.100 SDK):

* Model -> DeviceClass enum name (htra_api.h:977-1010): ``Device_N90_R0 = 67`` is
  displayed as ``N90 R0`` -- strip the ``Device_`` prefix and render the revision
  suffix after a space.
* MFW/FFW/Bus/API versions are u32 packed as ``(hi << 16) | (mid << 8) | lo`` and
  printed ``hi.mid.lo`` (the vendor example prints MCU/FPGA exactly this way).
* PMU/AGU versions are u16 raw values displayed as ``0.<hi>.<lo>(<raw>)`` -- the real
  unit shows ``0.1.15(271)`` / ``0.1.5(261)`` and 271=0x010F, 261=0x0105 decode exactly.

Nothing here talks to the device; the decoding is pure so it can be unit-tested
against the screenshot values without hardware.
"""
from __future__ import annotations

# DeviceClass_TypeDef (htra_api.h:977-1010). Kept verbatim from the SDK header,
# including the W200 oddity (R3=140 before R2=141).
DEVICE_CLASS_NAMES: dict[int, str] = {
    10: 'E90 R0', 11: 'E90 R1', 12: 'E90 R2', 13: 'E90 R3', 14: 'E90 R4', 15: 'E90 R5',
    20: 'E200 R0', 21: 'E200 R1', 22: 'E200 R2', 23: 'E200 R3',
    33: 'TRX60 R3', 34: 'TRX60 R4',
    40: 'ZRX200 R0',
    53: 'N60 R4', 54: 'M60 R4', 55: 'N45 R4', 56: 'M80 R5', 57: 'M60 R5',
    58: 'N60 R5', 59: 'N45 R5',
    60: 'E310 R0', 61: 'E310 R1',
    65: 'N45 R6', 66: 'N60 R6',
    67: 'N90 R0', 68: 'PN90 R0NDS', 69: 'N91 R0',
    70: 'E330 R0', 80: 'TX90 R0',
    90: 'N400 R0', 91: 'N400 R1', 92: 'N400 R2', 93: 'N150 R1',
    94: 'N400 R2V2', 95: 'N400 R3', 96: 'N400 R4',
    100: 'SIMULATOR',
    110: 'RX90 R0',
    120: 'A24 R0', 121: 'A24 R1',
    130: 'C24 R0', 131: 'C24 R1',
    141: 'W200 R2', 140: 'W200 R3', 144: 'W200 R4',
}


def model_name(model: int) -> str:
    """Display name for a DeviceInfo.Model value ("N90 R0" for 67, the bench unit)."""
    try:
        model = int(model)
    except (TypeError, ValueError):
        return 'Model ?'
    return DEVICE_CLASS_NAMES.get(model, 'Model %d' % model)


def fmt_fw3(version: int) -> str:
    """u32 packed firmware (MFW/FFW/Bus/API) -> "hi.mid.lo" (vendor example rule)."""
    try:
        version = int(version)
    except (TypeError, ValueError):
        return '?'
    if version < 0 or version > 0xFFFFFFFF:
        return '?'
    return '%d.%d.%d' % ((version >> 16) & 0xFFFF, (version >> 8) & 0xFF, version & 0xFF)


def fmt_pmu_agu(version: int) -> str:
    """u16 PMU/AGU firmware -> "0.<hi>.<lo>(<raw>)" as the official About panel shows."""
    try:
        version = int(version)
    except (TypeError, ValueError):
        return '?'
    if version < 0 or version > 0xFFFF:
        return '?'
    return '0.%d.%d(%d)' % ((version >> 8) & 0xFF, version & 0xFF, version)
