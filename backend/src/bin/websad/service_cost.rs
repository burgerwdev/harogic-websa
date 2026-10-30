use super::*;
use std::time::Instant;
use websa_channelizer::{FineChannel, FineOutput, FineSpec, PfbOutput};
use websa_channelizer::rate_plan::RatePlan;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CostKey {
    mode: String,
    bandwidth: u64,
    pitch: u64,
    input_rate: u64,
    coarse_rate: u64,
    taps: usize,
    input_hop: usize,
    selected_bins: usize,
    groups: usize,
    decimate: i32,
    output_rate: u64,
    occupied_bandwidth: u64,
    transition: u64,
    stopband: u64,
    decoder_rate: Option<u64>,
    auxiliary: bool,
    direct: bool,
}

/// D2 covers the selected planned coarse or raw-input kernel and baseband framing.
/// Receiver workers, PFB processing and transport have separate runtime costs.
pub(super) fn admit_service(
    configs: &[receiver_workers::ChannelConfig],
    fs: f64,
    taps: usize,
    selected_bins: usize,
) -> Result<(), String> {
    check_budget(service_cost_report(configs, fs, taps, selected_bins)?)
}

/// Admission at an active capture boundary must never benchmark a new kernel.
pub(super) fn admit_service_cached(
    configs: &[receiver_workers::ChannelConfig],
    fs: f64,
    taps: usize,
    selected_bins: usize,
) -> Result<(), String> {
    check_budget(service_cost_report_cached(configs, fs, taps, selected_bins)?)
}

fn check_budget(report: Value) -> Result<(), String> {
    let required = report["required_ns_per_second"].as_f64()
        .ok_or("planned fine/framer cost calibration invalid")?;
    if required > sdr_pipeline::FINE_COST_LIMIT_NS {
        return Err(format!(
            "planned fine/framer cost budget exceeded: required {required:.0} ns/s, available {:.0} ns/s (reserve {}; excludes receiver workers, PFB and transport)",
            sdr_pipeline::FINE_COST_LIMIT_NS, sdr_pipeline::FINE_COST_RESERVE));
    }
    Ok(())
}

pub(super) fn service_cost_report(
    configs: &[receiver_workers::ChannelConfig],
    fs: f64,
    taps: usize,
    selected_bins: usize,
) -> Result<Value, String> {
    service_cost_report_inner(configs, fs, taps, selected_bins, true)
}

pub(super) fn service_cost_report_cached(
    configs: &[receiver_workers::ChannelConfig],
    fs: f64,
    taps: usize,
    selected_bins: usize,
) -> Result<Value, String> {
    service_cost_report_inner(configs, fs, taps, selected_bins, false)
}

