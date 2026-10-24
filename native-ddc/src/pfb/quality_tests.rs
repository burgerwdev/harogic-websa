use super::Pfb;
use rustfft::num_complex::Complex32;
use std::f64::consts::PI;

#[test]
fn multibin_quality_matches_direct_fir_on_one_packetized_iq() {
    let fs = 62_500_000.0;
    let selected = quality_bins(Pfb::new(fs, 2).unwrap().m);
    let lengths = [777, 1623, 941];
    let total: usize = lengths.iter().sum();
    let geometry = Pfb::new(fs, 2).unwrap();
    let spacing = fs / geometry.m as f64;
    // Identical input bytes and total amplitude for every route count.
    let iq: Vec<i16> = (0..total)
        .flat_map(|n| {
            let t = n as f64 / fs;
            let z: Complex32 = selected
                .iter()
                .enumerate()
                .map(|(i, &bin)| {
                    let signed = if bin as usize > geometry.m / 2 {
                        bin as i32 - geometry.m as i32
                    } else {
                        bin as i32
                    };
                    let phase = 2.0 * PI * (signed as f64 * spacing + (i % 3) as f64 * 1000.0) * t;
                    Complex32::new(phase.cos() as f32, phase.sin() as f32) * 400.0
                })
                .sum();
            [z.re as i16, z.im as i16]
        })
        .collect();
    for (taps, count) in [16, 8, 4].into_iter().flat_map(|taps| {
        [1, 4, 8, 16, 32].into_iter().map(move |count| (taps, count))
    }) {
        let bins = &selected[..count];
        let mut p = Pfb::with_taps(fs, 2, taps).unwrap();
        let mut outputs = Vec::new();
        let mut at = 0;
        for n in lengths {
            let mut out = vec![0.0; 2 * bins.len() * (n / p.hop + 2)];
            let groups = p.process(&iq[2 * at..2 * (at + n)], bins, &mut out);
            outputs.extend_from_slice(&out[..2 * groups * bins.len()]);
            at += n;
        }
        for (group, row) in outputs.chunks_exact(2 * bins.len()).enumerate() {
            let t = (group + 1) * p.hop - 1;
            for (col, &bin) in bins.iter().enumerate() {
                // Independent time-domain FIR + complex mixer, not FFT/polyphase indexing.
                let mut direct = Complex32::new(0.0, 0.0);
                for lag in 0..p.taps.len().min(t + 1) {
                    let n = t - lag;
                    let phase = -2.0 * PI * bin as f64 * n as f64 / p.m as f64;
                    let rot = Complex32::new(phase.cos() as f32, phase.sin() as f32);
                    direct +=
                        Complex32::new(iq[2 * n] as f32, iq[2 * n + 1] as f32) * rot * p.taps[lag];
                }
                let actual = Complex32::new(row[2 * col], row[2 * col + 1]);
                assert!(
                    (actual - direct).norm() < 0.2,
                    "count={count} bin={bin} group={group} actual={actual:?} direct={direct:?}"
                );
            }
        }
    }
}

#[test]
fn bin_edges_alias_and_impulse_delay() {
    let fs = 62_500_000.0;
    let p = Pfb::new(fs, 2).unwrap();
    let spacing = fs / p.m as f64;
    let bin = 127;
    let measure = |offset: f64| {
        let mut channelizer = Pfb::new(fs, 2).unwrap();
        let f = -spacing + offset;
        let mut power = 0.0f64;
        let mut count = 0;
        for block in 0..6 {
            let iq: Vec<i16> = (0..16240)
                .flat_map(|j| {
                    let ph = 2.0 * PI * f * (block * 16240 + j) as f64 / fs;
                    [(12000.0 * ph.cos()) as i16, (12000.0 * ph.sin()) as i16]
                })
                .collect();
            let mut out = vec![0.0; 2 * (16240 / channelizer.hop + 2)];
            let groups = channelizer.process(&iq, &[bin], &mut out);
            if block > 0 {
                for z in out[..2 * groups].chunks_exact(2) {
                    power += (z[0] * z[0] + z[1] * z[1]) as f64;
                    count += 1;
                }
            }
        }
        (power / count as f64).sqrt()
    };
    let center = measure(0.0);
    let edge = measure(0.45 * spacing);
    let adjacent = measure(1.8 * spacing);
    assert!(
        edge / center > 0.9,
        "edge: {} dB",
        20.0 * (edge / center).log10()
    );
    assert!(
        20.0 * (adjacent / center).log10() < -40.0,
        "alias: {} dB",
        20.0 * (adjacent / center).log10()
    );

    let impulse_at = 1024;
    let mut iq = vec![0i16; 2 * 4096];
    iq[2 * impulse_at] = 12000;
    let mut channelizer = Pfb::new(fs, 2).unwrap();
    let mut out = vec![0.0; 2 * (4096 / p.hop + 1)];
    let groups = channelizer.process(&iq, &[bin], &mut out);
    let peak = out[..2 * groups]
        .chunks_exact(2)
        .enumerate()
        .max_by(|(_, a), (_, b)| a[0].hypot(a[1]).total_cmp(&b[0].hypot(b[1])))
        .unwrap()
        .0;
    let expected = (impulse_at as f64 + (p.taps.len() - 1) as f64 / 2.0) / p.hop as f64 - 1.0;
    assert!(
        (peak as f64 - expected).abs() <= 1.0,
        "peak={peak}, expected={expected}"
    );
}

