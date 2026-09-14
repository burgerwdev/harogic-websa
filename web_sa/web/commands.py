"""Command table: one declarative entry per WS/REST command.

Why this module exists (report findings P1-1/P1-2, phase 2):

* `ws.py` used to hold the command set, a 144-line `if/elif` validator and a 311-line
  `if/elif` dispatcher, so adding a command meant editing three places and the mode policy
  was spread across the dispatch chain.
* Here every command is one `CommandSpec` that declares its validation, its handler and
  the modes/sessions it is allowed in. `validate()` and `dispatch()` below are the only
  two entry points; `ws.py` and `http_api.py` only adapt transport to them.

The handlers are the previous `_dispatch` branches, moved verbatim; the guards
(SWP-owned commands during Harmonic/PNM, SDR-only restrictions, RTA-only restrictions)
became spec flags so the policy is visible per command instead of buried in control flow.
"""
from __future__ import annotations

import asyncio
import logging
import math
from collections.abc import Awaitable, Callable
from dataclasses import dataclass

from ..config import (
    DISPLAY_REF_MAX_DBM,
    DISPLAY_REF_MIN_DBM,
    HARM_COUNT_MAX,
    HARM_SPAN_MAX_HZ,
    PNM_CARRIER_MAX_HZ,
    PNM_CARRIER_MIN_HZ,
    PNM_OFFSET_MAX_HZ,
    REF_RANGE_DB_MAX,
    REF_RANGE_DB_MIN,
    REFCLK_CAL_COUNT_MAX,
    REFCLK_CAL_COUNT_MIN,
    SDR_DEEMPH_MAX_US,
    SDR_DEEMPH_MIN_US,
    SDR_IFBW_MAX_HZ,
    SDR_IFBW_MIN_HZ,
    SDR_PITCH_MAX_HZ,
    SDR_PITCH_MIN_HZ,
    SDR_VOLUME_MAX,
    SDR_VOLUME_MIN,
    SQUELCH_MAX_DBFS,
    SQUELCH_MIN_DBFS,
    TRIGGER_ACQ_MAX_S,
    TRIGGER_ACQ_MIN_S,
    TRIGGER_LEVEL_MAX_DBM,
    TRIGGER_LEVEL_MIN_DBM,
    TRIGGER_RETRIGGER_MAX,
    TRIGGER_RETRIGGER_PERIOD_MAX_S,
    TRIGGER_TIME_MAX_S,
)
from .recovery import fatal

log = logging.getLogger(__name__)

#: How long one hardware call may take before the worker is restarted (the DLL is allowed
#: to be slow, not to hang).
HW_CALL_TIMEOUT_S = 20.0

#: Commands the swept engine owns. While a Harmonic/PNM measurement runs it owns the
#: device configuration, so re-applying these behind its back would break its acquisition.
SWP_OWNED = frozenset({
    'SET_REF', 'AUTO_SCALE', 'SET_FREQ', 'SET_RBW', 'SET_VBW', 'SET_SWEEP', 'SET_POINTS',
    'SET_SPUR', 'SET_WINDOW', 'SET_DETECTOR', 'SET_AMP', 'SET_REFCK', 'SET_REFCKOUT',
})
#: SWP-only parameters that make no sense inside an RTA profile.
SWP_ONLY = frozenset({'SET_WINDOW', 'SET_POINTS', 'SET_SPUR'})
#: In SDR mode the demod chain owns the configuration; these would fight with it.
NOT_IN_SDR = frozenset({
    'SET_FREQ', 'SET_RBW', 'SET_VBW', 'SET_SWEEP', 'SET_POINTS', 'SET_SPUR',
    'SET_WINDOW', 'SET_DETECTOR', 'SET_TRIGGER', 'SET_RTA',
})


class CommandError(ValueError):
    """A client command is malformed or cannot be applied in the current state.

    `code` is a stable identifier the frontend maps to a localized message and `params`
    carries the values interpolated into it. str(exc) stays English so API clients and
    logs keep a readable diagnostic.
    """

    code = 'command_failed'
    params: dict = {}

    def __init__(self, message: str, code: str = '', **params):
        super().__init__(message)
        if code:
            self.code = code
        self.params = params


def error_payload(exc: CommandError) -> dict:
    """Serialize a command error for WS/REST transport."""
    payload = {'msg': str(exc), 'code': getattr(exc, 'code', 'command_failed')}
    params = getattr(exc, 'params', None)
    if params:
        payload['params'] = params
    return payload


# ---------------------------------------------------------------------------
# Validation primitives (shared by the per-command validators; they coerce the value in
# place so the handler sees a number, not the client's string).
# ---------------------------------------------------------------------------


