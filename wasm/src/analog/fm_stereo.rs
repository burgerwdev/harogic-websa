//! The FM stereo side chain: pilot, L−R and RDS carriers out of the discriminator's composite.
//!
//! A broadcast WFM composite is, in order of frequency:
//!
//! ```text
//!   L+R            0 .. 15 kHz      (this is all the mono path keeps)
//!   pilot         19 kHz            (sin, 8..10% of deviation)
//!   L−R (DSB-SC)  23 .. 53 kHz      around the suppressed 38 kHz carrier (2 × pilot)
//!   RDS           57 kHz ± 2.4 kHz  (the pilot's third harmonic, 90° shifted)
//! ```
//!
//! The mono path must not change (the reference fixture pins it), so this chain is a *side*
//! consumer of the same discriminator output. It uses the structure every small FM stereo
//! implementation uses (`rtl_fm`, gr-rds): one cheap NCO mix and one narrow one-pole IIR per
//! carrier, no long FIRs. The whole chain costs a few percent of real time; a 129-tap FIR bank
//! measured 54%, which starved the WebSocket loop.
//!
//! The NCOs start retarded by the band filter's group delay, because the side chain reads the
//! detector output and the detector output trails the baseband. With that offset, the 19 kHz
//! pilot IIR locks to the true pilot phase, tripling its phase gives the 57 kHz RDS reference,
//! and the L−R product detector runs on a plain 38 kHz NCO with the L/R sides intact.
//!
//! The side chain publishes:
//!
//! * the pilot amplitude (stereo detection, with hysteresis on the smoothed ratio),
//! * the L−R audio, resampled to the consumer's rate, ready to matrix against the mono path,
//! * the RDS state (`rds.rs` steps per aux sample; the PI code, the PS name, the radio text).

use crate::ddc::fir::{design_lowpass, FirState};

/// The pilot frequency in the composite (Hz).
pub const PILOT_HZ: f64 = 19_000.0;
/// The DSB-SC subcarrier of the stereo difference signal (Hz).
pub const SUBCARRIER_HZ: f64 = 38_000.0;

/// Stereo detection thresholds on the Goertzel pilot-to-carrier ratio.
/// Time constants of the reported statistics: the deviation holds its peak and decays slowly
/// (a quasi-peak detector, the broadcast-monitor convention); the levels average.
const PEAK_DECAY_S: f64 = 3.0;
/// Averaging of the per-block deviation reading before the quasi-peak hold (calm readout).
const DEV_SMOOTH_TAU_S: f64 = 1.0;
/// Bins in the deviation meter's per-block histogram, and the tail it discards. The tail exists
/// to drop discriminator wraps: on a capture where the modulation touches the sample rate's limit
/// they were 0.006% of the samples, well inside the 0.1% the percentile ignores.
const DEV_BINS: usize = 256;
const DEV_TAIL: f64 = 1.0e-3;
/// The pilot detector's neighbouring band, in Hz: a narrow band there measures the noise beside
/// the pilot. The audio ends at 15 kHz and the L−R DSB starts at 23 kHz, so 21 kHz is clean.
const PILOT_NBR_HZ: f64 = 21_000.0;
/// Pilot presence test, in dB of (19 kHz band) over (21 kHz band). Measured on the bench: with no
/// signal 5 dB, a mono transmission 1 dB, a piloted one 14-30 dB. The badge requires the pilot to
/// clear this, so an empty frequency cannot read STEREO — the amplitude ratio alone could not tell
/// noise from a pilot (noise measured 0.039 against the 0.030 acquire threshold).
const PILOT_SNR_DB: f64 = 9.0;
const PILOT_SNR_RELEASE_DB: f64 = 6.0;
/// Averaging of the peak deviation and of the pilot amplitude behind the reported ratio. Both are
/// slow on purpose: a speech or music programme swings the peak deviation by more than 100% over a
/// second (measured on the bench with a speech recording), so a ratio built on short windows is
/// unreadable however it is smoothed afterwards.
const PEAK_TAU_S: f64 = 2.0;
const PILOT_TAU_S: f64 = 2.0;
const STEREO_ACQUIRE: f64 = 0.030;
const STEREO_RELEASE: f64 = 0.018;
/// How many consecutive batches the ratio must exceed the acquire threshold.
const STEREO_COUNT: u32 = 10;
/// Batches skipped for detection: the band filter's startup transient wraps the detector.
const WARMUP_BATCHES: u32 = 1;
/// The pilot detector's one-pole bandwidth. Narrow on purpose: the stereo badge compares the
/// pilot's amplitude against the composite's RMS, and a wide window collects the discriminator's
/// noise around 19 kHz, which lifts a MONO signal's ratio toward a stereo one. On real captures a
/// 300 Hz window measured 0.022 for mono against 0.051 for a piloted one; 50 Hz separates them by
/// roughly five to one, so the hysteresis has room.
const PILOT_BW_HZ: f64 = 50.0;
/// The pilot PLL: a type-II loop that steers the NCO's frequency to the pilot the phasor
/// measures, so the bit clock and every derived reference follow the transmitter's own clock
/// (measured: the two TCXOs sit a fraction of a hertz apart at 19 kHz and drift while the
/// Pluto warms, and a free-running NCO slips a bit every few tens of seconds). The loop is
/// 8 Hz wide with 0.7 damping; the gains are per aux sample, set in `new` from `aux_fs`.
const PLL_BW_HZ: f64 = 8.0;
const PLL_ZETA: f64 = 0.7;
const PLL_PULL_HZ: f64 = 2_000.0;
const SIDE_BW_HZ: f64 = 15_000.0;
/// Decimation to the side chain's working rate.
const AUX_DECIMATE: f64 = 2.0;

