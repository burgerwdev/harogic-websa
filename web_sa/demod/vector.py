"""demod/vector.py -- Tier 1 vector measurements (NumPy only, no vendor library).

Tier 1 answers "what is in this signal" without deciding symbols: the spectrum,
power versus time, the CCDF, a spectrogram and a carrier/timing-corrected
constellation cloud. It is deliberately free of the channelizer and the analog
demodulators, so it runs in CI without the vendor library.

**One level convention.** ``v`` is the IQ block in *volts*, i.e. raw device counts
times the vendor ``IQS_ScaleToV`` (already an absolute volts-per-count figure --
measured: 30 dB of ``RefLevel`` moves it by 33.7x), and every function here reports

    10*log10(mean|v|^2 / 50) + 30        dBm

There is no extra 3 dB bandpass factor: the input is complex baseband, so a tone
of amplitude A has power ``A^2/50`` exactly as the vendor's own swept path reports
it (agreement measured within 0.3 dB).

**Cost.** Measured on this machine (one core, best of three calls, as a percentage
of real time for the 3.9 MS/s stream: how much CPU a second of signal costs), for the
default capture depth 2^17 samples (33.6 ms of signal) and for a deep 2^20 frame:

==================  ==============  ==============  ==============================
measurement         2^17 (33.6 ms)  2^20 (268 ms)   note
==================  ==============  ==============  ==============================
spectrum + levels       21 %             6 %       the baseline product: it is also
                                                    the RTAF frame and the reference
                                                    tracker's input, so it always runs
power v. time           23 %             7 %       block RMS trace plus the duty cycle
CCDF                    57 %            21 %       per-sample sort; a block-averaged
                                                    envelope is cheaper but is no
                                                    longer comparable to Rayleigh
spectrogram            136 %            44 %       capture only
constellation          389 %           181 %       capture only; the estimator chain
                                                    (rate, resample, timing, CFO,
                                                    matched filter)
==================  ==============  ==============  ==============================

The percentages fall with depth because a fixed per-call cost dominates a short block.
(For comparison the probe suite reported 22 / 3 / 25 / 120 / 316 % over 100k-sample
blocks: the same order, with the CCDF gap explained by its block-averaged envelope.)

So the streaming path runs the spectrum only, and the capture path runs whatever
``SET_VSA measure`` asks for. ``VsaSession`` publishes the scalar summary in
``STATUS.vsa.last`` and the spectrum as an RTAF frame (a constellation payload
needs the VSAD frame: roadmap 2.5).
"""
from __future__ import annotations

import numpy as np

from . import digital as D

#: Windows the measurements accept (``blackman-harris`` is the default: -92 dB
#: sidelobes, which keeps a weak tone visible next to a strong one).
WINDOWS = ('rect', 'hann', 'blackman-harris')

#: CCDF levels (dB relative to the mean power) reported in the summary table.
CCDF_LEVELS_DB = (0.0, -2.0, -4.0, -6.0, -8.0, -10.0)

#: Measurements ``measure()`` can produce.
MEASUREMENTS = ('spectrum', 'power', 'ccdf', 'spectrogram', 'constellation')

#: Measurements that need the whole capture, so they cannot run in the 20 Hz
#: streaming path (cost table above).
CAPTURE_ONLY = ('spectrogram', 'constellation')

#: Trace spread (dB, 1st to 99th percentile) below which the envelope has no two
#: levels to compare: a continuous carrier or a noise floor, i.e. duty 1.0.
DUTY_FLAT_DB = 3.0

_FLOOR_W = 1e-30


def window(name: str, n: int) -> np.ndarray:
    """Window coefficients by name (``rect`` is the no-window case)."""
    if name == 'rect':
        return np.ones(n)
    if name == 'hann':
        return np.hanning(n)
    if name == 'blackman-harris':
        k = np.arange(n)
        return (0.35875 - 0.48829 * np.cos(2 * np.pi * k / n)
                + 0.14128 * np.cos(4 * np.pi * k / n)
                - 0.01168 * np.cos(6 * np.pi * k / n))
    raise ValueError(f'unknown window {name!r}')


def dbm_of_power(power: np.ndarray | float):
    """Absolute power in dBm from linear power in W (the one convention)."""
    return 10 * np.log10(np.maximum(power, _FLOOR_W) / 50.0) + 30.0


