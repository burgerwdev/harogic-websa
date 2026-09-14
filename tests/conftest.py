"""Pytest configuration: run the hardware-free subset when the vendor SDK is absent.

`web_sa.hardware.sdk_bindings` loads `/opt/htraapi/lib/x86_64/libhtraapi.so` at import
time (overridable with `HTRA_API_LIB`). CI and contributor machines do not have the vendor
library, so the modules that import it are skipped there instead of erroring at collection.
On the bench (`make hw-test`) every module runs.

The frame codec, HTTP/WS layer and configuration tests stay hardware-free on purpose: see
docs/*/ARCH_REVIEW.md findings P0-3/P0-4 and gap G-3.
"""
from __future__ import annotations

import pytest

# Test modules that import the vendor binding layer directly or transitively (a device
# session, a profile struct, a DLL symbol).
NEEDS_VENDOR = (
    'test_device_state.py',
    'test_device_state_grouping.py',
    'test_http_api.py',
    'test_publisher.py',
    'test_rta_state.py',
    'test_sdk_bindings.py',
    'test_sdr.py',
    'test_ws_commands.py',
)


def _vendor_status() -> tuple[bool, str]:
    try:
        from web_sa.hardware import sdk_bindings  # noqa: F401
    except OSError as exc:          # library missing / wrong architecture
        return False, str(exc)
    return True, ''


VENDOR_OK, VENDOR_ERROR = _vendor_status()

if not VENDOR_OK:
    collect_ignore = list(NEEDS_VENDOR)


def pytest_report_header(config) -> str:
    if VENDOR_OK:
        return 'vendor SDK: available (all tests)'
    return (f'vendor SDK: unavailable ({VENDOR_ERROR}) - skipping '
            f'{len(NEEDS_VENDOR)} hardware test modules')


@pytest.fixture(scope='session')
def vendor_required():
    """Explicit guard for tests that would silently pass without the hardware."""
    if not VENDOR_OK:
        pytest.skip(f'vendor SDK unavailable: {VENDOR_ERROR}')
