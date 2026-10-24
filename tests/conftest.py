"""Pytest guards for the independent SDK and reference-tool tests.

Only modules that directly require the proprietary SDK are skipped when its
shared library is unavailable. Numerical DSP and other tool tests collect
without loading the SDK.
"""
from __future__ import annotations

from functools import cache

import pytest

NEEDS_VENDOR = {
    'test_capability_query.py',
    'test_native_ddc.py',
    'test_sdr_capture.py',
}


@cache
def _vendor_status() -> tuple[bool, str]:
    try:
        from tools.reference import sdk_bindings  # noqa: F401
    except OSError as exc:
        return False, str(exc)
    return True, ''


def pytest_ignore_collect(collection_path):
    if collection_path.name in NEEDS_VENDOR:
        available, _error = _vendor_status()
        return not available
    return None


@pytest.fixture(scope='session')
def vendor_required():
    """Skip an SDK-dependent case without hiding independent cases in its module."""
    available, error = _vendor_status()
    if not available:
        pytest.skip(f'vendor SDK unavailable: {error}')