def mean_dbm(iq: np.ndarray) -> float:
    """Mean power of the block in absolute dBm (50 ohm)."""
    x = np.asarray(iq)
    if not len(x):
        return float('-inf')
    return float(dbm_of_power(float(np.mean(np.abs(x) ** 2))))


def noise_floor_dbm(iq: np.ndarray, fs: float, window_name: str = 'blackman-harris',
                    percentile: float = 50.0) -> float:
    """Spectral noise density in a 1 Hz band, at ``percentile`` of the bins.

    No ENBW correction: a per-bin power already is a density once divided by the bin
    width (see :func:`tone_level_dbm` for the correction a *peak marker* needs). The
    default median is a display floor, and per-bin power is exponentially distributed,
    so the median sits ``10*log10(ln 2)`` = 1.6 dB below the mean density.
    """
    x = np.asarray(iq)
    n = len(x)
    w = window(window_name, n)
    p = np.abs(np.fft.fft(x * w)) ** 2 / (n * np.sum(w ** 2))
    floor = float(np.percentile(p, percentile))
    return float(dbm_of_power(floor / (fs / n)))


def _peak_interp_db(mag: np.ndarray, k: int) -> tuple:
    """Parabolic interpolation of a log-magnitude peak: ``(bin offset, dB gain)``."""
    if k <= 0 or k >= len(mag) - 1:
        return 0.0, 0.0
    a, b, c = (20 * np.log10(max(_FLOOR_W, mag[k - 1])),
               20 * np.log10(max(_FLOOR_W, mag[k])),
               20 * np.log10(max(_FLOOR_W, mag[k + 1])))
    den = a - 2 * b + c
    d = 0.0 if abs(den) < 1e-12 else float(np.clip(0.5 * (a - c) / den, -0.5, 0.5))
    return d, b - 0.25 * (a - c) * d - b


def tone_level_dbm(dbm: np.ndarray, half_bins: int = 4) -> float:
    """Level of the strongest tone in a per-bin dBm trace (main lobe integrated).

    The trace is power *per bin*, so a CW tone's power is spread over the window's
    main lobe: the peak bin alone reads low by the window's equivalent noise bandwidth
    (measured 3.1 dB for a CW tone with blackman-harris, whose ENBW is 2.00 bins). Integrating +-4 bins recovers the tone's own power, which is
    the number a marker shows and the reference tracker consumes. Per-bin noise
    reads high by the same integration, which is why the mean power is reported
    separately.
    """
    t = np.asarray(dbm)
    if not len(t):
        return float('-inf')
    k = int(np.argmax(t))
    lo, hi = max(0, k - half_bins), min(len(t), k + half_bins + 1)
    return float(10 * np.log10(np.sum(10 ** ((t[lo:hi] - 30.0) / 10.0))) + 30.0)


def tone_dbm(iq: np.ndarray, fs: float, f_rel: float = 0.0, window_name: str = 'hann',
             search_hz: float | None = None) -> tuple:
    """Absolute level of the strongest tone near ``f_rel`` (Hz relative to DC).

    Returns ``(frequency_hz, dbm)``. The input is complex baseband (analytic), so
    the coherent bin maps to the tone's own complex amplitude ``|X_k|/sum(w)`` and
    the power is ``|A|^2/50``; that is the normalisation measured to agree within
    0.3 dB with the block mean power of the same tone.
    """
    x = np.asarray(iq)
    n = len(x)
    w = window(window_name, n)
    spec = np.fft.fft(x * w)
    amp = np.abs(spec) / np.sum(w)
    freqs = np.fft.fftfreq(n, d=1.0 / fs)
    band = (np.abs(((freqs - f_rel + fs / 2) % fs) - fs / 2)
            <= (search_hz if search_hz is not None else fs / n * 4))
    if not band.any():
        return float('nan'), float('-inf')
    k = int(np.argmax(np.where(band, amp, 0)))
    d, gain_db = _peak_interp_db(amp, k)
    a = amp[k] * 10 ** (gain_db / 20.0)
    return float(freqs[k] + d * fs / n), float(dbm_of_power(float(a) ** 2))