/// The WFM stereo side chain: recovers the pilot and the L−R difference signal from the
/// discriminator's composite, without touching the mono path.
///
/// The pilot phasor comes from an NCO mix plus a narrow complex one-pole IIR. Squaring the
/// normalised phasor gives the 38 kHz reference; the coherent product with the composite
/// demodulates the L−R audio. The L−R is de-emphasised and resampled alongside the mono path.
pub struct FmStereo {
    aux_fs: f64,
    decimate: f64,
    decim_phase: f64,
    nco_pilot: f64,
    /// The pilot PLL's frequency integrator (cycles per sample) and the loop gains.
    nco_pll_f: f64,
    pll_kp: f64,
    pll_ki: f64,
    pll_pull: f64,
    pilot_i: f64,
    pilot_q: f64,
    side_i: f64,
    side_q: f64,
    a_pilot: f64,
    a_side: f64,
    pilot_level: f64,
    /// The neighbour-band phasor (21 kHz) and its rotating mix, for the pilot's SNR.
    nbr_i: f64,
    nbr_q: f64,
    nbr_rot_i: f64,
    nbr_rot_q: f64,
    nbr_step_i: f64,
    nbr_step_q: f64,
    pilot_snr_db: f64,
    peak_dev_hz: f64,
    pilot_ratio_cache: f64,
    stereo: bool,
    stereo_count: u32,
    rds: Option<crate::analog::rds::RdsDecoder>,
    warmup: u32,
    /// The pilot averaging coefficient for the current block (from its duration).
    a_pilot_block: f64,
    /// The deviation meter's per-block histogram of |instantaneous frequency| (Hz) and its
    /// scale: one bin per `aux_fs/2 / DEV_BINS`.
    dev_hist: [u32; DEV_BINS],
    dev_count: u32,
    dev_scale: f64,
    /// The smoothed per-block reading the quasi-peak hold works on.
    dev_smooth: f64,
    /// A slow average of the held peak deviation: the ratio's denominator.
    peak_slow: f64,
    /// True once the statistics have been seeded (the first non-warmup block).
    seeded: bool,
    side_deemph: Option<FirState>,
    output_gain: f32,
    resample_step: f64,
    resample_t: f64,
    /// Output delay, in OUTPUT samples, that lines the drained side up with the mono PCM. The
    /// mono passes an audio low-pass the side chain does not, and that filter's group delay is
    /// what the consumer's L = M + S matrix would otherwise rotate out of phase: at 2.5 kHz an
    /// uncompensated 64-sample delay at 300 kSps is nearly an inversion. The delay is fractional,
    /// so the line interpolates.
    align_out: f64,
    /// The last block's decimated sample count, for the bench's stereo diagnostics.
    last_aux: u32,
    /// Ring of the most recent output samples, for `align_out`.
    side_ring: Vec<f32>,
    side_ring_pos: usize,
    side_delayed: Vec<f32>,
    resample_prev: Option<f64>,
    /// Resampler scratch (the previous block's last sample plus this block's side signal).
    resample_buf: Vec<f64>,
    side_aux: Vec<f64>,
    side_out: Vec<f32>,
    /// Per-block scratch: the decimated aux samples and the de-emphasised side. Fields, not
    /// locals, so a 20 ms block never allocates.
    aux: Vec<f64>,
    shaped: Vec<f64>,
}