def _number(data, key, *, minimum=None, maximum=None, required=False):
    if key not in data:
        if required:
            raise CommandError(f'missing {key}', 'missing_param', key=key)
        return None
    value = data[key]
    if isinstance(value, bool):
        raise CommandError(f'{key} must be a number', 'not_a_number', key=key)
    try:
        value = float(value)
    except (TypeError, ValueError):
        raise CommandError(f'{key} must be a number', 'not_a_number', key=key) from None
    if not math.isfinite(value):
        raise CommandError(f'{key} must be finite', 'not_finite', key=key)
    if minimum is not None and value < minimum:
        raise CommandError(f'{key} must be >= {minimum}', 'below_min', key=key, min=minimum)
    if maximum is not None and value > maximum:
        raise CommandError(f'{key} must be <= {maximum}', 'above_max', key=key, max=maximum)
    data[key] = value
    return value


def _integer(data, key, *, minimum=None, maximum=None, required=False):
    value = _number(data, key, minimum=minimum, maximum=maximum, required=required)
    if value is None:
        return None
    if not value.is_integer():
        raise CommandError(f'{key} must be an integer', 'not_an_integer', key=key)
    data[key] = int(value)
    return data[key]


def _choice(data, key, choices, *, required=False):
    if key not in data:
        if required:
            raise CommandError(f'missing {key}', 'missing_param', key=key)
        return None
    value = data[key]
    if value not in choices:
        raise CommandError(
            f'{key} must be one of {", ".join(map(str, choices))}', 'invalid_choice',
            key=key, choices=', '.join(map(str, choices)))
    return value


def _caps(dev):
    caps = dev.state.caps
    if caps is None:
        raise CommandError('device capabilities are unavailable', 'caps_unavailable')
    return caps


# ---------------------------------------------------------------------------
# Parameter schema (report finding E-1)
#
# One ParamSpec per wire field is the single description of a parameter: the command layer
# validates from it, `build_schema()` publishes it on /api/schema for the frontend, and a
# new parameter is one row (plus an optional cross-field rule) instead of edits in three
# places. Bounds may be a number or a callable taking DeviceCapabilities, so model limits
# stay in the capability table.
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class ParamSpec:
    """One wire parameter: type, bounds, choices and when it is required."""

    name: str
    kind: str                                   # 'number' | 'integer' | 'choice' | 'boolean'
    minimum: object = None                      # number or Callable[[caps], number]
    maximum: object = None
    choices: tuple = ()
    unit: str = ''
    default: object = None
    required: bool = False
    #: Extra condition for requiredness, e.g. only when mode == 'manual'.
    required_if: Callable | None = None

    def bound(self, caps, which: str):
        """Resolve a bound; a capability-backed bound is None without a device."""
        value = self.minimum if which == 'min' else self.maximum
        if not callable(value):
            return value
        return value(caps) if caps is not None else None

    def is_required(self, data: dict) -> bool:
        if self.required:
            return True
        return bool(self.required_if and self.required_if(data))

    def to_json(self, caps) -> dict:
        return {
            'name': self.name,
            'type': self.kind,
            'min': self.bound(caps, 'min'),
            'max': self.bound(caps, 'max'),
            'choices': list(self.choices),
            'unit': self.unit,
            'default': self.default,
            'required': self.required,
        }


def _yes(key, value):
    return lambda data: data.get(key) == value


def _has_any(*keys):
    return lambda data: any(k in data for k in keys)


RBW_MAX = lambda caps: caps.rbw_max_hz        # noqa: E731 - capability-backed bound
VBW_MAX = lambda caps: caps.vbw_max_hz        # noqa: E731
POINTS_MAX = lambda caps: caps.points_max     # noqa: E731
ATTEN_MAX = lambda caps: caps.atten_max       # noqa: E731
IFGAIN_MAX = lambda caps: caps.ifgain_max     # noqa: E731
DECIMATE_MAX = lambda caps: caps.decimate_max  # noqa: E731
RTA_SPAN_MAX = lambda caps: caps.rta_span_max_hz  # noqa: E731
REF_MIN = lambda caps: caps.ref_min_dbm       # noqa: E731
REF_MAX = lambda caps: caps.ref_max_dbm       # noqa: E731
TRIG_MIN = lambda caps: caps.trigger_level_min_dbm  # noqa: E731
TRIG_MAX = lambda caps: caps.trigger_level_max_dbm  # noqa: E731