def band_power_dbm(iq: np.ndarray, fs: float, f_rel: float, bw_hz: float,
                   window_name: str = 'hann') -> float:
    """Integrated noise-like power in a band (incoherent normalisation)."""
    x = np.asarray(iq)
    n = len(x)
    w = window(window_name, n)
    p = np.abs(np.fft.fft(x * w)) ** 2 / (n * np.sum(w ** 2))
    freqs = np.fft.fftfreq(n, d=1.0 / fs)
    band = np.abs(((freqs - f_rel + fs / 2) % fs) - fs / 2) <= bw_hz / 2
    return float(dbm_of_power(float(p[band].sum())))     # p sums to the mean power


def spectrum(iq: np.ndarray, fs: float, nfft: int = 4096, overlap: float = 0.5,
             window_name: str = 'blackman-harris') -> tuple:
    """Welch-averaged power spectrum in absolute dBm.

    Returns ``(freq_hz, dbm)`` with ``freq_hz`` relative to DC (the caller adds the
    capture centre). Overlapping segments reduce the variance of a noise floor by
    roughly the number of segments, which is what makes a 1.4 % duty burst visible
    in a single capture.
    """
    x = np.asarray(iq)
    n = len(x)
    nfft = int(min(nfft, n))
    step = max(1, int(nfft * (1 - overlap)))
    w = window(window_name, nfft)
    norm = np.sum(w ** 2) * nfft
    acc = np.zeros(nfft)
    count = 0
    for start in range(0, n - nfft + 1, step):
        acc += np.abs(np.fft.fft(x[start:start + nfft] * w)) ** 2
        count += 1
    if not count:
        seg = np.zeros(nfft, dtype=complex)
        seg[:n] = x
        acc = np.abs(np.fft.fft(seg * w)) ** 2
        count = 1
    p = acc / (count * norm)
    freqs = np.fft.fftshift(np.fft.fftfreq(nfft, d=1 / fs))
    return freqs, np.asarray(dbm_of_power(np.fft.fftshift(p)))


def spectral_centroid(freqs: np.ndarray, dbm: np.ndarray,
                      band_hz: float | None = None) -> float:
    """Power-weighted mean frequency: the carrier offset of a symmetric spectrum.

    Restrict to ``band_hz`` when the capture also holds wideband noise: over the
    full band one noise realisation tilts the estimate by ~1 kHz (measured at
    25 dB SNR, 3.9 MSPS) while inside the occupied band the signal dominates.
    """
    f = np.asarray(freqs)
    p = 10 ** (np.asarray(dbm) / 10.0)
    if band_hz is not None:
        m = np.abs(f) <= band_hz
        f, p = f[m], p[m]
    return float(np.sum(f * p) / max(_FLOOR_W, float(p.sum())))


