#!/usr/bin/env python3
"""Shared harness for tools/vsa_probe/ (SAN-90 exclusive device access).

Repo rule: ``libhtraapi`` is not thread-safe and only one process may hold the
USB handle. Every hardware probe must run with the WebSA service stopped, so
wrap hardware work in :func:`exclusive_websa`: it stops the supervisor + worker,
verifies the device is free, and restores the service on exit (also on error).

Usage from a probe script (each probe documents its own command line in its
module docstring)::

    from harness import DeviceSession, exclusive_websa, Report
    with exclusive_websa():
        with DeviceSession() as dev:
            st = dev.dll.IQS_Configuration(...)

Offline (synthetic-loopback) probes must not import this module.
"""
from __future__ import annotations

import ctypes
import json
import os
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PROBE_DIR = os.path.dirname(os.path.abspath(__file__))
SDR_PROBE_DIR = os.path.join(REPO, 'tools', 'sdr_probe')
sys.path.insert(0, REPO)
sys.path.insert(0, SDR_PROBE_DIR)

from tinysa import TinySA  # noqa: E402  (reused known-signal source driver)

TINYSA_PORT = os.environ.get('TINYSA_PORT', '/dev/ttyACM0')


def websa_pids() -> list[str]:
    out = subprocess.run(['pgrep', '-f', r'python3 -m web_sa\.(supervisor|main)'],
                         capture_output=True, text=True)
    return [p for p in out.stdout.split() if p]


def websa_running() -> bool:
    return bool(websa_pids())


def device_probe_peers() -> list[str]:
    """Other running python VSA probe processes (they may hold the analyzer).

    A concurrent probe makes ``Device_Open`` fail with status -1, which looks
    like a broken analyzer. Check for it explicitly instead of guessing.
    """
    out = subprocess.run(['pgrep', '-af', r'vsa_probe/probe_'],
                         capture_output=True, text=True)
    me = os.getpid()
    peers = []
    for line in out.stdout.splitlines():
        pid, _, cmd = line.partition(' ')
        if not pid.isdigit() or int(pid) == me:
            continue
        if cmd.split(' ')[0].endswith('python3') or 'python' in cmd.split(' ')[0]:
            peers.append(line)
    return peers


class exclusive_websa:
    """Context manager: stop WebSA, yield, then restore it.

    ``restore=False`` keeps the service stopped (use when a probe chain runs on
    its own and the caller restarts later); the probe still refuses to run while
    the service holds the device.
    """

    def __init__(self, restore: bool = True, timeout: float = 30.0):
        self.restore = restore
        self.timeout = timeout
        self.was_running = False

    def __enter__(self):
        self.was_running = websa_running()
        if self.was_running:
            print('[harness] stopping WebSA (./stop.sh)', flush=True)
            subprocess.run(['./stop.sh'], cwd=REPO, check=True,
                           capture_output=True, text=True)
        end = time.time() + self.timeout
        while websa_running() and time.time() < end:
            time.sleep(0.2)
        if websa_running():
            raise RuntimeError('WebSA still holds the device; refusing to probe')
        peers = device_probe_peers()
        if peers:
            raise RuntimeError('another VSA probe holds the device; refusing to start: '
                               + '; '.join(peers))
        print('[harness] device is exclusively ours', flush=True)
        return self

    def __exit__(self, *exc):
        if self.restore and self.was_running:
            print('[harness] restoring WebSA (./run.sh)', flush=True)
            subprocess.run(['./run.sh'], cwd=REPO, check=True,
                           capture_output=True, text=True)
        return False