#: command -> wire parameters. Cross-field rules live in EXTRA_VALIDATORS below.
PARAMS: dict[str, tuple[ParamSpec, ...]] = {
    'CAL_REFCLK': (
        ParamSpec('count', 'integer', REFCLK_CAL_COUNT_MIN, REFCLK_CAL_COUNT_MAX,
                  default=10, unit='samples'),
    ),
    'SET_FREQ': (
        ParamSpec('center', 'number', unit='Hz'),
        ParamSpec('span', 'number', 100.0, unit='Hz'),
        ParamSpec('start', 'number', unit='Hz'),
        ParamSpec('stop', 'number', unit='Hz'),
    ),
    'SET_REF': (
        ParamSpec('mode', 'choice', choices=('manual', 'auto'), default='manual'),
        ParamSpec('range_db', 'number', REF_RANGE_DB_MIN, REF_RANGE_DB_MAX, unit='dB'),
        ParamSpec('ref', 'number', REF_MIN, REF_MAX, unit='dBm',
                  required_if=_yes('mode', 'manual')),
    ),
    'AUTO_SCALE': (
        ParamSpec('range_db', 'number', REF_RANGE_DB_MIN, REF_RANGE_DB_MAX, unit='dB'),
        # The level the user is looking at. It is a DISPLAY value, not a device Ref: in SDR the
        # client owns the scale and clamps it to its own range, so validating it against the
        # device's Ref bounds rejected legitimate fits (measured on the bench: the SDR display
        # sat at -60 dBm, the command was refused and pressing Auto looked dead).
        ParamSpec('current_ref', 'number', DISPLAY_REF_MIN_DBM, DISPLAY_REF_MAX_DBM, unit='dBm'),
    ),
    'SET_RBW': (
        ParamSpec('mode', 'choice', choices=('manual', 'auto'), default='auto'),
        ParamSpec('rbw', 'number', 100.0, RBW_MAX, unit='Hz',
                  required_if=_yes('mode', 'manual')),
    ),
    'SET_VBW': (
        ParamSpec('mode', 'choice',
                  choices=('manual', 'equal', 'tenth', 'bypass', 'onethousandth'),
                  default='bypass'),
        ParamSpec('vbw', 'number', 10.0, VBW_MAX, unit='Hz',
                  required_if=_yes('mode', 'manual')),
    ),
    'SET_SWEEP': (
        ParamSpec('mode', 'integer', 0, 8),
        ParamSpec('time', 'number', 0.0, 60.0, unit='s'),
    ),
    'SET_POINTS': (ParamSpec('points', 'integer', 51, POINTS_MAX, required=True),),
    'SET_SPUR': (ParamSpec('mode', 'choice', choices=('bypass', 'standard', 'enhanced'),
                           required=True),),
    'SET_WINDOW': (ParamSpec('window', 'integer', 0, 4, required=True),),
    'SET_DETECTOR': (ParamSpec('mode', 'choice',
                               choices=('auto', 'sample', 'pos_peak', 'neg_peak', 'rms',
                                        'auto_peak'), required=True),),
    'SET_AMP': (
        ParamSpec('atten', 'integer', -1, ATTEN_MAX, unit='dB', default=-1),
        ParamSpec('preamp', 'integer', 0, 1, default=0),
        ParamSpec('ifgain', 'integer', 0, IFGAIN_MAX, default=2),
        ParamSpec('gain_strategy', 'integer', 0, 1, default=0),
    ),
    'SET_REFCK': (ParamSpec('mode', 'choice',
                            choices=('internal', 'external', 'premium', 'external_forced'),
                            required=True),),
    'SET_REFCKOUT': (ParamSpec('on', 'boolean', required=True),),
    'SET_MODE': (ParamSpec('mode', 'choice',
                           choices=('std', 'harmonic', 'pnm', 'rta', 'sdr'),
                           default='std', required=True),),
    'SET_SDR': (
        ParamSpec('center', 'number', unit='Hz'),
        ParamSpec('decimate', 'integer', 1, DECIMATE_MAX),
    ),
    'SET_SDR_TUNE': (ParamSpec('listen', 'number', unit='Hz', required=True),),
    'SET_SDR_DEMOD': (
        ParamSpec('mode', 'choice', choices=('am', 'fm', 'nfm', 'wfm', 'usb', 'lsb', 'cw')),
        ParamSpec('deemph_us', 'number', SDR_DEEMPH_MIN_US, SDR_DEEMPH_MAX_US, unit='us'),
        ParamSpec('ifbw', 'number', SDR_IFBW_MIN_HZ, SDR_IFBW_MAX_HZ, unit='Hz'),
        ParamSpec('squelch', 'number', SQUELCH_MIN_DBFS, SQUELCH_MAX_DBFS, unit='dBFS'),
        ParamSpec('volume', 'number', SDR_VOLUME_MIN, SDR_VOLUME_MAX),
        ParamSpec('pitch', 'number', SDR_PITCH_MIN_HZ, SDR_PITCH_MAX_HZ, unit='Hz'),
        ParamSpec('agc', 'boolean'),
    ),
    'SET_RTA': (
        ParamSpec('center', 'number', unit='Hz'),
        ParamSpec('span', 'number', 1000.0, RTA_SPAN_MAX, unit='Hz'),
    ),
    'SET_TRIGGER': (
        ParamSpec('source', 'choice',
                  choices=('bus', 'freerun', 'level', 'external', 'timer')),
        ParamSpec('edge', 'choice', choices=('rising', 'falling', 'double')),
        ParamSpec('level', 'number', TRIG_MIN, TRIG_MAX, unit='dBm'),
        ParamSpec('safetime', 'number', 0.0, TRIGGER_TIME_MAX_S, unit='s'),
        ParamSpec('delay', 'number', 0.0, TRIGGER_TIME_MAX_S, unit='s'),
        ParamSpec('pretime', 'number', 0.0, TRIGGER_TIME_MAX_S, unit='s'),
        ParamSpec('acqtime', 'number', TRIGGER_ACQ_MIN_S, TRIGGER_ACQ_MAX_S, unit='s'),
        ParamSpec('retrigger', 'integer', 0, TRIGGER_RETRIGGER_MAX),
        ParamSpec('retriggerperiod', 'number', 0.0, TRIGGER_RETRIGGER_PERIOD_MAX_S, unit='s'),
        ParamSpec('out', 'choice', choices=('none', 'per_hop', 'per_sweep', 'per_profile')),
        ParamSpec('outpolarity', 'choice', choices=('positive', 'negative')),
    ),
    'SET_HARM': (
        ParamSpec('f0', 'number', unit='Hz'),
        ParamSpec('count', 'integer', 1, HARM_COUNT_MAX),
        ParamSpec('span', 'number', 1.0, HARM_SPAN_MAX_HZ, unit='Hz'),
    ),
    'SET_PNM': (
        ParamSpec('center', 'number', unit='Hz'),
        ParamSpec('threshold', 'number', TRIGGER_LEVEL_MIN_DBM, TRIGGER_LEVEL_MAX_DBM,
                  unit='dBm'),
        ParamSpec('traceavg', 'integer', 1, 1000),
        ParamSpec('start', 'number', PNM_CARRIER_MIN_HZ, PNM_CARRIER_MAX_HZ, unit='Hz'),
        ParamSpec('stop', 'number', PNM_CARRIER_MIN_HZ, PNM_OFFSET_MAX_HZ, unit='Hz'),
    ),
}


