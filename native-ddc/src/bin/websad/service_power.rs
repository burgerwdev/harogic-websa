use super::receiver_workers::ChannelConfig;
use super::DevState;
use std::time::{Duration, Instant};

const POWER_MAX_AGE: Duration = Duration::from_millis(1500);

/// Match the browser's calibrated pan-bin level used for channel squelch.
pub(super) fn estimate_channel_power(
    freq: &[f64],
    power: &[f32],
    config: &ChannelConfig,
) -> Option<f64> {
    if freq.len() < 2 || freq.len() != power.len() || !config.freq.is_finite() {
        return None;
    }
    let bin = (freq[1] - freq[0]).abs();
    if !bin.is_finite() || bin <= 0.0 {
        return None;
    }
    freq.iter().zip(power)
        .filter(|(frequency, level)| (**frequency - config.freq).abs() <= bin && level.is_finite())
        .map(|(_, level)| f64::from(*level))
        .max_by(f64::total_cmp)
}

pub(super) fn measure_channel_power(
    freq: &[f64],
    power: &[f32],
    configs: &[ChannelConfig],
) -> [Option<(Instant, f64)>; 3] {
    let now = Instant::now();
    std::array::from_fn(|index| {
        configs.iter().find(|config| config.enabled && config.channel as usize == index + 1)
            .and_then(|config| estimate_channel_power(freq, power, config))
            .map(|level| (now, level))
    })
}

impl DevState {
    pub(super) fn observe_channel_power(&mut self, freq: &[f64], power: &[f32]) {
        self.channel_power = measure_channel_power(freq, power, &self.channel_configs());
    }

    pub(super) fn fresh_channel_power(&self) -> [Option<f64>; 3] {
        let now = Instant::now();
        self.channel_power.map(|measurement| {
            measurement.filter(|(measured, _)| now.saturating_duration_since(*measured) < POWER_MAX_AGE)
                .map(|(_, level)| level)
        })
    }
}