impl FmStereo {
    /// `align_baseband_samples` is the mono path's audio-low-pass group delay, in samples at
    /// `fs`. The side chain takes the discriminator output before that filter, so it runs ahead
    /// of the mono by exactly this much; the resampler's phase starts where the mono is.
    pub fn new(fs: f64, out_rate: f64, align_baseband_samples: f64) -> Option<Self> {
        let aux_fs = fs / AUX_DECIMATE;
        if aux_fs / 2.0 < SUBCARRIER_HZ + 16_000.0 {
            return None;
        }
        // The mono path's audio low-pass sits after the detector and delays the PCM by its group
        // delay; the side chain takes the detector output, so it runs ahead by exactly that much.
        let align_out = (align_baseband_samples * out_rate / fs).max(0.0);
        let align_ring = align_out.floor() as usize + 2;
        let iir = |bw: f64| 1.0 - (-core::f64::consts::TAU * bw / aux_fs).exp();
        // The side chain reads the detector's output, which trails the baseband by the band
        // filter's group delay (BAND_TAPS/2 samples). Retard both NCOs by that much so their
        // phases line up with the signal they multiply: the RDS mix lands on the real axis and
        // the L−R product keeps L and R on their own sides.
        let delay = super::BAND_TAPS as f64 / 2.0;
        let wrap = |phase: f64| phase - phase.floor();
        Some(Self {
            aux_fs,
            decimate: AUX_DECIMATE,
            decim_phase: 0.0,
            nco_pilot: wrap(-delay * PILOT_HZ / fs),
            nco_pll_f: 0.0,
            pll_kp: 2.0 * PLL_ZETA * PLL_BW_HZ / (fs / AUX_DECIMATE),
            pll_ki: PLL_BW_HZ / (fs / AUX_DECIMATE).powi(2),
            pll_pull: PLL_PULL_HZ / (fs / AUX_DECIMATE),
            nbr_i: 0.0,
            nbr_q: 0.0,
            nbr_rot_i: 1.0,
            nbr_rot_q: 0.0,
            nbr_step_i: (core::f64::consts::TAU * (PILOT_NBR_HZ - PILOT_HZ) / aux_fs).cos(),
            nbr_step_q: (core::f64::consts::TAU * (PILOT_NBR_HZ - PILOT_HZ) / aux_fs).sin(),
            pilot_snr_db: -120.0,
            pilot_i: 0.0,
            pilot_q: 0.0,
            side_i: 0.0,
            side_q: 0.0,
            a_pilot: iir(PILOT_BW_HZ),
            a_side: iir(SIDE_BW_HZ),
            pilot_level: 0.0,
            peak_dev_hz: 0.0,
            pilot_ratio_cache: 0.0,
            stereo: false,
            stereo_count: 0,
            rds: Some(crate::analog::rds::RdsDecoder::new(fs / AUX_DECIMATE)),
            warmup: WARMUP_BATCHES,
            a_pilot_block: 0.0,
            dev_hist: [0; DEV_BINS],
            dev_count: 0,
            dev_scale: DEV_BINS as f64 / (aux_fs * 0.5),
            dev_smooth: 0.0,
            peak_slow: 0.0,
            seeded: false,
            side_deemph: Some(FirState::new(design_lowpass(aux_fs, 15_000.0, 64))),
            output_gain: 1.0,
            resample_step: aux_fs / out_rate,
            resample_t: 0.0,
            align_out,
            last_aux: 0,
            side_ring: vec![0.0; align_ring],
            side_ring_pos: 0,
            side_delayed: Vec::new(),
            resample_prev: None,
            resample_buf: Vec::new(),
            side_aux: Vec::new(),
            side_out: Vec::new(),
            aux: Vec::new(),
            shaped: Vec::new(),
        })
    }

