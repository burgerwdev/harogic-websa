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
    'SET_FREQ', 'SET_REF', 'SET_RBW', 'SET_VBW', 'SET_SWEEP', 'SET_POINTS',
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
# Per-command validation
# ---------------------------------------------------------------------------


def _v_cal_refclk(dev, data):
    _integer(data, 'count', minimum=REFCLK_CAL_COUNT_MIN, maximum=REFCLK_CAL_COUNT_MAX)


def _v_set_freq(dev, data):
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


def _v_set_ref(dev, data):
    mode = _choice(data, 'mode', ('manual', 'auto')) or 'manual'
    _number(data, 'range_db', minimum=REF_RANGE_DB_MIN, maximum=REF_RANGE_DB_MAX)
    if mode == 'manual':
        caps = _caps(dev)
        _number(data, 'ref', minimum=caps.ref_min_dbm, maximum=caps.ref_max_dbm, required=True)


def _v_set_rbw(dev, data):
    mode = _choice(data, 'mode', ('manual', 'auto')) or 'auto'
    if mode == 'manual':
        _number(data, 'rbw', minimum=100.0, maximum=_caps(dev).rbw_max_hz, required=True)


def _v_set_vbw(dev, data):
    mode = _choice(data, 'mode', ('manual', 'equal', 'tenth', 'bypass', 'onethousandth')) or 'bypass'
    if mode == 'manual':
        _number(data, 'vbw', minimum=10.0, maximum=_caps(dev).vbw_max_hz, required=True)


def _v_set_sweep(dev, data):
    mode = _integer(data, 'mode', minimum=0, maximum=8)
    current_mode = (dev.state.rta_sweep_time_mode if dev.state.mode == 'rta'
                    else dev.state.sweep_time_mode)
    mode = current_mode if mode is None else mode
    if mode == 7:
        _number(data, 'time', minimum=0.001, maximum=60.0, required=True)
    elif mode in (6, 8):
        _number(data, 'time', minimum=1.0, maximum=1000.0, required=True)
    else:
        _number(data, 'time', minimum=0.0, maximum=60.0)


def _v_set_points(dev, data):
    _integer(data, 'points', minimum=51, maximum=_caps(dev).points_max, required=True)


def _v_set_spur(dev, data):
    _choice(data, 'mode', ('bypass', 'standard', 'enhanced'), required=True)


def _v_set_window(dev, data):
    _integer(data, 'window', minimum=0, maximum=4, required=True)


def _v_set_detector(dev, data):
    _choice(data, 'mode', ('auto', 'sample', 'pos_peak', 'neg_peak', 'rms', 'auto_peak'),
            required=True)


def _v_set_amp(dev, data):
    caps = _caps(dev)
    _integer(data, 'atten', minimum=-1, maximum=caps.atten_max)
    _integer(data, 'preamp', minimum=0, maximum=1)
    _integer(data, 'ifgain', minimum=0, maximum=caps.ifgain_max)
    _integer(data, 'gain_strategy', minimum=0, maximum=1)


def _v_set_refck(dev, data):
    _choice(data, 'mode', ('internal', 'external', 'premium', 'external_forced'), required=True)


def _v_set_refckout(dev, data):
    if not isinstance(data.get('on'), bool):
        raise CommandError('on must be a boolean', 'bool_required', key='on')


def _v_set_mode(dev, data):
    mode = _choice(data, 'mode', ('std', 'harmonic', 'pnm', 'rta', 'sdr'), required=True)
    if mode == 'pnm' and not dev.state.pnm_supported:
        raise CommandError('phase-noise measurement is not supported', 'pnm_unsupported')


def _v_set_sdr(dev, data):
    caps = _caps(dev)
    _number(data, 'center', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz)
    _integer(data, 'decimate', minimum=1, maximum=caps.decimate_max)
    if 'center' not in data and 'decimate' not in data:
        raise CommandError('SET_SDR requires center or decimate', 'sdr_requires_param')