def _apply_spec(dev, data: dict, spec: ParamSpec) -> None:
    """Validate one field from its spec (and coerce it in place)."""
    if spec.kind == 'boolean':
        if spec.name in data and not isinstance(data[spec.name], bool):
            raise CommandError(f'{spec.name} must be a boolean', 'bool_required', key=spec.name)
        return
    if spec.kind == 'choice':
        _choice(data, spec.name, spec.choices, required=spec.is_required(data))
        return
    caps = _caps(dev) if (callable(spec.minimum) or callable(spec.maximum)) else dev.state.caps
    minimum = spec.bound(caps, 'min')
    maximum = spec.bound(caps, 'max')
    if spec.kind == 'integer':
        _integer(data, spec.name, minimum=minimum, maximum=maximum,
                 required=spec.is_required(data))
    else:
        _number(data, spec.name, minimum=minimum, maximum=maximum,
                required=spec.is_required(data))


# Cross-field rules that a per-field schema cannot express.
def _x_set_freq(dev, data):
    caps = _caps(dev)
    has_center_span = 'center' in data or 'span' in data
    has_start_stop = 'start' in data or 'stop' in data
    if has_center_span and has_start_stop:
        raise CommandError('use center/span or start/stop, not both', 'freq_mixed_assignment')
    if has_start_stop:
        _number(data, 'start', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz, required=True)
        _number(data, 'stop', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz, required=True)
        if data['stop'] - data['start'] < 100.0:
            raise CommandError('stop - start must be >= 100 Hz', 'span_too_small')
    elif has_center_span:
        _number(data, 'center', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz)
        _number(data, 'span', minimum=100.0, maximum=caps.freq_max_hz - caps.freq_min_hz)
    else:
        raise CommandError('SET_FREQ requires center/span or start/stop', 'freq_requires_pair')


def _x_set_sweep(dev, data):
    mode = data.get('mode')
    current = (dev.state.rta_sweep_time_mode if dev.state.mode == 'rta'
               else dev.state.sweep_time_mode)
    mode = current if mode is None else mode
    if mode == 7:
        _number(data, 'time', minimum=0.001, maximum=60.0, required=True)
    elif mode in (6, 8):
        _number(data, 'time', minimum=1.0, maximum=1000.0, required=True)


def _x_set_sdr(dev, data):
    if 'center' not in data and 'decimate' not in data:
        raise CommandError('SET_SDR requires center or decimate', 'sdr_requires_param')


def _x_set_rta(dev, data):
    if 'center' not in data and 'span' not in data:
        raise CommandError('SET_RTA requires center or span', 'rta_requires_pair')


def _x_set_pnm(dev, data):
    start, stop = data.get('start'), data.get('stop')
    if start is not None and stop is not None and start >= stop:
        raise CommandError('start must be lower than stop', 'range_invalid')


def _x_set_mode(dev, data):
    if data.get('mode') == 'pnm' and not dev.state.pnm_supported:
        raise CommandError('phase-noise measurement is not supported', 'pnm_unsupported')


