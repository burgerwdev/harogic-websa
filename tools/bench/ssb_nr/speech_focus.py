"""Offline DFN speech protection / selective enhancement experiment.

The neural estimate is reconstructed from two attenuation-limited outputs of
the SAME model and input. A third output must verify the mixing identity before
this is used. No stem is treated as clean reference speech.

Unlike a whole-track volume change, this varies residual preservation and a
bounded presence emphasis at each time/frequency location. Speech probability
can be wrong; naturalness, consonants and callsigns require listening evidence.
This is an experimental DFN/statistical combination, not a trained new model.
"""
from dataclasses import asdict, dataclass

import numpy as np
from scipy.ndimage import gaussian_filter1d


def recover_components(low, high, low_db=10.0, high_db=20.0):
    """Invert Y_a=(1-a)*E+a*X. E is the model estimate, X the noisy input."""
    if np.shape(low) != np.shape(high) or not 0 < low_db < high_db:
        raise ValueError("need matching outputs and 0 < low_db < high_db")
    a, b = 10**(-low_db/20), 10**(-high_db/20)
    estimated = (a*high-b*low)/(a-b)
    original = ((1-b)*low-(1-a)*high)/(a-b)
    return original, estimated


@dataclass(frozen=True)
class FocusConfig:
    noise_atten_db: float = 20.0
    speech_residual_atten_db: float = 8.0
    voice_boost_db: float = 0.0
    attack_ms: float = 8.0
    release_ms: float = 80.0
    stats_average_ms: float = 24.0
    fixed_presence: bool = False
    gain_smoothing_hz: float = 0.0
    rate: int = 48000
    fft: int = 1536
    hop: int = 384