const QUALITY_FS: f64 = 62_500_000.0;
const QUALITY_SAMPLES: usize = 262_144;
const QUALITY_TONES: [f64; 7] = [-0.5, -0.36, -0.17, 0.0, 0.11, 0.33, 0.5];
const QUALITY_PEAK: f64 = 28_000.0;
const BLOCKER_RATIO: f64 = 10.0;
const PACKETS: [&[usize]; 2] = [&[777, 1623, 941, 64, 4093], &[8191, 65, 513]];
type C64 = rustfft::num_complex::Complex64;

fn phasor(phase: f64) -> C64 {
    C64::new(phase.cos(), phase.sin())
}

fn quality_bins(m: usize) -> Vec<u32> {
    (0..16)
        .flat_map(|i| [2 + 4 * i, m - 2 - 4 * i])
        .map(|bin| bin as u32)
        .collect()
}

fn tone_phase(route: usize, tone: usize) -> f64 {
    0.73 * route as f64 + 0.41 * (tone * tone) as f64 + 0.19 * (route * tone) as f64
}

fn wanted_amplitude() -> f64 {
    QUALITY_PEAK / (1.0 + BLOCKER_RATIO) / 32.0 / QUALITY_TONES.len() as f64
}

struct QualityScene {
    bandwidth: f64,
    decimate: usize,
    offset: f64,
    blocker_separation: f64,
}

impl QualityScene {
    fn ideal(&self, route: usize, time: f64) -> C64 {
        QUALITY_TONES
            .iter()
            .enumerate()
            .map(|(tone, frequency)| {
                phasor(2.0 * PI * frequency * self.bandwidth * time + tone_phase(route, tone))
                    * wanted_amplitude()
            })
            .sum()
    }

    fn input(&self, m: usize, bins: &[u32]) -> [Vec<i16>; 3] {
        let comb = |phase_offset: usize| -> Vec<Vec<C64>> {
            (0..QUALITY_TONES.len())
                .map(|tone| {
                    (0..m)
                        .map(|n| {
                            bins.iter()
                                .enumerate()
                                .map(|(route, &bin)| {
                                    phasor(
                                        2.0 * PI * bin as f64 * n as f64 / m as f64
                                            + tone_phase(route, tone + phase_offset),
                                    )
                                })
                                .sum()
                        })
                        .collect()
                })
                .collect()
        };
        let wanted_comb = comb(0);
        let blocker_comb = comb(13);
        let steps: Vec<_> = QUALITY_TONES
            .iter()
            .map(|frequency| {
                let f = self.offset + frequency * self.bandwidth;
                [
                    phasor(2.0 * PI * f / QUALITY_FS),
                    phasor(2.0 * PI * (f + self.blocker_separation) / QUALITY_FS),
                ]
            })
            .collect();
        let mut phases = vec![[C64::new(1.0, 0.0); 2]; QUALITY_TONES.len()];
        let mut inputs = std::array::from_fn(|_| Vec::with_capacity(2 * QUALITY_SAMPLES));
        for n in 0..QUALITY_SAMPLES {
            let mut wanted = C64::new(0.0, 0.0);
            let mut blocker = C64::new(0.0, 0.0);
            for tone in 0..QUALITY_TONES.len() {
                wanted += wanted_comb[tone][n % m] * phases[tone][0];
                blocker += blocker_comb[tone][n % m] * phases[tone][1];
                phases[tone][0] *= steps[tone][0];
                phases[tone][1] *= steps[tone][1];
            }
            wanted *= wanted_amplitude();
            blocker *= wanted_amplitude() * BLOCKER_RATIO;
            for (input, z) in inputs.iter_mut().zip([wanted, blocker, wanted + blocker]) {
                assert!(z.re.abs().max(z.im.abs()) < 30_000.0, "quality input clips");
                input.extend([z.re.round() as i16, z.im.round() as i16]);
            }
        }
        inputs
    }
}