EXTRA_VALIDATORS = {
    'SET_FREQ': _x_set_freq,
    'SET_SWEEP': _x_set_sweep,
    'SET_SDR': _x_set_sdr,
    'SET_RTA': _x_set_rta,
    'SET_PNM': _x_set_pnm,
    'SET_MODE': _x_set_mode,
}


# ---------------------------------------------------------------------------
# Command context + handlers
# ---------------------------------------------------------------------------


@dataclass
class CommandContext:
    """Everything a handler needs; keeps the handlers free of closures."""

    dev: object
    hw_call: Callable[..., Awaitable]

    @property
    def state(self):
        return self.dev.state

    async def configure_swp(self) -> None:
        result = await self.hw_call(self.dev.configure_swp)
        if result and result[0] is False:
            raise CommandError(f'device rejected the configuration: {result[1]}',
                               'hardware_config', detail=result[1])

    async def configure_active(self) -> None:
        session = self.dev.session
        if session is not None:
            await self.hw_call(session.reconfigure)
        else:
            await self.configure_swp()


async def _h_set_preset(ctx: CommandContext, data: dict) -> bool:
    # Preset restores the power-on defaults for EVERY mode without switching the current
    # one. Previously the SDR parameters and the shared front-end settings were left
    # untouched, and pressing Preset while in SDR reconfigured the device into SWP behind
    # the live SDR session.
    dev = ctx.dev
    dev.preset_state()
    dev.reset_rta_state()
    dev.reset_sdr_state()
    dev.reset_common_state()
    session = dev.session
    name = session.name if session is not None else 'std'
    if name == 'rta':
        # Preset supersedes the SWP restore point captured when RTA was entered.
        session.snapshot_current()
        await ctx.hw_call(session.reset_defaults)
    elif name == 'sdr':
        # Re-apply the IQS/DDC chain so the SDR session picks up the reset parameters.
        await ctx.hw_call(session.reconfigure)
    else:
        await ctx.configure_swp()
    return True


async def _h_cal_refclk(ctx: CommandContext, data: dict) -> bool:
    # Run the GNSS 1PPS calibration on a background thread, pausing the publisher during
    # it; the state is set synchronously for frontend feedback.
    dev, s = ctx.dev, ctx.state

    async def _cal():
        count = int(data.get('count', 10))
        try:
            command_lock = getattr(dev, 'command_lock', None)
            timeout = count * 1.2 + 10
            if command_lock is None:
                task = asyncio.to_thread(dev.calibrate_ref_clock, count)
                ok, freq = await asyncio.wait_for(task, timeout=timeout)
            else:
                async with command_lock:
                    task = asyncio.to_thread(dev.calibrate_ref_clock, count)
                    ok, freq = await asyncio.wait_for(task, timeout=timeout)
            if ok:
                dev.state.last_cal_freq = freq
                dev.state.refclk_ppm = (freq / 100e6 - 1.0) * 1e6
        except asyncio.TimeoutError:
            s.last_error = 'reference calibration timed out'
            fatal(s.last_error)
        except Exception as exc:
            s.last_error = f'reference calibration failed: {exc}'
            log.exception('Reference-clock calibration failed')
        finally:
            dev.state.calibrating = False

    if not dev.state.calibrating:
        dev.state.calibrating = True
        asyncio.create_task(_cal())
    return True


async def _h_connect(ctx: CommandContext, data: dict) -> bool:
    if ctx.state.connected:
        return False
    # reopen (close + open): the handle may be a dead one left by a bus error, so a plain
    # open() is not enough to recover (see HarogicDevice.reopen).
    ok, err = await ctx.hw_call(ctx.dev.reopen)
    if not ok:
        raise CommandError(f'device connection failed: {err}', 'connect_failed', detail=err)
    return True


async def _h_set_freq(ctx: CommandContext, data: dict) -> bool:
    from ..config import fit_center_span, fit_start_stop

    s = ctx.state
    if 'start' in data:
        s.center_hz, s.span_hz = fit_start_stop(data['start'], data['stop'], s.caps)
    else:
        center = data.get('center', s.center_hz)
        span = data.get('span', s.span_hz)
        s.center_hz, s.span_hz = fit_center_span(center, span, s.caps)
    ctx.dev.prepare_auto_reference_retune('std')
    await ctx.configure_swp()
    return True