    /// The block's deviation reading: the highest bin whose tail holds no more than
    /// `DEV_TAIL` of the block's samples.
    fn dev_peak(&self) -> f64 {
        if self.dev_count == 0 {
            return 0.0;
        }
        let target = ((self.dev_count as f64 * DEV_TAIL).ceil() as u32).max(1);
        let mut seen = 0_u32;
        for bin in (0..DEV_BINS).rev() {
            seen += self.dev_hist[bin];
            if seen >= target {
                return (bin + 1) as f64 / self.dev_scale;
            }
        }
        0.0
    }

    pub fn capable(&self) -> bool {
        self.aux_fs / 2.0 >= SUBCARRIER_HZ + 16_000.0
    }

    pub fn stereo(&self) -> bool {
        self.stereo
    }

    pub fn pilot_ratio(&self) -> f64 {
        self.pilot_ratio_cache
    }

    /// The pilot's narrow-band SNR in dB (its band over the neighbouring band).
    pub fn pilot_snr_db(&self) -> f64 {
        self.pilot_snr_db
    }

    /// The pilot's own deviation in Hz (twice the smoothed phasor magnitude). This is a station
    /// constant — it does not move with the programme — which is why the pane shows it instead of
    /// the ratio.
    pub fn pilot_deviation_hz(&self) -> f64 {
        2.0 * self.pilot_level
    }

    pub fn peak_deviation_hz(&self) -> f64 {
        self.peak_dev_hz
    }

    pub fn aux_rate(&self) -> f64 {
        self.aux_fs
    }

    /// Bench diagnostics beyond the pane's own numbers: the pilot phasor's raw components, the
    /// side chain's working rate and the last block's decimated length. A pane that reads MONO
    /// cannot say whether the pilot is absent, the chain is starved or its rate is wrong; these
    /// four do. They are not a product feature: no window draws them.
    pub fn diag(&self) -> [f64; 4] {
        [self.pilot_i, self.pilot_q, self.aux_fs, f64::from(self.last_aux)]
    }

    /// RDS bench diagnostics: the carrier reference's magnitude, the two axes' smoothed
    /// energies and the cumulative bit count (its slope is the bit clock the decoder actually
    /// ran at, next to the 1187.5 Hz the format asks for).
    pub fn rds_diag(&self) -> [f64; 4] {
        self.rds.as_ref().map(|r| r.diag()).unwrap_or([0.0; 4])
    }

    /// The PLL's frequency integrator in Hz (bench diagnostics).
    pub fn pll_freq_error_hz(&self) -> f64 {
        self.nco_pll_f * self.aux_fs
    }


    pub fn retune(&mut self) {
        self.pilot_i = 0.0;
        self.pilot_q = 0.0;
        self.nco_pll_f = 0.0;
        self.nbr_i = 0.0;
        self.nbr_q = 0.0;
        self.pilot_snr_db = -120.0;
        self.side_i = 0.0;
        self.side_q = 0.0;
        self.pilot_level = 0.0;
        self.peak_dev_hz = 0.0;
        self.stereo = false;
        self.stereo_count = 0;
        if let Some(rds) = self.rds.as_mut() {
            rds.reset();
        }
        if let Some(d) = self.side_deemph.as_mut() { d.reset(); }
        self.resample_t = 0.0;
        for v in self.side_ring.iter_mut() { *v = 0.0; }
        self.side_ring_pos = 0;
        self.resample_prev = None;
        self.side_aux.clear();
        self.side_out.clear();
        // The detection state belongs to the station that was tuned. The deviation histogram, its
        // smoothed peak and the warmup counter are all measured from that signal, and they feed the
        // pilot ratio the badge thresholds on. Left behind, they carry the old station's peak
        // deviation into the new one: a tune from a quiet station to a loud one reads a pilot ratio
        // far above the acquire threshold (a false STEREO), and the reverse reads below it (a stereo
        // station stuck on MONO, which is what a bench report showed after switching frequencies).
        self.warmup = WARMUP_BATCHES;
        self.dev_hist = [0; DEV_BINS];
        self.dev_count = 0;
        self.dev_smooth = 0.0;
        self.peak_slow = 0.0;
        self.seeded = false;
        self.pilot_ratio_cache = 0.0;
        self.a_pilot_block = 0.0;
    }