struct QualityChannel(*mut crate::narrow::Channel);

impl Drop for QualityChannel {
    fn drop(&mut self) {
        unsafe { crate::narrow::channel_close(self.0) };
    }
}

struct QualityOutput {
    channels: Vec<Vec<C64>>,
    first_steady: usize,
    delay_input_samples: f64,
    output_stride: usize,
}

fn quality_chain(
    taps: usize,
    bins: &[u32],
    scene: &QualityScene,
    input: &[i16],
    packets: &[usize],
) -> QualityOutput {
    let mut pfb = Pfb::with_taps(QUALITY_FS, 2, taps).unwrap();
    pfb.fir_kernel = u8::from(pfb.avx2);
    let fine: Vec<_> = bins
        .iter()
        .map(|_| {
            let channel = crate::narrow::channel_open_coarse(
                QUALITY_FS / pfb.hop as f64,
                -scene.offset,
                scene.decimate as i32,
            );
            assert!(!channel.is_null(), "unsupported fine quality geometry");
            QualityChannel(channel)
        })
        .collect();
    let fine_delay = unsafe { crate::narrow::channel_delay(fine[0].0) };
    let stride = pfb.hop * scene.decimate;
    let delay = (pfb.taps.len() - 1) as f64 / 2.0 + fine_delay * stride as f64;
    let mut channels = vec![Vec::new(); bins.len()];
    let largest = *packets.iter().max().unwrap();
    let coarse_capacity = largest / pfb.hop + 2;
    let fine_capacity = coarse_capacity / scene.decimate + 2;
    let mut coarse = vec![0.0; 2 * coarse_capacity * bins.len()];
    let mut selected = vec![0.0; 2 * coarse_capacity];
    let mut output = vec![0.0; 2 * fine_capacity];
    let mut at = 0;
    let mut packet = 0;
    while at < input.len() / 2 {
        let end = (at + packets[packet % packets.len()]).min(input.len() / 2);
        let groups = pfb.process(&input[2 * at..2 * end], bins, &mut coarse);
        for route in 0..bins.len() {
            for group in 0..groups {
                let index = 2 * (group * bins.len() + route);
                selected[2 * group..2 * group + 2].copy_from_slice(&coarse[index..index + 2]);
            }
            let produced = unsafe {
                crate::narrow::channel_process_float(
                    fine[route].0,
                    selected.as_ptr(),
                    groups as i32,
                    output.as_mut_ptr(),
                    fine_capacity as i32,
                )
            };
            assert!(produced >= 0, "fine processing rejects a valid packet");
            channels[route].extend(
                output[..2 * produced as usize]
                    .chunks_exact(2)
                    .map(|z| C64::new(z[0] as f64, z[1] as f64)),
            );
        }
        at = end;
        packet += 1;
    }
    QualityOutput {
        channels,
        first_steady: ((2.0 * delay + 1.0) / stride as f64).ceil() as usize,
        delay_input_samples: delay,
        output_stride: stride,
    }
}

struct QualityMetrics {
    evm: f64,
    evm_unity_gain: f64,
    gain_db: f64,
    power: f64,
}