fn service_cost_report_inner(
    configs: &[receiver_workers::ChannelConfig],
    fs: f64,
    taps: usize,
    selected_bins: usize,
    allow_calibration: bool,
) -> Result<Value, String> {
    const PROFILE_CAPACITY: usize = 64;
    static COSTS: std::sync::LazyLock<PLMutex<std::collections::VecDeque<(CostKey, f64)>>> =
        std::sync::LazyLock::new(|| PLMutex::new(std::collections::VecDeque::with_capacity(PROFILE_CAPACITY)));
    static CALIBRATION: std::sync::LazyLock<PLMutex<()>> =
        std::sync::LazyLock::new(|| PLMutex::new(()));
    if !fs.is_finite() || fs <= 0.0 || taps == 0 {
        return Err("planned fine/framer admission: invalid input geometry".into());
    }
    let bin_count = PFB_BINS.min((fs / BIN_SPACING).round() as i64).max(2) as usize;
    if selected_bins == 0 || selected_bins > bin_count {
        return Err("planned fine/framer admission: invalid selected-bin count".into());
    }
    let input_hop = bin_count / 2;
    let coarse_rate = fs / input_hop as f64;
    let groups = 16240usize.div_ceil(input_hop);
    let mut required = 0.0;
    for config in configs.iter().filter(|config| config.enabled) {
        let source = super::service_ddc::source_plan(fs, config)?;
        let plan = source.plan();
        let source_rate = if source.is_direct() { fs } else { coarse_rate };
        let key = CostKey {
            mode: config.mode.clone(), bandwidth: config.if_bw.to_bits(), pitch: config.pitch.to_bits(),
            input_rate: fs.to_bits(), coarse_rate: coarse_rate.to_bits(), taps, input_hop,
            selected_bins, groups, decimate: plan.decimate, output_rate: plan.output_rate.to_bits(),
            occupied_bandwidth: plan.occupied_bandwidth_hz.to_bits(),
            transition: plan.transition_hz.to_bits(), stopband: plan.stopband_hz.to_bits(),
            decoder_rate: plan.decoder_rate.map(f64::to_bits), auxiliary: config.channel != 1,
            direct: source.is_direct(),
        };
        let cached = COSTS.lock().iter().find(|(candidate, _)| candidate == &key).map(|(_, cost)| *cost);
        let cost = if let Some(cost) = cached { cost } else {
            if !allow_calibration {
                return Err("planned fine/framer cost profile is not calibrated; run command preflight".into());
            }
            // Only calibrations serialize; live cached reads must keep progressing.
            let _calibration = CALIBRATION.lock();
            let cached = COSTS.lock().iter().find(|(candidate, _)| candidate == &key).map(|(_, cost)| *cost);
            if let Some(cost) = cached { cost } else {
                let cost = if key.direct {
                    calibrate_direct(&key, plan, fs)?
                } else {
                    calibrate(&key, plan, coarse_rate)?
                };
                let mut costs = COSTS.lock();
                if costs.len() == PROFILE_CAPACITY { costs.pop_front(); }
                costs.push_back((key, cost));
                cost
            }
        };
        required += cost * source_rate;
    }
    required *= sdr_pipeline::FINE_COST_RESERVE;
    if !required.is_finite() {
        return Err("planned fine/framer cost calibration invalid".into());
    }
    Ok(json!({
        "required_ns_per_second": required,
        "limit_ns_per_second": sdr_pipeline::FINE_COST_LIMIT_NS,
        "reserve": sdr_pipeline::FINE_COST_RESERVE,
        "scope": "fine+framer",
        "excludes": ["receiver workers", "PFB", "transport"]
    }))
}

fn calibrate(key: &CostKey, plan: &RatePlan, coarse_rate: f64) -> Result<f64, String> {
    let bins: Vec<u32> = (0..key.selected_bins as u32).collect();
    let mut channel = FineChannel::new_planned(FineSpec {
        id: 1, epoch: 1, first_seq: 0, bin: bins[key.selected_bins - 1],
        offset_hz: 10_000.0, decimate: plan.decimate, coarse_delay_samples: 0.0,
        fused_fir: true,
    }, &bins, coarse_rate, key.input_hop, key.groups, plan)
        .ok_or("planned fine/framer calibration channel unavailable")?;
    let mut framer = if key.auxiliary {
        BasebandFramer::new_aux(1, 1, 100e6, plan.output_rate)
    } else {
        BasebandFramer::new(1, 1, 100e6, plan.output_rate)
    }.ok_or("planned fine/framer calibration framer unavailable")?;
    let iq = (0..key.groups * bins.len()).flat_map(|index| {
        let value = (index % 251) as f32;
        [1200.0 + value, 900.0 - value]
    }).collect();
    let mut packet = PfbOutput { seq: 0, epoch: 1, stamp: 0, input_index: 0,
        groups: key.groups as u32, process_ns: 0, iq };
    // Each batch spans a complete 30 fps framing cycle, including encoding and disposal.
    let packets = (coarse_rate / 30.0).ceil() as usize / key.groups + 1;
    let mut samples = [0.0f64; 5];
    for batch in 0..6 {
        let started = Instant::now();
        for _ in 0..packets {
            let output = channel.process(&packet).map_err(str::to_owned)?;
            drop(std::hint::black_box(framer.push(&output).map_err(str::to_owned)?));
            packet.seq += 1;
            packet.input_index += (key.groups * key.input_hop) as u64;
            packet.stamp = packet.input_index;
        }
        let cost = started.elapsed().as_secs_f64() * 1e9 / (packets * key.groups) as f64;
        if !cost.is_finite() || cost <= 0.0 {
            return Err("planned fine/framer calibration invalid".into());
        }
        if batch > 0 { samples[batch - 1] = cost; }
    }
    samples.sort_by(f64::total_cmp);
    Ok(samples[2])
}