    /// Feed one block of discriminator output.
    ///
    /// Every buffer here holds ONE block: `aux`, `side_aux` and `shaped` are scratch fields that
    /// this method clears first. Accumulating across blocks would make the de-emphasis FIR and the
    /// resampler walk the whole history again on every block — quadratic, and it starves the audio
    /// and the message loop within seconds on a real device.
    pub fn process(&mut self, det: &[f32]) {
        if !self.capable() || det.is_empty() { return; }
        let n = det.len();
        let mut aux = core::mem::take(&mut self.aux);
        aux.clear();
        let mut pos = self.decim_phase;
        while (pos as usize) < n {
            let i0 = (pos as usize).min(n - 1);
            let frac = pos - i0 as f64;
            aux.push(det[i0] as f64 * (1.0 - frac) + det[(i0 + 1).min(n - 1)] as f64 * frac);
            pos += self.decimate;
        }
        self.decim_phase = pos - n as f64;
        self.last_aux = aux.len() as u32;
        if aux.is_empty() {
            self.aux = aux;
            return;
        }
        let m = aux.len();

        // Composite RMS (the detect ratio's denominator) and the peak deviation. Both skip the
        // warmup batch: the band filter's startup transient would seed them on garbage.
        //
        // The two are smoothed differently on purpose, the way a broadcast monitor does it:
        //
        // * the deviation uses a quasi-peak detector — instant attack, slow decay (about 1.5 s) —
        //   because the number on the dial is the *peak* the station occupies, and a plain average
        //   of block peaks both under-reads it and jumps with the programme material;
        // * the composite RMS and the pilot amplitude use long averages (0.5 s and 1 s), because
        //   the badge compares two stationary quantities and a per-block ratio flickers.
        // The warmup block's statistics are discarded at the END of the block (see below), not
        // here: the peak of the current block is only known once its samples have run.
        let warmup_block = self.warmup > 0;
        let samples = self.dev_count;
        let peak = self.dev_peak();
        self.dev_hist = [0; DEV_BINS];
        self.dev_count = 0;
        if warmup_block {
            self.warmup -= 1;
        } else {
            // The block's duration in the aux domain (the rate the side chain runs at).
            let block_s = m as f64 / self.aux_fs;
            let a_pilot = 1.0 - (-block_s / PILOT_TAU_S).exp();
            let decay = (-block_s / PEAK_DECAY_S).exp();
            // The discriminator output IS the instantaneous frequency in Hz. An empty histogram
            // (the first block after the warmup) holds the previous reading instead of dropping it.
            if samples > 0 {
                // A light average on the block reading first (about 0.3 s): the percentile of one
                // short block still swings by a kilohertz or two with the programme material, and
                // a broadcast deviation readout is expected to sit still. The quasi-peak hold
                // then takes the fast attack the standard asks for.
                let a_dev = 1.0 - (-block_s / DEV_SMOOTH_TAU_S).exp();
                self.dev_smooth = if self.dev_smooth == 0.0 {
                    peak
                } else {
                    self.dev_smooth + a_dev * (peak - self.dev_smooth)
                };
                self.peak_dev_hz = if self.dev_smooth > self.peak_dev_hz {
                    self.dev_smooth
                } else {
                    self.peak_dev_hz * decay
                };
                // A second, slower average of the held peak for the ratio's denominator: the hold
                // follows a loud passage within a block, which would still move the ratio.
                let a_peak = 1.0 - (-block_s / PEAK_TAU_S).exp();
                self.peak_slow = if self.peak_slow == 0.0 {
                    self.peak_dev_hz
                } else {
                    self.peak_slow + a_peak * (self.peak_dev_hz - self.peak_slow)
                };
            }
            self.a_pilot_block = a_pilot;
        }

        // Per-sample: pilot NCO + IIR, coherent side mix.
        let two_pi = core::f64::consts::TAU;
        let d_pilot = PILOT_HZ / self.aux_fs;
        self.side_aux.clear();
        for &v in aux.iter() {
            // Pilot NCO + complex one-pole IIR: the phasor tracks the pilot's slow drift.
            let th = two_pi * self.nco_pilot;
            let (xr, xi) = (v * th.cos(), -v * th.sin());
            self.pilot_i += self.a_pilot * (xr - self.pilot_i);
            self.pilot_q += self.a_pilot * (xi - self.pilot_q);
            // The same mix, rotated to the neighbour band: a complex multiply by a unit vector
            // that advances by (nbr - pilot) per sample, which costs no trigonometry.
            let (ri, rq) = (self.nbr_rot_i, self.nbr_rot_q);
            let (nxr, nxi) = (xr * ri - xi * rq, xr * rq + xi * ri);
            self.nbr_i += self.a_pilot * (nxr - self.nbr_i);
            self.nbr_q += self.a_pilot * (nxi - self.nbr_q);
            let (si, sq) = (self.nbr_step_i, self.nbr_step_q);
            self.nbr_rot_i = ri * si - rq * sq;
            self.nbr_rot_q = ri * sq + rq * si;
            // The RDS decoder keys on the same pilot phase (its carrier is the third harmonic,
            // its bit clock the pilot over sixteen), so it steps before the phase advances.
            if let Some(rds) = self.rds.as_mut() {
                rds.step(v as f64, self.nco_pilot);
            }
            // The pilot PLL: steer the NCO's frequency by the phasor's angle, so the NCO tracks
            // the received pilot (the side chain's own clock reference) instead of running at
            // the nominal 19 kHz against it. Without a pilot the loop idles: the integrator
            // leaks home, so the next station starts from the nominal rate.
            let p_mag2 = self.pilot_i * self.pilot_i + self.pilot_q * self.pilot_q;
            if p_mag2 > 100.0 {
                let err = self.pilot_q.atan2(self.pilot_i) * core::f64::consts::FRAC_1_PI * 0.5;
                self.nco_pll_f =
                    (self.nco_pll_f + self.pll_ki * err).clamp(-self.pll_pull, self.pll_pull);
                self.nco_pilot += self.pll_kp * err + self.nco_pll_f;
            } else {
                self.nco_pll_f *= 0.9999;
            }
            self.nco_pilot += d_pilot;
            if self.nco_pilot >= 1.0 { self.nco_pilot -= 1.0; }

            // Side: the classic product detector, locked to the RECOVERED pilot. The subcarrier is
            // the pilot's second harmonic, so its reference is 2 * theta_pilot, and theta_pilot is
            // the NCO's phase plus the error the phasor tracks. A free-running 38 kHz reference
            // instead left the drained side at cos(phase error) of it, and that error is whatever
            // the whole signal path's delay makes it: measured on the bench, the same composite
            // read 0.37 of the audio through the device's channel filter where a synthetic read
            // 0.96, so a real broadcast's separation came and went with the backend's filter.
            let pm = (self.pilot_i * self.pilot_i + self.pilot_q * self.pilot_q).sqrt();
            let (pc, ps) = if pm > 1.0e-9 {
                let (ci, si) = (th.cos(), th.sin());
                let (ei, eq) = (self.pilot_i / pm, self.pilot_q / pm);
                (ci * ei - si * eq, si * ei + ci * eq)
            } else {
                // No pilot to lock to (a mono station, or the moment before the phasor settles).
                // The side is discarded downstream in that state; keep the reference sane.
                (th.cos(), th.sin())
            };
            // EN 50067: the pilot is a sine and the subcarrier a cosine, so the subcarrier is the
            // NEGATIVE second harmonic of the cosine phase the phasor tracks.
            let reference = ps * ps - pc * pc;
            // The factor of two is the coherent DSB detector's own gain, not a fudge: the
            // composite carries (L−R)/2 on the subcarrier, and mixing it with a unit-amplitude
            // reference leaves half of that at baseband. Without it the drained side was half
            // the difference the consumer's matrix (L = M + S, R = M − S) expects, so every
            // stereo programme came out with half its separation — the two traces in the decode
            // window looked nearly alike, which is what this factor pins.
            let mixed = 2.0 * v * reference;
            self.side_i += self.a_side * (mixed - self.side_i);
            self.side_aux.push(self.side_i);
            // Deviation meter: a histogram of |instantaneous frequency| over the block. The
            // block's reading is its 99.9th percentile, not its maximum: a discriminator wrap —
            // a handful of samples where the signal touches the sample rate's limit — is not a
            // deviation reading, and a maximum absorbs it and pins the meter. Smoothing the
            // magnitude instead cannot work: the composite's |instantaneous frequency| carries
            // structure up to the 19 kHz pilot, so any filter slow enough to reject a one-sample
            // wrap also reads the mean of the audio envelope (half the true peak).
            let mag = v.abs();
            let bin = (mag * self.dev_scale) as usize;
            self.dev_hist[bin.min(DEV_BINS - 1)] += 1;
            self.dev_count += 1;
        }

        // Pilot level: batch mean |phasor| for the detect numerator (Goertzel-equivalent).
        let pmag = (self.pilot_i * self.pilot_i + self.pilot_q * self.pilot_q).sqrt();
        if self.warmup > 0 {
            // The warmup batch carries the band filter's startup transient; the IIRs ran, the
            // detection state stays untouched. The next batch seeds the EMAs cleanly.
        } else {
        self.pilot_level = if self.pilot_level == 0.0 { pmag } else { self.pilot_level + self.a_pilot_block * (pmag - self.pilot_level) };
        // Cache the ratio for the stereo_info readout; the detection below compares against it.
        // EN 50067 gives the pilot as a fraction of the deviation (6.75 kHz of 75 kHz), and that
        // fraction is a property of the STATION, not of its programme: measured against the peak
        // deviation it stays put while the audio comes and goes. The earlier ratio (pilot over the
        // composite RMS) moved with the material by a factor of two or more.
        let peak = self.peak_slow;
        let ratio = if peak > 1.0e-9 { (2.0 * self.pilot_level) / peak } else { 0.0 };
        self.pilot_ratio_cache = ratio;

        // The pilot's SNR, updated on the same block cadence as the ratio.
        let block_s = m as f64 / self.aux_fs;
        let p_mag = self.pilot_level;
        let n_mag = (self.nbr_i * self.nbr_i + self.nbr_q * self.nbr_q).sqrt();
        let snr = if n_mag > 1.0e-9 { 20.0 * (p_mag / n_mag).log10() } else { 60.0 };
        let a_snr = 1.0 - (-block_s / PILOT_TAU_S).exp();
        self.pilot_snr_db = if self.pilot_snr_db <= -119.0 {
            snr
        } else {
            self.pilot_snr_db + a_snr * (snr - self.pilot_snr_db)
        };

        // Stereo hysteresis with sustained-pilot requirement. The pilot must both carry the
        // standard's share of the deviation and stand out of the noise beside it: on an empty
        // frequency the first test alone passes (measured 0.039 against a 0.030 threshold).
        if ratio > STEREO_ACQUIRE && self.pilot_snr_db > PILOT_SNR_DB {
            self.stereo_count = (self.stereo_count + 1).min(40);
            if self.stereo_count >= STEREO_COUNT { self.stereo = true; }
        } else if ratio > STEREO_RELEASE && self.pilot_snr_db > PILOT_SNR_RELEASE_DB && self.stereo {
            self.stereo_count = 0;
        } else {
            self.stereo_count = 0;
            self.stereo = false;
        }
        }

        if warmup_block {
            // The startup transient's wraps live in this block's histogram. It is cleared here, at
            // the END of the block, because the reading is taken from the histogram at the START of
            // the next one: clearing it earlier (with the read) would leave the transient for that
            // next block to consume, which is how the meter came up pinned at the wrap value.
            self.dev_hist = [0; DEV_BINS];
            self.dev_count = 0;
        }

        // L−R: de-emphasis (also the audio LPF), then resample to the consumer's rate.
        let mut shaped = core::mem::take(&mut self.shaped);
        shaped.clear();
        if let Some(d) = self.side_deemph.as_mut() {
            d.process_into(&self.side_aux, &mut shaped);
        }
        self.resample_into(&shaped);
        self.align_side();
        self.shaped = shaped;
        self.aux = aux;
    }