fn quality_metrics(output: &QualityOutput, scene: &QualityScene, route: usize) -> QualityMetrics {
    let actual = &output.channels[route][output.first_steady..];
    assert!(actual.len() >= 128, "insufficient steady-state measurement interval");
    let reference: Vec<_> = (output.first_steady..output.channels[route].len())
        .map(|j| {
            let time = ((j + 1) as f64 * output.output_stride as f64
                - 1.0
                - output.delay_input_samples)
                / QUALITY_FS;
            scene.ideal(route, time)
        })
        .collect();
    let reference_power: f64 = reference.iter().map(|z| z.norm_sqr()).sum();
    let gain: C64 = actual
        .iter()
        .zip(&reference)
        .map(|(y, x)| *y * x.conj())
        .sum::<C64>()
        / reference_power;
    assert!(gain.norm() > 1e-9 && gain.norm().is_finite(), "wanted channel disappears");
    let phase = gain / gain.norm();
    let squared_error: f64 = actual
        .iter()
        .zip(&reference)
        .map(|(y, x)| (*y - gain * x).norm_sqr())
        .sum();
    let unity_error: f64 = actual
        .iter()
        .zip(&reference)
        .map(|(y, x)| (*y - phase * x).norm_sqr())
        .sum();
    QualityMetrics {
        evm: (squared_error / (gain.norm_sqr() * reference_power)).sqrt(),
        evm_unity_gain: (unity_error / reference_power).sqrt(),
        gain_db: 20.0 * gain.norm().log10(),
        power: actual.iter().map(|z| z.norm_sqr()).sum::<f64>() / actual.len() as f64,
    }
}

fn quality_bessel0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    for order in 1..80 {
        term *= x * x / (4.0 * (order * order) as f64);
        sum += term;
        if term < sum * f64::EPSILON {
            break;
        }
    }
    sum
}

fn quality_ideal_filter(length: usize, cutoff: f64) -> Vec<f64> {
    let mut coefficients: Vec<_> = (0..length)
        .map(|n| {
            let centered = n as f64 - (length - 1) as f64 / 2.0;
            let position = 2.0 * n as f64 / (length - 1) as f64 - 1.0;
            let sinc = if centered == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * PI * cutoff * centered).sin() / (PI * centered)
            };
            sinc * quality_bessel0(8.6 * (1.0 - position * position).max(0.0).sqrt())
        })
        .collect();
    let dc: f64 = coefficients.iter().sum();
    for coefficient in &mut coefficients {
        *coefficient /= dc;
    }
    coefficients
}

fn quality_response_power(coefficients: &[f64], frequency_cycles: f64) -> f64 {
    coefficients
        .iter()
        .enumerate()
        .map(|(n, coefficient)| phasor(-2.0 * PI * frequency_cycles * n as f64) * coefficient)
        .sum::<C64>()
        .norm_sqr()
}

fn print_quality_theory(taps: usize, geometry: &Pfb, scene: &QualityScene) {
    let coarse = quality_ideal_filter(taps * geometry.m, 0.5 / geometry.hop as f64);
    let first = quality_ideal_filter(16, 0.40);
    let second = quality_ideal_filter(32 * scene.decimate, 0.49 / scene.decimate as f64);
    let coarse_fs = QUALITY_FS / geometry.hop as f64;
    let powers = |separation: f64| -> (f64, f64) {
        QUALITY_TONES.iter().fold((0.0, 0.0), |(coarse_sum, total_sum), tone| {
            let baseband = tone * scene.bandwidth + separation;
            let coarse_power = quality_response_power(&coarse, (scene.offset + baseband) / QUALITY_FS);
            let fine_power = quality_response_power(&first, baseband / coarse_fs)
                * quality_response_power(&second, baseband / coarse_fs);
            (coarse_sum + coarse_power, total_sum + coarse_power * fine_power)
        })
    };
    let (coarse_wanted, wanted) = powers(0.0);
    let (coarse_blocker, blocker) = powers(scene.blocker_separation);
    println!("quality_theory,{taps},{:.3},{:.0},{:.3},{:.6},{:.6},{:.6}",
        scene.offset, scene.bandwidth, scene.blocker_separation,
        10.0 * (coarse_wanted / QUALITY_TONES.len() as f64).log10(),
        10.0 * (coarse_wanted / coarse_blocker).log10(),
        10.0 * (wanted / blocker).log10());
}