async def _h_set_ref(ctx: CommandContext, data: dict) -> bool:
    dev, s = ctx.dev, ctx.state
    mode = data.get('mode', 'manual')
    if 'range_db' in data:
        s.ref_range_db = float(data['range_db'])
    session = dev.session
    if mode == 'auto':
        # Legacy spelling of the one-shot fit (presets and scripts sent it). There is no
        # tracking mode any more: quoting "auto" must not leave a mode latched on. In SDR the
        # client applies the reported target to its display reference.
        return await _run_auto_scale(ctx)
    if session is not None and session.name == 'sdr':
        if 'ref' in data:
            s.ref_level = data['ref']
        s.ref_mode = 'manual'
        dev.reset_auto_reference('sdr')     # a manual level ends any fit's authorship
        await ctx.hw_call(session.reconfigure)
        return True
    if session is not None and session.name == 'rta':
        await ctx.hw_call(session.set_reference, mode='manual', ref=data.get('ref'))
        return True
    s.ref_mode = 'manual'
    s.ref_level = data['ref']
    # The user owns the level now: forget the placement this loop made (the retune safety
    # must not lift a level the user chose) and the observations that went with it.
    dev.reset_auto_reference('std')
    await ctx.configure_swp()
    return True


async def _run_auto_scale(ctx: CommandContext, current: float | None = None) -> bool:
    """One-shot reference placement; returns whether the configuration changed."""
    dev = ctx.dev
    if dev.state.mode not in ('std', 'rta', 'sdr'):
        return False
    result, _target = dev.auto_scale(dev.auto_reference_scope(), current)
    # The client must re-read `auto_ref.target` (SDR applies it to the display scale), so a STATUS
    # push is needed even when the device itself was not reconfigured.
    return result != 'no_data'


async def _h_auto_scale(ctx: CommandContext, data: dict) -> bool:
    if 'range_db' in data:
        ctx.state.ref_range_db = float(data['range_db'])
    return await _run_auto_scale(ctx, data.get('current_ref'))


async def _h_set_rbw(ctx: CommandContext, data: dict) -> bool:
    dev, s = ctx.dev, ctx.state
    session = dev.session
    if session is not None and session.name == 'rta':
        await ctx.hw_call(session.set_rbw, mode=data.get('mode', 'auto'), rbw=data.get('rbw', 0))
        return True
    if 'mode' in data:
        s.rbw_mode = data['mode']
    if 'rbw' in data:
        s.rbw_hz = data['rbw']
    await ctx.configure_swp()
    return True


async def _h_set_vbw(ctx: CommandContext, data: dict) -> bool:
    dev, s = ctx.dev, ctx.state
    session = dev.session
    if session is not None and session.name == 'rta':
        await ctx.hw_call(session.set_vbw, mode=data.get('mode', 'equal'), vbw=data.get('vbw', 0))
        return True
    if 'mode' in data:
        s.vbw_mode = data['mode']
    if 'vbw' in data:
        s.vbw_hz = data['vbw']
    await ctx.configure_swp()
    return True


async def _h_set_sweep(ctx: CommandContext, data: dict) -> bool:
    dev, s = ctx.dev, ctx.state
    session = dev.session
    if session is not None and session.name == 'rta':
        await ctx.hw_call(
            session.set_sweep,
            mode=int(data.get('mode', s.rta_sweep_time_mode)),
            time=float(data.get('time', s.rta_sweep_time) or 0),
        )
        return True
    if 'mode' in data:
        mode = int(data['mode'])
        if 0 <= mode <= 8:
            s.sweep_time_mode = mode
    if 'time' in data:
        s.sweep_time = max(0.0, min(1e6, float(data['time'])))
    await ctx.configure_swp()
    return True


async def _h_set_points(ctx: CommandContext, data: dict) -> bool:
    ctx.state.points_req = data['points']
    await ctx.configure_swp()
    return True


async def _h_set_spur(ctx: CommandContext, data: dict) -> bool:
    ctx.state.spur_mode = data['mode']
    await ctx.configure_swp()
    return True


async def _h_set_window(ctx: CommandContext, data: dict) -> bool:
    ctx.state.window = data['window']
    await ctx.configure_swp()
    return True


async def _h_set_detector(ctx: CommandContext, data: dict) -> bool:
    ctx.state.detector = data['mode']
    await ctx.configure_swp()
    return True


async def _h_set_amp(ctx: CommandContext, data: dict) -> bool:
    s = ctx.state
    caps = s.caps
    if 'atten' in data:
        limit = caps.atten_max if caps else 33
        s.atten = int(max(-1, min(limit, int(data['atten']))))
    if 'preamp' in data:
        s.preamplifier = 1 if data['preamp'] else 0
    if 'ifgain' in data:
        limit = caps.ifgain_max if caps else 3
        s.ifgain = int(max(0, min(limit, int(data['ifgain']))))
    if 'gain_strategy' in data:
        s.gain_strategy = 1 if data['gain_strategy'] else 0
    await ctx.configure_active()
    return True


async def _h_set_refck(ctx: CommandContext, data: dict) -> bool:
    ctx.state.ref_clock = data['mode']
    await ctx.configure_active()
    return True


async def _h_set_refckout(ctx: CommandContext, data: dict) -> bool:
    ctx.state.refclk_out = data['on']
    await ctx.configure_active()
    return True


