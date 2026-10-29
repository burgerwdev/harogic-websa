"""The shared Pluto layer: a start-up that fails must put the crystal back.

A refused `--lo` once aborted the tool after the crystal trim, and the unit stayed
1.98 ppm off across the next open. These tests use a fake pyadi-iio, so they need no
radio.
"""
from __future__ import annotations

import argparse
import signal
import sys
import types
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from tools.pluto import pluto_radio as radio  # noqa: E402

TRIM_HZ = int(round(radio.XO_NOMINAL_HZ * (1.0 - radio.XO_PPM_LOW * 1e-6)))


@pytest.fixture(autouse=True)
def restore_signal_handlers():
    # Radio installs process-wide handlers even when its driver is a test double.
    previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        yield
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


class _Attr:
    """An IIO attribute that records every write, so the order is testable."""

    def __init__(self, value: str):
        self.writes: list[str] = []
        self._value = value

    @property
    def value(self) -> str:
        return self._value

    @value.setter
    def value(self, value: str) -> None:
        self.writes.append(value)
        self._value = value


class _Pluto:
    """A Pluto whose AD9363 refuses a low LO, as an unextended one does."""

    def __init__(self, uri: str, refuse_lo: bool = True):
        self.uri = uri
        self.refuse_lo = refuse_lo
        self.attrs = {'xo_correction': _Attr(str(radio.XO_NOMINAL_HZ))}
        self._ctrl = types.SimpleNamespace(
            attrs=self.attrs,
            ctx=types.SimpleNamespace(attrs={'ad9361-phy,model': 'ad9363a'}))
        self.sample_rate = 521000
        self.tx_hardwaregain_chan0 = 0.0
        self.lo = None

    @property
    def tx_lo(self) -> int | None:
        return self.lo

    @tx_lo.setter
    def tx_lo(self, value: int) -> None:
        if self.refuse_lo:
            raise OSError(22, 'Invalid argument')
        self.lo = value

    def tx_destroy_buffer(self) -> None:
        pass


class _Factory:
    """Stands in for `adi.Pluto`, keeping the instances for the assertions."""

    def __init__(self, refuse_lo: bool = True):
        self.refuse_lo = refuse_lo
        self.instances: list[_Pluto] = []

    def __call__(self, uri: str) -> _Pluto:
        pluto = _Pluto(uri, self.refuse_lo)
        self.instances.append(pluto)
        return pluto


def _fake_adi(monkeypatch: pytest.MonkeyPatch, refuse_lo: bool) -> _Factory:
    factory = _Factory(refuse_lo)
    module = types.ModuleType('adi')
    module.Pluto = factory  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, 'adi', module)
    return factory


def _args(**over) -> argparse.Namespace:
    base = dict(uri='ip:fake', lo=411e6, rate=521000.0, gain=-10.0,
                xo_ppm=radio.XO_PPM_LOW, no_xo_trim=False)
    base.update(over)
    return argparse.Namespace(**base)


def test_a_refused_lo_puts_the_crystal_trim_back(monkeypatch):
    factory = _fake_adi(monkeypatch, refuse_lo=True)

    with pytest.raises(SystemExit, match=r'99\.000000 MHz'), radio.Radio(_args(lo=99e6)):
        pass                                  # pragma: no cover - never entered

    writes = factory.instances[0].attrs['xo_correction'].writes
    assert writes == [str(TRIM_HZ), str(radio.XO_NOMINAL_HZ)]


def test_the_refused_lo_message_names_the_unit_and_the_range(monkeypatch):
    _fake_adi(monkeypatch, refuse_lo=True)

    with pytest.raises(SystemExit) as refused, radio.Radio(_args(lo=99e6)):
        pass                                  # pragma: no cover - never entered

    message = str(refused.value)
    assert '325-3800 MHz' in message and 'ad9363a' in message


def test_an_accepted_lo_keeps_the_trim_until_the_exit(monkeypatch):
    factory = _fake_adi(monkeypatch, refuse_lo=False)

    with radio.Radio(_args(lo=411e6)) as tx:
        assert tx.sdr is factory.instances[0]
        assert factory.instances[0].tx_lo == 411000000
        assert tx.rate == 521000
        assert factory.instances[0].attrs['xo_correction'].writes == [str(TRIM_HZ)]

    assert factory.instances[0].attrs['xo_correction'].writes == [str(TRIM_HZ),
                                                                 str(radio.XO_NOMINAL_HZ)]