    /// The decoded RDS state, when the aux band can carry the 57 kHz subcarrier.
    pub fn rds_state(&self) -> Option<crate::analog::rds::RdsState> {
        self.rds.as_ref().map(|r| r.state.clone())
    }



    fn resample_into(&mut self, x: &[f64]) {
        if x.is_empty() { return; }
        let mut buf = core::mem::take(&mut self.resample_buf);
        buf.clear();
        if let Some(p) = self.resample_prev { buf.push(p); }
        buf.extend_from_slice(x);
        let n = buf.len();
        if n >= 2 {
            let count = (((n as f64 - 1.0 - self.resample_t) / self.resample_step).floor() + 1.0).max(0.0) as usize;
            for k in 0..count {
                let pos = self.resample_t + k as f64 * self.resample_step;
                let i0 = ((pos.trunc() as i64).clamp(0, n as i64 - 2)) as usize;
                let frac = (pos - i0 as f64).clamp(0.0, 1.0);
                self.side_out.push((buf[i0] * (1.0 - frac) + buf[i1_safe(i0, n)] * frac) as f32);
            }
            if count > 0 { self.resample_t += count as f64 * self.resample_step; }
        }
        self.resample_t -= n as f64 - 1.0;
        self.resample_prev = buf.last().copied();
        self.resample_buf = buf;
    }