async def _h_set_mode(ctx: CommandContext, data: dict) -> bool:
    from ..measurements import SessionManager, SessionNotReady

    dev = ctx.dev
    name = data.get('mode', 'std')
    if name not in ('std', 'harmonic', 'pnm', 'rta', 'sdr'):
        return False

    def _switch():
        try:
            SessionManager(dev).switch(name)
        except SessionNotReady:
            # "Requested" is not "in effect": a failure inside a session's configure path
            # used to look like a successful switch (an AttributeError in the RTA
            # auto-recovery path did exactly that), so the UI believed it had switched.
            raise CommandError(f'mode switch to {name} failed to become ready',
                               'mode_not_ready') from None

    await ctx.hw_call(_switch)
    return True


async def _h_set_sdr(ctx: CommandContext, data: dict) -> bool:
    session = ctx.dev.session
    if session is None or session.name != 'sdr':
        raise CommandError('SET_SDR requires SDR mode', 'sdr_mode_required')
    await ctx.hw_call(session.set_params, center=data.get('center'),
                      decimate=data.get('decimate'))
    return True


async def _h_set_sdr_tune(ctx: CommandContext, data: dict) -> bool:
    session = ctx.dev.session
    if session is None or session.name != 'sdr':
        raise CommandError('SET_SDR_TUNE requires SDR mode', 'sdr_mode_required')
    # State-only: no DLL call, applied by the acquisition loop with a software NCO, so
    # dragging stays smooth (no 180 ms DDC reconfiguration per tune).
    session.set_tune(data['listen'])
    return True


async def _h_set_sdr_demod(ctx: CommandContext, data: dict) -> bool:
    session = ctx.dev.session
    if session is None or session.name != 'sdr':
        raise CommandError('SET_SDR_DEMOD requires SDR mode', 'sdr_mode_required')
    await ctx.hw_call(session.set_demod, mode=data.get('mode'), if_bw=data.get('ifbw'),
                      squelch=data.get('squelch'), volume=data.get('volume'),
                      agc=data.get('agc'), pitch=data.get('pitch'),
                      deemph_us=data.get('deemph_us'))
    return True


async def _h_set_rta(ctx: CommandContext, data: dict) -> bool:
    session = ctx.dev.session
    if session is None or session.name != 'rta':
        raise CommandError('SET_RTA requires RTA mode', 'rta_mode_required')
    await ctx.hw_call(session.set_params, center=data.get('center'), span=data.get('span'))
    return True


async def _h_set_trigger(ctx: CommandContext, data: dict) -> bool:
    s = ctx.state
    for key, attr in (('source', 'trigger_source'), ('edge', 'trigger_edge'),
                      ('out', 'trigger_out'), ('outpolarity', 'trigger_out_polarity')):
        if key in data:
            setattr(s, attr, data[key])
    for key, attr in (('level', 'trigger_level_dbm'), ('safetime', 'trigger_safe_time_s'),
                      ('delay', 'trigger_delay_s'), ('pretime', 'trigger_pre_time_s'),
                      ('acqtime', 'trigger_acq_time_s'),
                      ('retriggerperiod', 'trigger_retrigger_period_s')):
        if key in data:
            setattr(s, attr, float(data[key]))
    if 'retrigger' in data:
        s.trigger_retrigger_count = int(data['retrigger'])
    # already in RTA: re-apply the profile now, otherwise the values are used on entry
    session = ctx.dev.session
    if session is not None and session.name == 'rta':
        await ctx.hw_call(session.set_trigger)
    return True


async def _h_set_harm(ctx: CommandContext, data: dict) -> bool:
    session = ctx.dev.session
    if session is not None and session.name == 'harmonic':
        session.set_params(f0=data.get('f0'), count=data.get('count'), span=data.get('span'))
        return True
    return False


async def _h_set_pnm(ctx: CommandContext, data: dict) -> bool:
    session = ctx.dev.session
    if session is not None and session.name == 'pnm':
        session.set_params(center=data.get('center'), threshold=data.get('threshold'),
                           traceavg=data.get('traceavg'), start=data.get('start'),
                           stop=data.get('stop'))
        return True
    return False


async def _h_status(ctx: CommandContext, data: dict) -> bool:
    return False


# ---------------------------------------------------------------------------
# The table
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class CommandSpec:
    """One command: its parameter schema, its handler and whether it needs a device."""

    name: str
    run: Callable[..., Awaitable[bool]]
    #: Wire parameters, validated from PARAMS (report finding E-1).
    params: tuple[ParamSpec, ...] = ()
    #: Cross-field rule that a per-field schema cannot express.
    extra: Callable[[object, dict], None] | None = None
    #: Commands that need a live device (STATUS/CONNECT are the exceptions).
    needs_device: bool = True

    def validate(self, dev, data) -> None:
        for spec in self.params:
            _apply_spec(dev, data, spec)
        if self.extra is not None:
            self.extra(dev, data)