class DeviceSession:
    """Opens the analyzer (and a DSP handle) and always closes it again."""

    def __init__(self, open_dsp: bool = True):
        self.open_dsp = open_dsp
        self.dev = None
        self.dsp = None
        self.boot = None

    def __enter__(self):
        import htra_api as T

        self.T = T
        bp = T.BootProfile_TypeDef()
        bp.DevicePowerSupply = T.DevicePowerSupply_TypeDef.USBPortAndPowerPort
        bp.PhysicalInterface = T.PhysicalInterface_TypeDef.USB
        last = None
        # 0 and -49 are both documented as a successful open. A stale handle from a
        # process that just exited can make the first attempt fail, so retry briefly.
        for _attempt in range(4):
            self.dev = ctypes.c_void_p()
            self.boot = T.BootInfo_TypeDef()
            last = T.dll.Device_Open(ctypes.byref(self.dev), ctypes.c_int(0),
                                     ctypes.byref(bp), ctypes.byref(self.boot))
            if last in (0, -49):
                break
            peers = device_probe_peers()
            if peers:
                raise RuntimeError('Device_Open failed and another VSA probe is running: '
                                   + '; '.join(peers))
            time.sleep(1.5)
        if last not in (0, -49):
            raise RuntimeError(f'Device_Open status={last} after 4 attempts '
                               '(analyzer present? on the USB bus?)')
        if self.open_dsp:
            self.dsp = ctypes.c_void_p()
            T.dll.DSP_Open(ctypes.byref(self.dsp))
        return self

    def __exit__(self, *exc):
        T = self.T
        try:
            if self.dsp is not None:
                T.dll.DSP_Close(ctypes.byref(self.dsp))
        finally:
            if self.dev is not None:
                T.dll.Device_Close(ctypes.byref(self.dev))
        print('[harness] device closed', flush=True)
        return False

    # ---- convenience ----
    def info(self) -> dict:
        """Device identity as the SDK reports it (raw fields, no interpretation)."""
        di = self.boot.DeviceInfo
        return dict(uid='%012X' % di.DeviceUID, model=int(di.Model),
                    hw=int(di.HardwareVersion), mfw=int(di.MFWVersion),
                    ffw=int(di.FFWVersion),
                    bus_mbps=int(self.boot.BusSpeed),
                    bus_version=int(self.boot.BusVersion),
                    api_version_raw=int(self.boot.APIVersion))

    def hardware_state(self) -> dict:
        """HardWareState_TypeDef is not exported by htra_api.py -> declare it here."""
        T = self.T

        class HardWareState(ctypes.Structure):
            _fields_ = [('GNSSPeriphType', ctypes.c_int), ('GNSSType', ctypes.c_int),
                        ('OCXOType', ctypes.c_int), ('InternalOCXO', ctypes.c_uint8),
                        ('SignalSourceEn', ctypes.c_uint8),
                        ('ADC_VariableRateEn', ctypes.c_uint8),
                        ('IM3_filter', ctypes.c_uint8)]

        T.dll.Device_GetHardwareState.argtypes = [T.POINTER(ctypes.c_void_p),
                                                  T.POINTER(HardWareState)]
        T.dll.Device_GetHardwareState.restype = ctypes.c_int
        hs = HardWareState()
        st = T.dll.Device_GetHardwareState(ctypes.byref(self.dev), ctypes.byref(hs))
        out = {'status': int(st)}
        for name, _ in hs._fields_:
            out[name] = int(getattr(hs, name))
        return out


def tiny_sa(port: str | None = None):
    """Open the tinySA Ultra known signal source (CW/AM/FM only)."""
    return TinySA(port or TINYSA_PORT)


class Report:
    """Collects results, prints them immediately, and can dump JSON."""

    def __init__(self, title: str):
        self.title = title
        self.data: dict = {'title': title, 'started': time.strftime('%Y-%m-%d %H:%M:%S')}
        print(f'### {title}', flush=True)

    def section(self, name: str):
        print(f'\n== {name} ==', flush=True)
        return self.data.setdefault(name, {})

    def kv(self, key, value, unit: str = ''):
        shown = f'{value:g}' if isinstance(value, float) else str(value)
        print(f'  {key:<34s} {shown}{(" " + unit) if unit else ""}', flush=True)
        self.data.setdefault('values', {})[key] = value
        return value

    def table(self, headers: list[str], rows: list[list], where: dict | None = None):
        widths = [max(len(str(h)), *(len(f'{r[i]:g}') if isinstance(r[i], float)
                                       else len(str(r[i])) for r in rows))
                  for i, h in enumerate(headers)] if rows else [len(h) for h in headers]
        print('  ' + '  '.join(str(h).ljust(w) for h, w in zip(headers, widths, strict=True)), flush=True)
        for r in rows:
            cells = [f'{c:g}' if isinstance(c, float) else str(c) for c in r]
            print('  ' + '  '.join(c.ljust(w) for c, w in zip(cells, widths, strict=True)), flush=True)
        if where is not None:
            where['rows'] = rows
            where['headers'] = headers
        return rows

    def save(self, path: str):
        self.data['finished'] = time.strftime('%Y-%m-%d %H:%M:%S')
        with open(path, 'w') as fh:
            json.dump(self.data, fh, indent=2, default=str)
        print(f'\n[report] wrote {path}', flush=True)