    /// Delay the freshly resampled side by `align_out` samples, so the drained L−R carries the
    /// same instant as the mono PCM the consumer pairs it with. A fractional delay interpolates
    /// between the two surrounding ring entries.
    fn align_side(&mut self) {
        let delay = self.align_out;
        if delay <= 0.0 || self.side_out.is_empty() {
            return;
        }
        let whole = delay.floor() as usize;
        let frac = delay - whole as f64;
        let ring_len = self.side_ring.len();
        if ring_len < whole + 2 {
            return;
        }
        let mut out = core::mem::take(&mut self.side_delayed);
        out.clear();
        out.reserve(self.side_out.len());
        let mut pos = self.side_ring_pos;
        for &v in self.side_out.iter() {
            self.side_ring[pos] = v;
            let a = self.side_ring[(pos + ring_len - whole) % ring_len];
            let b = self.side_ring[(pos + ring_len - whole - 1) % ring_len];
            out.push((a as f64 * (1.0 - frac) + b as f64 * frac) as f32);
            pos = (pos + 1) % ring_len;
        }
        self.side_ring_pos = pos;
        core::mem::swap(&mut self.side_out, &mut out);
        self.side_delayed = out;
    }

    pub fn take_side(&mut self) -> Vec<f32> {
        let g = self.output_gain;
        for v in self.side_out.iter_mut() {
            *v *= g;
        }
        core::mem::take(&mut self.side_out)
    }

    pub fn set_output_gain(&mut self, gain: f32) {
        if gain.is_finite() && gain > 0.0 { self.output_gain = gain; }
    }

    pub fn set_deemph_taps(&mut self, taps: Vec<f64>) {
        self.side_deemph = if taps.is_empty() { None } else { Some(FirState::new(taps)) };
    }
}

fn i1_safe(i0: usize, n: usize) -> usize { (i0 + 1).min(n - 1) }