_REGISTRY = (
    ('STATUS', _h_status, (), None, False),
    ('CONNECT', _h_connect, (), None, False),
    ('SET_PRESET', _h_set_preset, (), None, True),
    ('CAL_REFCLK', _h_cal_refclk, (), None, True),
    ('SET_FREQ', _h_set_freq, (), None, True),
    ('SET_REF', _h_set_ref, (), None, True),
    ('AUTO_SCALE', _h_auto_scale, (), None, True),
    ('SET_RBW', _h_set_rbw, (), None, True),
    ('SET_VBW', _h_set_vbw, (), None, True),
    ('SET_SWEEP', _h_set_sweep, (), None, True),
    ('SET_POINTS', _h_set_points, (), None, True),
    ('SET_SPUR', _h_set_spur, (), None, True),
    ('SET_WINDOW', _h_set_window, (), None, True),
    ('SET_DETECTOR', _h_set_detector, (), None, True),
    ('SET_AMP', _h_set_amp, (), None, True),
    ('SET_REFCK', _h_set_refck, (), None, True),
    ('SET_REFCKOUT', _h_set_refckout, (), None, True),
    ('SET_MODE', _h_set_mode, (), None, True),
    ('SET_SDR', _h_set_sdr, (), None, True),
    ('SET_SDR_TUNE', _h_set_sdr_tune, (), None, True),
    ('SET_SDR_DEMOD', _h_set_sdr_demod, (), None, True),
    ('SET_RTA', _h_set_rta, (), None, True),
    ('SET_TRIGGER', _h_set_trigger, (), None, True),
    ('SET_HARM', _h_set_harm, (), None, True),
    ('SET_PNM', _h_set_pnm, (), None, True),
)

#: The command table: parameter schema from PARAMS, cross-field rule from
#: EXTRA_VALIDATORS, handler from the tuple above.
COMMANDS: dict[str, CommandSpec] = {
    name: CommandSpec(name=name, run=run, params=PARAMS.get(name, ()),
                      extra=EXTRA_VALIDATORS.get(name), needs_device=needs_device)
    for name, run, _params, _extra, needs_device in _REGISTRY
}


def build_schema(dev=None) -> dict:
    """Machine-readable command/parameter description (served on /api/schema).

    This is the E-1 seam: the frontend can generate numeric/enum/boolean controls and their
    bounds from the same declaration the backend validates with, instead of keeping a third
    copy of every range (report finding E-1).
    """
    caps = getattr(getattr(dev, 'state', None), 'caps', None)
    commands = {}
    for name, spec in COMMANDS.items():
        # A capability-backed bound resolves to None without a device attached.
        resolved = [param.to_json(caps) for param in spec.params]
        commands[name] = {
            'params': resolved,
            'needs_device': spec.needs_device,
            'session_exclusive': name in SWP_OWNED,
            'swp_only': name in SWP_ONLY,
            'denied_in_sdr': name in NOT_IN_SDR,
        }
    return {'commands': commands}


def command_names() -> frozenset[str]:
    return frozenset(COMMANDS)


def spec_for(cmd) -> CommandSpec:
    """Return the spec for ``cmd`` or raise the transport-level unknown-command error."""
    if not isinstance(cmd, str) or cmd not in COMMANDS:
        raise CommandError('unknown or missing command', 'unknown_command')
    return COMMANDS[cmd]


def validate(dev, cmd, data) -> CommandSpec:
    """Check the envelope, the connection state, the mode/session guards and the payload.

    The mode guards live here (not in the dispatcher) so they are declarative: a command
    that must not run in a mode or while a measurement owns the device declares it in the
    table above rather than adding another `if` to the dispatch chain.
    """
    if not isinstance(data, dict):
        raise CommandError('JSON message must be an object', 'json_object_required')
    spec = spec_for(cmd)
    if spec.needs_device and not dev.state.connected:
        raise CommandError('device is not connected', 'device_not_connected')

    session = getattr(dev, 'session', None)
    if session is not None and session.name in ('harmonic', 'pnm') and cmd in SWP_OWNED:
        raise CommandError(
            f'{cmd} is not available while the {session.name} measurement is active',
            'cmd_unavailable_measurement', cmd=cmd, session=session.name)
    if dev.state.mode == 'sdr' and cmd in NOT_IN_SDR:
        raise CommandError(f'{cmd} is not available in SDR mode', 'sdr_unsupported', cmd=cmd)
    if dev.state.mode == 'rta' and cmd in SWP_ONLY:
        raise CommandError(f'{cmd} is only available in SWP mode', 'swp_only', cmd=cmd)

    if spec.validate is not None:
        spec.validate(dev, data)
    return spec


async def dispatch(ctx: CommandContext, cmd, data) -> bool:
    """Validate and run one command; returns whether STATUS needs to be re-sent."""
    spec = validate(ctx.dev, cmd, data)
    return await spec.run(ctx, data)
