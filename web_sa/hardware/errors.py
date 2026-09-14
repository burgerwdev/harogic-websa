"""Hardware errors that do not depend on the vendor library.

`DeviceError` lives here (and is re-exported by `hardware.device`) so that modules on the
scheduler path - e.g. `web/publisher.py` - can import it without loading libhtraapi. That is
what lets the fake device (`hardware/fake_device.py`) run the whole application with no vendor
library at all, which is how the UI regression runs in CI (report finding A1).
"""
from __future__ import annotations


class DeviceError(RuntimeError):
    """A fatal hardware error: the worker must exit so the supervisor can reopen the device."""
