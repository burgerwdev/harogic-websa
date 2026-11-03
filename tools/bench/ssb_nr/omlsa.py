"""Offline OM-LSA / two-stage IMCRA experiment, NOT a production NR implementation.

Equations: Cohen/Berdugo (2001), Cohen (2003), and Cohen's public omlsa.m
(https://israelcohen.com/wp-content/uploads/2020/11/omlsa.zip).
This research implementation follows the reference recurrences and records its
SSB-specific departures explicitly. The author's MATLAB is downloaded separately,
not vendored. Its 'all rights reserved' notice supplies no production license.

All estimates use the current/past STFT frames; no entire-file noise quantile or
clean reference is used. The first nonzero frame initializes the noise estimator;
it is NOT assumed to be clean noise. This is a known startup limitation.
"""
from dataclasses import asdict, dataclass

import numpy as np
from scipy.special import exp1


@dataclass
class Config:
    profile: str = "ssb"
    attenuation_db: float = 10.0
    rate: int = 16000
    fft: int = 512
    hop: int = 128
    low_hz: float = 270.0
    high_hz: float = 2700.0


class OmlsaImcra:
    def __init__(self, config):
        self.c = config
        if config.profile not in ("author", "ssb"):
            raise ValueError("profile must be author or ssb")
        if (config.rate, config.fft, config.hop) != (16000, 512, 128):
            raise ValueError("this experiment fixes STFT to 16k / 512 / 128")
        self.nbin = config.fft // 2 + 1
        self.alpha_s, self.alpha_d = .9, .85
        self.alpha_eta, self.alpha_xi = .95, .7
        self.minimum_eta = 10 ** (-18 / 10)
        self.floor = 10 ** (-config.attenuation_db / 20)
        self.b = np.hanning(3)
        self.b /= self.b.sum()
        self.global_kernel = np.hanning(31)
        self.global_kernel /= self.global_kernel.sum()
        hz = np.fft.rfftfreq(config.fft, 1/config.rate)
        lo, hi = (50, 10000) if config.profile == "author" else (config.low_hz, config.high_hz)
        self.band = (hz >= lo) & (hz <= hi)
        self.k2, self.k3 = round(500/config.rate*config.fft), round(3500/config.rate*config.fft)
        self.initialized = False
        self.frame = 0
        self.xi_frame, self.xi_peak_db = 0.0, 0.0
        self.eta_previous = np.ones(self.nbin)
        self.xi = np.zeros(self.nbin)
        self.block_count = 0
        self.minima, self.minima_t = [], []

    @staticmethod
    def probability(db):
        return .005 + .995 * np.clip((db + 10) / 5, 0, 1)

    @staticmethod
    def posterior(q, eta, v):
        q = np.clip(q, 0, 1-1e-12)
        return 1 / (1 + q / (1-q) * (1+eta) * np.exp(-v))

    def gain(self, power):
        p = np.asarray(power, dtype=np.float64)
        if p.shape != (self.nbin,) or not np.all(np.isfinite(p)) or np.any(p < 0):
            raise ValueError("invalid frame power")
        # Zero padding must not initialize or poison the tracker.
        if not self.initialized and np.max(p) < 1e-20:
            return np.ones_like(p), np.zeros_like(p), np.zeros_like(p)
        self.frame += 1
        sf = np.convolve(p, self.b, mode="same")
        if not self.initialized:
            self.noise, self.noise_av = p.copy(), p.copy()
            self.s, self.st, self.sy = sf.copy(), sf.copy(), p.copy()
            self.initialized = True
        else:
            self.s = self.alpha_s*self.s + (1-self.alpha_s)*sf
        gamma = p / np.maximum(self.noise, 1e-10)
        eta = np.maximum(self.alpha_eta*self.eta_previous + (1-self.alpha_eta)*np.maximum(gamma-1, 0), self.minimum_eta)
        v = gamma*eta/(1+eta)

        # IMCRA first pass: minima of a frequency/time smoothed periodogram.
        if self.frame < 15:
            self.smin, self.active = self.s.copy(), self.s.copy()
        else:
            self.smin = np.minimum(self.smin, self.s)
            self.active = np.minimum(self.active, self.s)
        mask = ((p < 4.6*1.66*self.smin) & (self.s < 1.67*1.66*self.smin)).astype(float)
        count = np.convolve(mask, self.b, mode="same")
        masked_power = np.convolve(mask*p, self.b, mode="same")
        sft = self.st.copy()
        np.divide(masked_power, count, out=sft, where=count > 0)

        # IMCRA second pass: exclude probable speech before tracking minima.
        if self.frame < 15:
            self.st = self.s.copy()
            self.smint, self.active_t = self.st.copy(), self.st.copy()
        else:
            self.st = self.alpha_s*self.st + (1-self.alpha_s)*sft
            self.smint = np.minimum(self.smint, self.st)
            self.active_t = np.minimum(self.active_t, self.st)
        gamma_min = p/(1.66*np.maximum(self.smint, 1e-10))
        zeta = self.s/(1.66*np.maximum(self.smint, 1e-10))
        qhat = np.ones_like(p)
        between = (gamma_min > 1) & (gamma_min < 3) & (zeta < 1.67)
        qhat[between] = (3-gamma_min[between])/2
        phat = np.zeros_like(p)
        phat[between] = self.posterior(qhat[between], eta[between], v[between])
        phat[(gamma_min >= 3) | (zeta >= 1.67)] = 1
        alpha = self.alpha_d + (1-self.alpha_d)*phat
        self.noise_av = alpha*self.noise_av + (1-alpha)*p
        if self.frame < 15:
            self.noise_long = self.noise_av.copy()
        else:
            alpha_long = .99+.01*phat
            self.noise_long = alpha_long*self.noise_long+(1-alpha_long)*p
        self.block_count += 1
        if self.block_count == 15:
            self.block_count = 0
            if not self.minima:
                self.minima = [self.s.copy() for _ in range(8)]
                self.minima_t = [self.st.copy() for _ in range(8)]
            else:
                self.minima = self.minima[1:]+[self.active.copy()]
                self.minima_t = self.minima_t[1:]+[self.active_t.copy()]
                self.smin = np.min(self.minima, axis=0)
                self.smint = np.min(self.minima_t, axis=0)
                self.active, self.active_t = self.s.copy(), self.st.copy()
        self.noise = 1.4685*self.noise_av  # author's medium nonstationarity setting

        # OM-LSA speech presence prior: local, broad band, and frame evidence.
        self.xi = self.alpha_xi*self.xi + (1-self.alpha_xi)*eta
        xi_global = np.convolve(self.xi, self.global_kernel, mode="same")
        plocal = self.probability(10*np.log10(np.maximum(self.xi, 1e-10)))
        pglobal = self.probability(10*np.log10(np.maximum(xi_global, 1e-10)))
        previous_frame = self.xi_frame
        self.xi_frame = float(np.mean(self.xi[self.band]))
        frame_db = 10*np.log10(max(self.xi_frame, 1e-10))
        if frame_db <= -10:
            pframe = .005
        elif self.xi_frame >= previous_frame:
            self.xi_peak_db = np.clip(frame_db, 0, 10)
            pframe = 1.0
        else:
            pframe = float(self.probability(frame_db-self.xi_peak_db))
        if self.c.profile == "author":
            # Preserve the author's hard low-probability and tonal decisions.
            mean_plocal = np.mean(plocal[2:self.k2+self.k3-1])
            if mean_plocal < .25:
                plocal[self.k2:self.k3+1] = .005
            if mean_plocal < .5 and self.frame > 120:
                indices = np.flatnonzero(self.noise_long[7:-8] > 2.5*(self.noise_long[9:-6]+self.noise_long[5:-10]))
                plocal[np.concatenate([indices+6, indices+7, indices+8])] = .005
        q = np.minimum(1-plocal*pglobal*pframe, .998)
        gamma = p/np.maximum(self.noise, 1e-10)
        eta = np.maximum(self.alpha_eta*self.eta_previous + (1-self.alpha_eta)*np.maximum(gamma-1, 0), self.minimum_eta)
        v = gamma*eta/(1+eta)
        ph1 = self.posterior(q, eta, v)
        if self.c.profile == "author":
            ph1[q >= .9] = 0
        gh1 = eta/(1+eta)
        small = (v <= 5) & (v > 0)
        gh1[small] *= np.exp(.5*exp1(v[small]))
        if self.c.profile == "author":
            global_noise = self.noise.copy()
            global_noise[3:-3] = np.minimum.reduce([self.noise[3:-3], self.noise[:-6], self.noise[6:]])
            self.sy = .8*self.sy + .2*p
            gh0 = self.floor*np.sqrt(global_noise/(self.sy+1e-10))
        else:
            gh0 = self.floor
        g = np.exp(ph1*np.log(np.maximum(gh1, 1e-20))+(1-ph1)*np.log(np.maximum(gh0, 1e-20)))
        self.eta_previous = gh1**2*gamma
        if self.c.profile == "ssb":
            # Bound ALL attenuation, including weak consonants; no tone gate.
            g = np.clip(g, self.floor, 1)
        return g, self.noise.copy(), ph1


