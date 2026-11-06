//! Service admission and subscription budgets. Channel-dependent budgets derive
//! from MAX_CHANNELS; recording capacity and per-channel DSP budgets are independent.
use std::time::Duration;
pub(super) use websa_channelizer::stream_limits::{DECODE_ACK_BYTES, DECODE_ACK_WINDOW};

pub(super) const MAX_CHANNELS: usize = 8;
pub(super) const FAX_SNAPSHOT_BYTES_PER_CHANNEL: usize = 20 * 1024 * 1024;
// Retained gray pixels become base64 on the wire. Include every channel and
// envelope headroom; sent/unacknowledged data has its own bounded ACK credit.
pub(super) const DECODE_PENDING_BYTE_LIMIT: u64 = MAX_CHANNELS as u64
    * (FAX_SNAPSHOT_BYTES_PER_CHANNEL as u64).div_ceil(3) * 4 + DECODE_ACK_BYTES as u64;
pub(super) const IQ_PENDING_BYTES: u64 = 4 * 1024 * 1024;
pub(super) const SEND_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const DECODE_ACK_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{load_golden, DevState};

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
        assert_eq!(state.sdr_channels.len(), MAX_CHANNELS);
        let mut last = crate::receiver_workers::ChannelConfig::for_slot(MAX_CHANNELS as u32);
        assert!(last.validate().is_ok());
        last.channel += 1;
        assert!(last.validate().is_err());
    }
}