fn calibrate_direct(key: &CostKey, plan: &RatePlan, fs: f64) -> Result<f64, String> {
    use websa_channelizer::{channel_clock, channel_close, channel_delay, channel_open_planned,
        channel_output_rate, channel_process, Channel};

    struct CalibrationChannel(*mut Channel);
    impl Drop for CalibrationChannel {
        fn drop(&mut self) { unsafe { channel_close(self.0) }; }
    }

    const INPUT_SAMPLES: usize = 16_240;
    const OFFSET_HZ: f64 = 10_000.0;
    let handle = channel_open_planned(fs, OFFSET_HZ, plan);
    if handle.is_null() {
        return Err("planned direct/framer calibration channel unavailable".into());
    }
    let channel = CalibrationChannel(handle);
    let output_rate = unsafe { channel_output_rate(channel.0) };
    let delay_samples = unsafe { channel_delay(channel.0) };
    let mut framer = if key.auxiliary {
        BasebandFramer::new_aux(1, 1, 100e6, output_rate)
    } else {
        BasebandFramer::new(1, 1, 100e6, output_rate)
    }.and_then(|framer| framer.with_source_rate(fs))
        .ok_or("planned direct/framer calibration framer unavailable")?;
    let iq: Vec<i16> = (0..INPUT_SAMPLES).flat_map(|index| {
        let value = (index % 251) as i16;
        [1200 + value, 900 - value]
    }).collect();
    let mut output = vec![0.0f32; 2 * (INPUT_SAMPLES / plan.decimate as usize + 2)];
    let decimate = plan.decimate as u64;
    let mut seq = 0;
    // Measure the raw capture kernel and a complete framing cycle with reused IQ storage.
    // BasebandFramer applies the same fixed 32768 normalization as the live route.
    let packets = (fs / 30.0).ceil() as usize / INPUT_SAMPLES + 1;
    let mut samples = [0.0f64; 5];
    for batch in 0..6 {
        let started = Instant::now();
        for _ in 0..packets {
            let (mut consumed, mut output_index) = (0, 0);
            if unsafe { channel_clock(channel.0, &mut consumed, &mut output_index) } != 0 {
                return Err("planned direct/framer calibration clock failed".into());
            }
            let input_index = consumed + decimate - 1 - consumed % decimate;
            let count = unsafe {
                channel_process(channel.0, iq.as_ptr(), INPUT_SAMPLES as i32,
                    output.as_mut_ptr(), (output.len() / 2) as i32)
            };
            if count < 0 {
                return Err("planned direct/framer calibration process failed".into());
            }
            let packet = FineOutput {
                channel_id: 1, epoch: 1, bin: 0, seq, stamp: consumed,
                input_index, output_index, output_rate,
                center_offset_hz: Some(-OFFSET_HZ), delay_samples,
                iq: &output[..2 * count as usize],
            };
            drop(std::hint::black_box(framer.push(&packet).map_err(str::to_owned)?));
            seq += 1;
        }
        let cost = started.elapsed().as_secs_f64() * 1e9 / (packets * INPUT_SAMPLES) as f64;
        if !cost.is_finite() || cost <= 0.0 {
            return Err("planned direct/framer calibration invalid".into());
        }
        if batch > 0 { samples[batch - 1] = cost; }
    }
    samples.sort_by(f64::total_cmp);
    Ok(samples[2])
}