def _v_set_sdr_tune(dev, data):
    caps = _caps(dev)
    _number(data, 'listen', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz, required=True)


def _v_set_sdr_demod(dev, data):
    _choice(data, 'mode', ('am', 'fm', 'nfm', 'wfm', 'usb', 'lsb', 'cw'))
    _number(data, 'deemph_us', minimum=SDR_DEEMPH_MIN_US, maximum=SDR_DEEMPH_MAX_US)
    _number(data, 'ifbw', minimum=SDR_IFBW_MIN_HZ, maximum=SDR_IFBW_MAX_HZ)
    _number(data, 'squelch', minimum=SQUELCH_MIN_DBFS, maximum=SQUELCH_MAX_DBFS)
    _number(data, 'volume', minimum=SDR_VOLUME_MIN, maximum=SDR_VOLUME_MAX)
    _number(data, 'pitch', minimum=SDR_PITCH_MIN_HZ, maximum=SDR_PITCH_MAX_HZ)
    if 'agc' in data and not isinstance(data['agc'], bool):
        raise CommandError('agc must be a boolean', 'bool_required', key='agc')


def _v_set_rta(dev, data):
    caps = _caps(dev)
    _number(data, 'center', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz)
    _number(data, 'span', minimum=1000.0, maximum=caps.rta_span_max_hz)
    if 'center' not in data and 'span' not in data:
        raise CommandError('SET_RTA requires center or span', 'rta_requires_pair')


def _v_set_trigger(dev, data):
    caps = dev.state.caps
    lo = caps.trigger_level_min_dbm if caps else TRIGGER_LEVEL_MIN_DBM
    hi = caps.trigger_level_max_dbm if caps else TRIGGER_LEVEL_MAX_DBM
    _choice(data, 'source', ('bus', 'freerun', 'level', 'external', 'timer'))
    _choice(data, 'edge', ('rising', 'falling', 'double'))
    _number(data, 'level', minimum=lo, maximum=hi)
    _number(data, 'safetime', minimum=0.0, maximum=TRIGGER_TIME_MAX_S)
    _number(data, 'delay', minimum=0.0, maximum=TRIGGER_TIME_MAX_S)
    _number(data, 'pretime', minimum=0.0, maximum=TRIGGER_TIME_MAX_S)
    _number(data, 'acqtime', minimum=TRIGGER_ACQ_MIN_S, maximum=TRIGGER_ACQ_MAX_S)
    _integer(data, 'retrigger', minimum=0, maximum=TRIGGER_RETRIGGER_MAX)
    _number(data, 'retriggerperiod', minimum=0.0, maximum=TRIGGER_RETRIGGER_PERIOD_MAX_S)
    _choice(data, 'out', ('none', 'per_hop', 'per_sweep', 'per_profile'))
    _choice(data, 'outpolarity', ('positive', 'negative'))


def _v_set_harm(dev, data):
    caps = _caps(dev)
    _number(data, 'f0', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz)
    _integer(data, 'count', minimum=1, maximum=HARM_COUNT_MAX)
    _number(data, 'span', minimum=1.0, maximum=HARM_SPAN_MAX_HZ)