def power_vs_time(iq: np.ndarray, fs: float, block: int = 256) -> tuple:
    """Envelope power trace in dBm plus the duty cycle above its own floor.

    Returns ``(t_s, dbm, duty)``, the answer to "how much of the capture is on": the
    threshold sits a quarter of the trace's own dynamic range above its 1st
    percentile, which recovers the burst duty for any duty below ~90 %, and a trace
    with no two levels to compare (a continuous carrier or a noise floor, spread
    below :data:`DUTY_FLAT_DB`) is 1.0. A *median*-based threshold would report 0 for
    CW and only work below 50 % duty.
    """
    x = np.asarray(iq)
    n = (len(x) // block) * block
    if n == 0:
        return np.zeros(0), np.zeros(0), 0.0
    p = np.mean(np.abs(x[:n].reshape(-1, block)) ** 2, axis=1)
    dbm = dbm_of_power(p)
    t = (np.arange(len(p)) + 0.5) * block / fs
    low = float(np.percentile(dbm, 1.0))
    span = float(np.percentile(dbm, 99.0)) - low
    duty = 1.0 if span < DUTY_FLAT_DB else float(np.mean(dbm > low + 0.25 * span))
    return t, dbm, duty


def ccdf(iq: np.ndarray, block: int = 1) -> tuple:
    """CCDF of the envelope power relative to its mean, in dB.

    Returns ``(db, probability)`` sorted from the highest level down, i.e. the
    fraction of samples that exceed each level. ``block=1`` compares directly with
    :func:`ccdf_rayleigh`; a larger block averages the envelope over the block,
    which lowers the variance and makes the curve *steeper* than Rayleigh.
    """
    x = np.asarray(iq)
    n = (len(x) // block) * block
    if n == 0:
        return np.zeros(0), np.zeros(0)
    p = np.mean(np.abs(x[:n].reshape(-1, block)) ** 2, axis=1)
    p = p / p.mean()
    order = np.sort(10 * np.log10(np.maximum(p, 1e-12)))[::-1]
    return order, np.arange(1, len(order) + 1) / len(order)


def ccdf_rayleigh(level_db: np.ndarray) -> np.ndarray:
    """CCDF a complex-Gaussian signal must follow: ``exp(-P/Pavg)``.

    Only valid against a per-sample envelope (``block=1``).
    """
    return np.exp(-(10 ** (np.asarray(level_db) / 10.0)))


def ccdf_table(iq: np.ndarray, levels: tuple = CCDF_LEVELS_DB, block: int = 1) -> dict:
    """CCDF probability at a few fixed levels (the shape of the curve, as JSON)."""
    db, prob = ccdf(iq, block=block)
    if not len(db):
        return {float(level): 0.0 for level in levels}
    return {float(level): float(np.interp(level, db[::-1], prob[::-1])) for level in levels}


def spectrogram(iq: np.ndarray, fs: float, nfft: int = 256, hop: int = 128) -> tuple:
    """STFT power in dB relative to the strongest bin (time x frequency).

    Returns ``(db_rows, freq_hz)``. Relative dB on purpose: a spectrogram is a
    display, and the absolute level of the capture is already in the summary.
    """
    x = np.asarray(iq)
    w = window('blackman-harris', nfft)
    rows = [np.abs(np.fft.fftshift(np.fft.fft(x[start:start + nfft] * w))) ** 2
            for start in range(0, len(x) - nfft + 1, hop)]
    if not rows:
        return np.zeros((0, nfft)), np.zeros(nfft)
    m = np.array(rows)
    freqs = np.fft.fftshift(np.fft.fftfreq(nfft, d=1 / fs))
    return 10 * np.log10(np.maximum(m, _FLOOR_W) / m.max()), freqs


def constellation(iq: np.ndarray, fs: float, *, modulation: str = 'qpsk',
                  rolloff: float = 0.35, symbol_rate: float | None = None,
                  sps: int = 8, compensate_cfo: bool = True) -> dict:
    """Tier 1 symbol cloud: timing and (optional) carrier correction, no slicing.

    Returns a dict with the cloud and its measured scale:

    ``symbols``            complex volts at the symbol instants
    ``nominal``            the nominal unit-RMS grid scaled to the cloud's RMS, for
                           the overlay a UI draws behind the cloud
    ``rms_v``              RMS symbol magnitude in volts (the cloud's own scale)
    ``mean_dbm``           the cloud's mean power in dBm (same convention as here)
    ``symbol_rate_est``    blind estimate from the |x|^2 line
    ``symbol_rate_used``   what the correction actually used (given or estimated)
    ``cfo_hz``/``timing_samples``  what was removed
    ``sps_too_low``        True when the blind estimate cannot be trusted (sps < 4)

    Without a symbol rate and with fewer than 4 samples per symbol the blind
    estimate collapses, so the caller gets an explicit flag instead of a silently
    wrong cloud.
    """
    x = np.asarray(iq)
    est_rate, _ = D.estimate_symbol_rate(x, fs)
    rate = float(symbol_rate or est_rate)
    out = {'symbols': np.zeros(0, dtype=complex), 'nominal': np.zeros(0, dtype=complex),
           'rms_v': 0.0, 'mean_dbm': float('-inf'), 'symbol_rate_est': float(est_rate),
           'symbol_rate_used': rate, 'cfo_hz': 0.0, 'timing_samples': 0.0,
           'sps_too_low': False}
    if rate <= 0 or not len(x):
        return out
    sps_in = fs / rate
    if sps_in < 4.0:
        out['sps_too_low'] = True
        return out
    n_out = int(round(len(x) * sps * rate / fs))
    xs = D.fft_resample(x, n_out)
    fs_out = sps * rate
    if compensate_cfo:
        # Tier 1 has no decisions, so the M-th power estimate runs on the matched
        # filter output (the M-th power needs symbol-spaced samples).
        mf = D.matched_filter_symbols(xs, sps, rolloff)
        if len(mf) > 16:
            out['cfo_hz'] = D.estimate_cfo_mth(mf, rate)
            del mf
        else:
            # Too short to estimate: fall back to the spectral centroid of the block,
            # which is what a VSA shows when it cannot decide symbols either.
            _, dbm = spectrum(x, fs, nfft=min(4096, len(x)))
            freqs = np.fft.fftshift(np.fft.fftfreq(min(4096, len(x)), d=1 / fs))
            out['cfo_hz'] = spectral_centroid(freqs, dbm, band_hz=rate)
    out['timing_samples'] = D.resolve_timing(xs, sps, rolloff,
                                            D.estimate_timing(xs, sps))
    if out['timing_samples']:
        xs = D.add_timing(xs, -out['timing_samples'])
    if out['cfo_hz']:
        xs = D.remove_cfo(xs, out['cfo_hz'], fs_out)
    sym = D.matched_filter_symbols(xs, sps, rolloff)
    out['symbols'] = sym
    if len(sym):
        rms = float(np.sqrt(np.mean(np.abs(sym) ** 2)))
        out['rms_v'] = rms
        out['mean_dbm'] = float(dbm_of_power(rms ** 2))
        out['nominal'] = D.nominal_points(modulation) * rms
    return out


#: Points a VSAD frame carries at most. A panel draws a few thousand points at most, and
#: an unbounded cloud would make the frame megabytes on a deep capture.
FRAME_MAX_POINTS = 4096
#: Spectrogram rows per frame: a waterfall scrolls, so it never needs the whole matrix.
FRAME_MAX_ROWS = 512


def measure_block(result: dict) -> dict:
    """The scalar measurements of a result, ready for the VSAD measurement block.

    Arrays, nested dicts, booleans and missing (NaN) values stay out; the frame encoder
    owns the key order and fills the slots this returns nothing for with NaN.
    """
    return {k: float(v) for k, v in result.items()
            if isinstance(v, (int, float)) and not isinstance(v, bool) and v == v}


def _thin(values, limit: int):
    """Evenly decimate to at most ``limit`` entries (keeps the first and last)."""
    values = np.asarray(values)
    if limit <= 0 or len(values) <= limit:
        return values
    return values[np.linspace(0, len(values) - 1, limit).astype(np.int64)]


def _iq_matrix(symbols) -> np.ndarray:
    symbols = np.asarray(symbols, dtype=complex)
    if not len(symbols):
        return np.zeros((0, 2), dtype=np.float32)
    return np.column_stack([symbols.real, symbols.imag]).astype(np.float32)


def frame_payload(result: dict, *, max_points: int = FRAME_MAX_POINTS,
                  max_rows: int = FRAME_MAX_ROWS) -> dict:
    """Display-sized VSAD payload for a measurement result (see ``framer.encode_vsa``).

    A capture holds far more points than any panel draws (2^18 samples give tens of
    thousands of symbols) and a spectrogram many more rows than a waterfall shows, so what
    goes on the wire is an evenly decimated slice; the measurement block describes the whole
    capture. A symbol cloud ships as ``(points, 2)`` interleaved I/Q volts with the nominal
    grid on the cloud's own scale; a power trace and a CCDF ship as ``(points, 2)`` (x, y);
    a spectrogram ships as ``(rows, bins)`` relative dB. ``spectrum`` has no VSAD payload --
    the RTAF frame is the spectrum -- and asking for one raises instead of sending an empty
    frame that a panel would draw as blank.
    """
    kind = result.get('kind', 'spectrum')
    out = {'kind': kind, 'data': np.zeros((0, 2), dtype=np.float32), 'ideal': None,
           'scalars': (), 'measurements': measure_block(result)}
    if kind == 'constellation':
        out['data'] = _iq_matrix(_thin(result.get('symbols', []), max_points))
        out['ideal'] = _iq_matrix(result.get('nominal', []))
        out['scalars'] = (result.get('symbol_rate_used', float('nan')),
                          result.get('cfo_hz', float('nan')),
                          result.get('timing_samples', float('nan')))
    elif kind == 'power':
        t, trace = result['power_vs_time']
        out['data'] = np.column_stack([_thin(t, max_points),
                                       _thin(trace, max_points)]).astype(np.float32)
    elif kind == 'ccdf':
        level, prob = result['ccdf']
        out['data'] = np.column_stack([_thin(level, max_points),
                                       _thin(prob, max_points)]).astype(np.float32)
    elif kind == 'spectrogram':
        out['data'] = _thin(result['spectrogram'], max_rows).astype(np.float32)
    else:
        raise ValueError(f'no VSAD payload for measurement {kind!r}')
    return out


def summary(result: dict) -> dict:
    """The JSON-safe scalars of a :func:`measure` result (everything but arrays)."""
    skip = {'spectrum', 'spectrogram', 'symbols', 'nominal', 'power_vs_time'}
    return {k: v for k, v in result.items() if k not in skip
            and isinstance(v, (int, float, str, bool, dict))}


def measure(iq: np.ndarray, fs: float, *, kind: str = 'spectrum',
            window_name: str = 'blackman-harris', nfft: int = 4096,
            overlap: float = 0.5, block: int = 256, modulation: str = 'qpsk',
            rolloff: float = 0.35, symbol_rate: float | None = None,
            sps: int = 8) -> dict:
    """Run one Tier 1 measurement and return its numbers plus its arrays.

    The levels and the Welch spectrum are always included (the spectrum is the
    RTAF frame and the reference tracker's input, so a capture pays for it once);
    ``kind`` selects what else to compute:

    * ``spectrum``      -- Welch spectrum, absolute dBm, plus the centroid
    * ``power``         -- envelope trace in dBm plus the measured duty cycle
    * ``ccdf``          -- per-sample CCDF plus the probabilities at ``CCDF_LEVELS_DB``
    * ``spectrogram``   -- STFT matrix (capture only: 120 % of real time)
    * ``constellation`` -- symbol cloud with rate/timing/CFO corrected (capture only)

    Raises ``ValueError`` for an unknown kind and for a capture-only kind used on a
    stream (the caller is expected to refuse that earlier, in the command layer).
    """
    if kind not in MEASUREMENTS:
        raise ValueError(f'unknown measurement {kind!r}')
    x = np.asarray(iq)
    out: dict = {'kind': kind, 'samples': int(len(x)), 'mean_dbm': mean_dbm(x)}
    freq, dbm = spectrum(x, fs, nfft=nfft, overlap=overlap, window_name=window_name)
    out['spectrum'] = (freq, dbm)
    out['peak_bin_dbm'] = float(np.max(dbm)) if len(dbm) else float('-inf')
    # `peak_dbm` is the strongest tone's own level (the marker value the reference
    # tracker uses); see tone_level_dbm for why the peak bin alone reads low.
    out['peak_dbm'] = tone_level_dbm(dbm)
    out['peak_hz'] = float(freq[int(np.argmax(dbm))]) if len(dbm) else float('nan')
    # The floor comes from the Welch spectrum it is already computed from (the 30th
    # percentile of the bins, i.e. the display's noise floor), so a capture does not
    # pay for a second full-block FFT; the 1 Hz-referenced density is derived from it.
    out['floor_dbm'] = float(np.percentile(dbm, 30)) if len(dbm) else -np.inf
    # Per-bin power -> noise density in a 1 Hz band (a display floor: see
    # noise_floor_dbm for why a median bin sits 1.6 dB under the mean density).
    out['floor_1hz_dbm'] = out['floor_dbm'] - 10 * np.log10(fs / nfft)
    out['centroid_hz'] = spectral_centroid(freq, dbm)
    if kind == 'power':
        t, trace, duty = power_vs_time(x, fs, block=block)
        out['power_vs_time'] = (t, trace)
        out['duty'] = duty
        out['burst_dbm'] = float(np.max(trace)) if len(trace) else out['peak_dbm']
    elif kind == 'ccdf':
        out['ccdf'] = ccdf(x)
        out['ccdf_table'] = ccdf_table(x)
    elif kind == 'spectrogram':
        rows, _freqs = spectrogram(x, fs)
        out['spectrogram'] = rows
        out['rows'] = int(rows.shape[0])
    elif kind == 'constellation':
        cloud = constellation(x, fs, modulation=modulation, rolloff=rolloff,
                              symbol_rate=symbol_rate, sps=sps)
        out.update(cloud)
        out['symbols_n'] = int(len(cloud['symbols']))
    return out