def enhance(samples, config, unity=False):
    """Aligned WOLA, returns same-length audio and frame diagnostics.

    Analysis 32 ms / hop 8 ms. A causal realization can emit an 8 ms hop
    after the 32 ms window is complete (24 ms overlap + hop scheduling).
    Offline timestamps undo WOLA framing, not a claimed zero latency system.
    """
    x = np.asarray(samples, dtype=np.float64)
    if x.ndim != 1 or not np.all(np.isfinite(x)):
        raise ValueError("expected finite mono PCM")
    n, hop = config.fft, config.hop
    pad = n-hop
    padded = np.pad(x, (pad, n))
    window = np.hamming(n)
    window /= np.sqrt(np.sum(window**2))
    out, norm = np.zeros_like(padded), np.zeros_like(padded)
    estimator = OmlsaImcra(config)
    gains, noises, probs, times = [], [], [], []
    for start in range(0, len(padded)-n+1, hop):
        spectrum = np.fft.rfft(padded[start:start+n]*window)
        if unity:
            gain, noise, probability = np.ones(len(spectrum)), np.zeros(len(spectrum)), np.zeros(len(spectrum))
        else:
            gain, noise, probability = estimator.gain(abs(spectrum)**2)
        if config.profile == "author" and not unity:
            gain[:3] = 0
            gain[-1] = 0
        reconstructed = np.fft.irfft(spectrum*gain, n)*window
        out[start:start+n] += reconstructed
        norm[start:start+n] += window**2
        gains.append(gain); noises.append(noise); probs.append(probability)
        times.append((start-pad+n/2)/config.rate)
    out /= np.maximum(norm, 1e-20)
    diagnostics = {"gain":np.array(gains),"noise_psd":np.array(noises),
                   "speech_probability":np.array(probs),"time":np.array(times),
                   "frequency":np.fft.rfftfreq(n,1/config.rate),"config":asdict(config)}
    return out[pad:pad+len(x)], diagnostics