def _v_set_pnm(dev, data):
    caps = _caps(dev)
    _number(data, 'center', minimum=caps.freq_min_hz, maximum=caps.freq_max_hz)
    _number(data, 'threshold', minimum=TRIGGER_LEVEL_MIN_DBM, maximum=TRIGGER_LEVEL_MAX_DBM)
    _integer(data, 'traceavg', minimum=1, maximum=1000)
    start = _number(data, 'start', minimum=PNM_CARRIER_MIN_HZ, maximum=PNM_CARRIER_MAX_HZ)
    stop = _number(data, 'stop', minimum=PNM_CARRIER_MIN_HZ, maximum=PNM_OFFSET_MAX_HZ)
    if start is not None and stop is not None and start >= stop:
        raise CommandError('start must be lower than stop', 'range_invalid')


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
    ok, err = await ctx.hw_call(ctx.dev.open)
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
    if session is not None and session.name == 'sdr':
        if mode == 'manual' and 'ref' in data:
            s.ref_level = data['ref']
        s.ref_mode = mode
        await ctx.hw_call(session.reconfigure)
        return True
    if session is not None and session.name == 'rta':
        await ctx.hw_call(session.set_reference, mode=mode, ref=data.get('ref'))
        return True
    s.ref_mode = mode
    dev.reset_auto_reference('std')
    if mode == 'manual':
        s.ref_level = data['ref']
        await ctx.configure_swp()
    return True


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
    from ..measurements import make_session

    dev = ctx.dev
    name = data.get('mode', 'std')
    if name not in ('std', 'harmonic', 'pnm', 'rta', 'sdr'):
        return False

    def _switch():
        old_session = dev.session
        if old_session is not None and old_session.name in ('rta', 'sdr'):
            old_session._ready = False   # stop the worker loop first (best-effort)
        session = make_session(dev, name)
        dev.set_session(session)
        # "Requested" is not "in effect": verify the session actually became ready.
        # Without this a failure inside a session's configure path left the mode unchanged
        # while the command still reported success (an AttributeError in the RTA
        # auto-recovery path did exactly that), so the UI believed it had switched.
        if name != 'std' and not getattr(session, '_ready', True):
            raise CommandError(f'mode switch to {name} failed to become ready',
                               'mode_not_ready')

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
    """One command: how to validate it, what it does, and where it may run."""

    name: str
    run: Callable[..., Awaitable[bool]]
    validate: Callable[[object, dict], None] | None = None
    #: Commands that need a live device (STATUS/CONNECT are the exceptions).
    needs_device: bool = True


COMMANDS: dict[str, CommandSpec] = {
    spec.name: spec for spec in (
        CommandSpec('STATUS', _h_status, needs_device=False),
        CommandSpec('CONNECT', _h_connect, needs_device=False),
        CommandSpec('SET_PRESET', _h_set_preset),
        CommandSpec('CAL_REFCLK', _h_cal_refclk, _v_cal_refclk),
        CommandSpec('SET_FREQ', _h_set_freq, _v_set_freq),
        CommandSpec('SET_REF', _h_set_ref, _v_set_ref),
        CommandSpec('SET_RBW', _h_set_rbw, _v_set_rbw),
        CommandSpec('SET_VBW', _h_set_vbw, _v_set_vbw),
        CommandSpec('SET_SWEEP', _h_set_sweep, _v_set_sweep),
        CommandSpec('SET_POINTS', _h_set_points, _v_set_points),
        CommandSpec('SET_SPUR', _h_set_spur, _v_set_spur),
        CommandSpec('SET_WINDOW', _h_set_window, _v_set_window),
        CommandSpec('SET_DETECTOR', _h_set_detector, _v_set_detector),
        CommandSpec('SET_AMP', _h_set_amp, _v_set_amp),
        CommandSpec('SET_REFCK', _h_set_refck, _v_set_refck),
        CommandSpec('SET_REFCKOUT', _h_set_refckout, _v_set_refckout),
        CommandSpec('SET_MODE', _h_set_mode, _v_set_mode),
        CommandSpec('SET_SDR', _h_set_sdr, _v_set_sdr),
        CommandSpec('SET_SDR_TUNE', _h_set_sdr_tune, _v_set_sdr_tune),
        CommandSpec('SET_SDR_DEMOD', _h_set_sdr_demod, _v_set_sdr_demod),
        CommandSpec('SET_RTA', _h_set_rta, _v_set_rta),
        CommandSpec('SET_TRIGGER', _h_set_trigger, _v_set_trigger),
        CommandSpec('SET_HARM', _h_set_harm, _v_set_harm),
        CommandSpec('SET_PNM', _h_set_pnm, _v_set_pnm),
    )
}


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