def enhance(original, estimated, probability, probability_times, probability_hz, config):
    """Same-length aligned 48 kHz WOLA; budget 32 ms additional frame latency.

    Original and estimated must already share the model's time alignment.
    Probability evidence is from current/past 32 ms frames. Using aligned offline
    stems does not remove the original DFN's causal delay in a live realization.
    """
    x, e = np.asarray(original, dtype=float), np.asarray(estimated, dtype=float)
    if x.ndim != 1 or e.shape != x.shape or not np.all(np.isfinite(x)) or not np.all(np.isfinite(e)):
        raise ValueError("need matching finite mono stems")
    if (config.rate, config.fft, config.hop) != (48000, 1536, 384):
        raise ValueError("this prototype fixes 48 kHz / 32 ms / 8 ms")
    if not 0 < config.speech_residual_atten_db <= config.noise_atten_db or not 0 <= config.voice_boost_db <= 6:
        raise ValueError("invalid residual bounds or voice emphasis")
    if (not np.isfinite(config.gain_smoothing_hz) or config.gain_smoothing_hz < 0
            or any(not np.isfinite(v) or v <= 0 for v in
                   [config.attack_ms, config.release_ms, config.stats_average_ms])):
        raise ValueError("invalid smoothing or envelope time")
    n, hop, rate = config.fft, config.hop, config.rate
    p = np.asarray(probability, dtype=float)
    times, hz = np.asarray(probability_times), np.asarray(probability_hz)
    if p.shape != (len(times), len(hz)) or not np.all(np.isfinite(p)) or np.any((p < 0) | (p > 1)):
        raise ValueError("invalid speech evidence")
    pad = n-hop
    xp, ep = np.pad(x,(pad,n)), np.pad(e,(pad,n))
    out, normalization = np.zeros_like(xp), np.zeros_like(xp)
    win = np.hamming(n)
    win /= np.sqrt(np.sum(win**2))
    f = np.fft.rfftfreq(n,1/rate)
    band = (f >= 270) & (f <= 2700)
    # Raised-cosine presence emphasis, restricted to the occupied speech band.
    presence_weight = np.zeros_like(f)
    selected = (f >= 500) & (f <= 2700)
    presence_weight[selected] = np.sin(np.pi*(f[selected]-500)/2200)**2
    a_noise, a_voice = 10**(-config.noise_atten_db/20), 10**(-config.speech_residual_atten_db/20)
    attack = 1-np.exp(-hop/rate/(config.attack_ms/1000))
    release = 1-np.exp(-hop/rate/(config.release_ms/1000))
    average = np.exp(-hop/rate/(config.stats_average_ms/1000))
    confidence, stat_average = np.zeros_like(f), np.zeros_like(f)
    records = []
    ir_lag = np.minimum(np.arange(n),n-np.arange(n))/rate
    for start in range(0,len(xp)-n+1,hop):
        t = (start-pad+n/2)/rate
        X, E = np.fft.rfft(xp[start:start+n]*win), np.fft.rfft(ep[start:start+n]*win)
        # Evidence centers agree at 8 ms intervals. Never choose a future
        # analysis frame as a result of roundoff or nearest-neighbor selection.
        index = np.searchsorted(times,t+1e-10,side='right')-1
        stat = np.zeros_like(f) if index < 0 else np.interp(f,hz,p[min(index,len(times)-1)],left=0,right=0)
        stat[~band] = 0
        stat = gaussian_filter1d(stat,.8,mode='nearest')
        stat_average = average*stat_average+(1-average)*stat
        stable_stat = np.clip((stat_average-.15)/.65,0,1)
        power_original = gaussian_filter1d(abs(X)**2,1,mode='nearest')
        power_model = gaussian_filter1d(abs(E)**2,1,mode='nearest')
        ratio_db = 10*np.log10(np.maximum(power_model,1e-20)/np.maximum(power_original,1e-20))
        neural = 1/(1+np.exp(np.clip(-(ratio_db+15)/3,-60,60)))
        neural[power_original < 1e-12] = 0
        neural[~band] = 0
        # Union of neural evidence and a persistent statistical weak-speech cue.
        target = 1-(1-neural)*(1-stable_stat)
        coefficient = np.where(target > confidence,attack,release)
        confidence += coefficient*(target-confidence)
        # Smooth only the rendered gain. Keep evidence/envelope history identical
        # so frequency smoothing and release time can be tested independently.
        rendered = (gaussian_filter1d(confidence,config.gain_smoothing_hz/(rate/n),mode='nearest')
                    if config.gain_smoothing_hz > 0 else confidence)
        residual_mix = a_noise+(a_voice-a_noise)*rendered
        voice_gain = 10**(config.voice_boost_db*(1 if config.fixed_presence else rendered)*presence_weight/20)
        Y = voice_gain*((1-residual_mix)*E+residual_mix*X)
        out[start:start+n] += np.fft.irfft(Y,n)*win
        normalization[start:start+n] += win**2
        # A frozen gain's impulse response diagnoses spectral ringing. This is
        # ONLY the original-signal residual branch, not the nonlinear whole DFN
        # or a measured room response / reverberation time.
        residual_ir = np.fft.irfft(residual_mix*voice_gain,n)
        ir_energy = np.sum(residual_ir**2)+1e-30
        late_ir = np.sum(residual_ir[ir_lag > .004]**2)/ir_energy
        roughness = np.mean(np.abs(np.diff(20*np.log10(np.maximum(residual_mix[band],1e-20)))))
        records.append([t,float(np.mean(confidence[band])),float(np.quantile(confidence[band],.9)),
                        float(np.mean(residual_mix[band])),float(np.max(voice_gain)),
                        float(late_ir),float(roughness)])
    out /= np.maximum(normalization,1e-20)
    result = out[pad:pad+len(x)]
    if not np.all(np.isfinite(result)):
        raise ValueError("non-finite reconstructed PCM")
    return result, {"config":asdict(config),"frame_summary":np.asarray(records),
        "columns":["time_seconds","mean_confidence","p90_confidence","mean_residual_mix","max_voice_gain",
                   "residual_branch_frozen_ir_energy_outside_4ms_fraction","adjacent_bin_gain_difference_db"]}
