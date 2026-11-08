//! Service admission and subscription budgets. Channel-dependent budgets derive
//! from the shared capacity contract; per-channel DSP budgets remain independent.
use std::time::Duration;
pub(super) use websa_channelizer::stream_limits::{DECODE_ACK_BYTES, DECODE_ACK_WINDOW};

pub(super) use websa_channelizer::service_capacity::{MAX_CHANNELS, MAX_RECORDING_TASKS, CAPACITY_TARGET};
pub(super) const FAX_SNAPSHOT_BYTES_PER_CHANNEL: usize = 20 * 1024 * 1024;
// Retained gray pixels become base64 on the wire. Include every channel and
// envelope headroom; sent/unacknowledged data has its own bounded ACK credit.
pub(super) const DECODE_PENDING_BYTE_LIMIT: u64 = MAX_CHANNELS as u64
    * (FAX_SNAPSHOT_BYTES_PER_CHANNEL as u64).div_ceil(3) * 4 + DECODE_ACK_BYTES as u64;
pub(super) const IQ_PENDING_BYTES: u64 = 4 * 1024 * 1024;
pub(super) const INPUT_JOBS: usize = 1024;
pub(super) const INPUT_BYTES_MIN: u64 = 4 * 1024 * 1024;
pub(super) const INPUT_BYTES_MAX: u64 = 32 * 1024 * 1024;
pub(super) const INPUT_BURST_NS: u64 = 500_000_000;
pub(super) const INPUT_MEDIA_NS: u64 = 2_000_000_000;

pub(super) fn input_bytes_limit(rate: f64, bytes_per_complex: usize) -> u64 {
    (rate * bytes_per_complex as f64 * INPUT_BURST_NS as f64 / 1e9)
        .ceil().clamp(INPUT_BYTES_MIN as f64, INPUT_BYTES_MAX as f64) as u64
}
pub(super) const SEND_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const DECODE_ACK_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{load_golden, DevState};

    #[test]
    fn input_budget_follows_sample_rate_and_representation_with_a_memory_ceiling() {
        assert_eq!(input_bytes_limit(48_000.0, 8), INPUT_BYTES_MIN);
        assert_eq!(input_bytes_limit(7_812_500.0, 4), 15_625_000);
        assert_eq!(input_bytes_limit(7_812_500.0 / 3.0, 8), 10_416_667);
        assert_eq!(input_bytes_limit(31_250_000.0, 8), INPUT_BYTES_MAX);
    }

    #[test]
    fn advertised_channel_schema_matches_runtime_admission_and_capabilities() {
        let (golden, schema) = load_golden();
        let channel = schema["commands"]["SET_SDR_CHANNEL"]["params"].as_array().unwrap()
            .iter().find(|param| param["name"] == "channel").unwrap();
        assert_eq!(channel["min"], 1);
        assert_eq!(channel["max"], MAX_CHANNELS);
        assert_eq!(channel["choices"], serde_json::json!((1..=MAX_CHANNELS).collect::<Vec<_>>()));
        let mut state = DevState::new(golden, schema);
        assert_eq!(state.receiver_snapshot()["capabilities"]["max_channels"], MAX_CHANNELS);
        assert_eq!(state.receiver_snapshot()["capabilities"]["max_recording_tasks"], MAX_CHANNELS);
        assert_eq!(state.receiver_snapshot()["capabilities"]["capacity_target"], CAPACITY_TARGET);
        assert_eq!(state.sdr_channels.len(), MAX_CHANNELS);
        let mut last = crate::receiver_workers::ChannelConfig::for_slot(MAX_CHANNELS as u32);
        assert!(last.validate().is_ok());
        last.channel += 1;
        assert!(last.validate().is_err());
    }
}