#[test]
#[ignore = "release-mode CSV quality characterization; run after the FIR kernel stage"]
fn taps_routes_adjacent_and_bin_edge_evm_matrix() {
    let geometry = Pfb::with_taps(QUALITY_FS, 2, 16).unwrap();
    let spacing = QUALITY_FS / geometry.m as f64;
    let bins = quality_bins(geometry.m);
    println!("quality_metadata,fs_hz={QUALITY_FS},oversample=2,active_input_channels=32,blocker_db=20,peak_bound={QUALITY_PEAK},tones=7,alignment=analytic_group_delay,evm_fit=one_constant_complex_gain,unity_evm_fit=phase_only,startup=full_impulse_support,kernel=grouped_if_avx2,attenuation_includes_i16_quantization_floor=true,high_attenuation_is_lower_bound=true");
    println!("quality,taps,count,bin,signed_bin,packet,offset_hz,edge_distance_hz,bw_hz,blocker_separation_hz,steady_samples,evm_pct,clean_evm_pct,unity_gain_evm_pct,gain_db,attenuation_db");
    println!("quality_theory_metadata,coefficients=independent_f64_kaiser_beta8.6,source=one_wanted_and_one_blocker,phases=infinite_time_mean_power,excludes=i16_quantization_and_other_routes_and_float_rounding");
    println!("quality_theory,taps,offset_hz,bw_hz,blocker_separation_hz,coarse_wanted_gain_db,coarse_attenuation_db,cascade_attenuation_db");
    for (bandwidth, decimate, adjacent) in [(12_000.0, 24, 25_000.0), (200_000.0, 4, 250_000.0)] {
        for offset_fraction in [-0.7, -0.51, -0.49, 0.0, 0.45, 0.49, 0.5, 0.51, 0.7] {
            for blocker_separation in [adjacent, spacing, 2.0 * spacing] {
                let scene = QualityScene {
                    bandwidth,
                    decimate,
                    offset: offset_fraction * spacing,
                    blocker_separation,
                };
                let inputs = scene.input(geometry.m, &bins);
                for taps in [16, 8, 4] {
                    print_quality_theory(taps, &geometry, &scene);
                    for (packet, count) in [1, 4, 8, 16, 32].into_iter().enumerate() {
                        let wanted = quality_chain(taps, &bins[..count], &scene, &inputs[0], PACKETS[packet % 2]);
                        let blocker = quality_chain(taps, &bins[..count], &scene, &inputs[1], PACKETS[packet % 2]);
                        let mixed = quality_chain(taps, &bins[..count], &scene, &inputs[2], PACKETS[packet % 2]);
                        for (route, &bin) in bins[..count].iter().enumerate() {
                            let clean = quality_metrics(&wanted, &scene, route);
                            let dirty = quality_metrics(&mixed, &scene, route);
                            let leakage = &blocker.channels[route][blocker.first_steady..];
                            let leakage_power = leakage.iter().map(|z| z.norm_sqr()).sum::<f64>() / leakage.len() as f64;
                            let attenuation = 20.0 * BLOCKER_RATIO.log10() + 10.0 * (clean.power / leakage_power).log10();
                            assert!(dirty.evm.is_finite() && attenuation.is_finite());
                            if offset_fraction == 0.0 {
                                assert!(clean.evm < 0.15 && clean.gain_db > -3.0,
                                    "center channel damaged: taps={taps} count={count} bin={bin} bw={bandwidth} evm={} gain_db={}", clean.evm, clean.gain_db);
                            }
                            let signed = if bin as usize >= geometry.m / 2 { bin as i32 - geometry.m as i32 } else { bin as i32 };
                            println!("quality,{taps},{count},{bin},{signed},{},{:.3},{:.3},{bandwidth:.0},{blocker_separation:.3},{},{:.6},{:.6},{:.6},{:.6},{attenuation:.6}",
                                packet % 2, scene.offset, spacing / 2.0 - scene.offset.abs(), leakage.len(),
                                100.0 * dirty.evm, 100.0 * clean.evm, 100.0 * dirty.evm_unity_gain, dirty.gain_db);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn fine_quality_is_independent_of_packet_boundaries() {
    let geometry = Pfb::with_taps(QUALITY_FS, 2, 16).unwrap();
    let bins = quality_bins(geometry.m);
    let scene = QualityScene {
        bandwidth: 200_000.0,
        decimate: 4,
        offset: 0.51 * QUALITY_FS / geometry.m as f64,
        blocker_separation: QUALITY_FS / geometry.m as f64,
    };
    let inputs = scene.input(geometry.m, &bins);
    for taps in [16, 8, 4] {
        let a = quality_chain(taps, &bins, &scene, &inputs[2], PACKETS[0]);
        let b = quality_chain(taps, &bins, &scene, &inputs[2], PACKETS[1]);
        for (route, (left, right)) in a.channels.iter().zip(&b.channels).enumerate() {
            assert_eq!(left.len(), right.len());
            for (sample, (x, y)) in left.iter().zip(right).enumerate() {
                assert!((*x - *y).norm() < 0.001,
                    "packet-dependent IQ: taps={taps} route={route} sample={sample} left={x} right={y}");
            }
        }
    }
}

fn quality_coarse(
    oversample: usize,
    taps: usize,
    bins: &[u32],
    scene: &QualityScene,
    input: &[i16],
) -> QualityOutput {
    let mut pfb = Pfb::with_taps(QUALITY_FS, oversample, taps).unwrap();
    pfb.fir_kernel = u8::from(pfb.avx2);
    let delay = (pfb.taps.len() - 1) as f64 / 2.0;
    let mut channels = vec![Vec::new(); bins.len()];
    let mut out = vec![0.0; 2 * bins.len() * (8191 / pfb.hop + 2)];
    let mut at = 0;
    let mut packet = 0;
    let mut emitted = 0;
    while at < input.len() / 2 {
        let end = (at + PACKETS[1][packet % PACKETS[1].len()]).min(input.len() / 2);
        let groups = pfb.process(&input[2 * at..2 * end], bins, &mut out);
        for group in 0..groups {
            let time = ((emitted + group + 1) * pfb.hop - 1) as f64 / QUALITY_FS;
            let nco = phasor(-2.0 * PI * scene.offset * time);
            for route in 0..bins.len() {
                let index = 2 * (group * bins.len() + route);
                channels[route].push(C64::new(out[index] as f64, out[index + 1] as f64) * nco);
            }
        }
        emitted += groups;
        at = end;
        packet += 1;
    }
    QualityOutput {
        channels,
        first_steady: ((2.0 * delay + 1.0) / pfb.hop as f64).ceil() as usize,
        delay_input_samples: delay,
        output_stride: pfb.hop,
    }
}

#[test]
#[ignore = "release-mode CSV coarse os1/os2 comparison; requires fixed bin spacing"]
fn critical_sampling_bin_edge_evm_matrix() {
    let geometry = Pfb::with_taps(QUALITY_FS, 2, 16).unwrap();
    let spacing = QUALITY_FS / geometry.m as f64;
    let bins = quality_bins(geometry.m);
    println!("coarse_quality_metadata,reference=analytic_multitone,no_fine_ddc=true,evm_fit=one_constant_complex_gain,unity_evm_fit=phase_only,startup=full_impulse_support");
    println!("coarse_quality,oversample,taps,count,bin,offset_hz,edge_distance_hz,bw_hz,steady_samples,evm_pct,unity_gain_evm_pct,gain_db");
    for bandwidth in [12_000.0, 200_000.0] {
        for offset_fraction in [-0.7, -0.51, -0.49, 0.0, 0.45, 0.49, 0.5, 0.51, 0.7] {
            let scene = QualityScene {
                bandwidth,
                decimate: 1,
                offset: offset_fraction * spacing,
                blocker_separation: spacing,
            };
            let input = scene.input(geometry.m, &bins);
            for oversample in [2, 1] {
                for taps in [16, 8, 4] {
                    let candidate = Pfb::with_taps(QUALITY_FS, oversample, taps).unwrap();
                    assert_eq!(candidate.m, geometry.m, "comparison must preserve bin spacing");
                    assert_eq!(candidate.hop * oversample, candidate.m);
                    let output = quality_coarse(oversample, taps, &bins[..2], &scene, &input[0]);
                    for (route, &bin) in bins[..2].iter().enumerate() {
                        let metric = quality_metrics(&output, &scene, route);
                        assert!(metric.evm.is_finite() && metric.evm_unity_gain.is_finite());
                        println!("coarse_quality,{oversample},{taps},2,{bin},{:.3},{:.3},{bandwidth:.0},{},{:.6},{:.6},{:.6}",
                            scene.offset, spacing / 2.0 - scene.offset.abs(),
                            output.channels[route].len() - output.first_steady,
                            100.0 * metric.evm, 100.0 * metric.evm_unity_gain, metric.gain_db);
                    }
                }
            }
        }
    }
}
