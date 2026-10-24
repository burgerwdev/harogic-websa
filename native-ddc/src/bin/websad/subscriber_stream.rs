use super::Message;
use axum::body::Bytes;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::collections::{BTreeMap, VecDeque};

const AUDIO_HEADER_BYTES: usize = 96;
const MAX_IDENTITIES: usize = 3;
type Identity = (u32, u32);

#[derive(Default)]
pub(super) struct SubscriberStream {
    audio: BTreeMap<Identity, AudioCursor>,
    audio_order: VecDeque<Identity>,
    decode: BTreeMap<Identity, DecodeCursor>,
    decode_order: VecDeque<Identity>,
}

struct SequenceCursor {
    source: u32,
    subscriber: u32,
}

impl SequenceCursor {
    fn advance(&mut self, source: u32) -> Result<u32, String> {
        if self.source.checked_add(1) != Some(source) {
            return Err("subscriber source sequence discontinuity".into());
        }
        let subscriber = self.subscriber.checked_add(1)
            .ok_or("subscriber sequence exhausted")?;
        self.source = source;
        self.subscriber = subscriber;
        Ok(subscriber)
    }
}

#[derive(PartialEq)]
struct AudioFormat {
    rate: u64,
    frequency: u64,
    channels: u32,
    encoding: u16,
    source_rate: u64,
}

impl AudioFormat {
    fn read(bytes: &[u8]) -> Self {
        Self {
            rate: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            frequency: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            channels: u32::from_le_bytes(bytes[56..60].try_into().unwrap()),
            encoding: u16::from_le_bytes(bytes[60..62].try_into().unwrap()),
            source_rate: u64::from_le_bytes(bytes[72..80].try_into().unwrap()),
        }
    }
}

struct AudioCursor {
    sequence: SequenceCursor,
    format: AudioFormat,
}

struct DecodeCursor {
    sequence: SequenceCursor,
    channel: u32,
    snapshot_boundary: Option<u32>,
}

fn insert_cursor<T>(cursors: &mut BTreeMap<Identity, T>, order: &mut VecDeque<Identity>,
    identity: Identity, cursor: T) {
    cursors.retain(|key, _| key.0 != identity.0);
    order.retain(|key| cursors.contains_key(key));
    if cursors.len() == MAX_IDENTITIES {
        if let Some(oldest) = order.pop_front() { cursors.remove(&oldest); }
    }
    cursors.insert(identity, cursor);
    order.push_back(identity);
}

#[derive(Deserialize)]
struct DecodeEnvelope<'a> {
    channel: u32,
    channel_id: u32,
    epoch: u32,
    #[serde(borrow)]
    seq: &'a RawValue,
    #[serde(borrow)]
    event: &'a RawValue,
}

#[derive(Deserialize)]
struct EventType<'a> {
    #[serde(rename = "type", borrow)]
    kind: &'a str,
}

fn decode_frame(bytes: &[u8], envelope: &DecodeEnvelope<'_>, sequence: u32,
    start: bool) -> Message {
    let mut digits = [0u8; 10];
    let mut offset = digits.len();
    let mut remaining = sequence;
    loop {
        offset -= 1;
        digits[offset] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 { break; }
    }
    let digits = &digits[offset..];
    let seq = envelope.seq.get().as_bytes();
    let event = envelope.event.get().as_bytes();
    let seq_offset = seq.as_ptr() as usize - bytes.as_ptr() as usize;
    let event_offset = event.as_ptr() as usize - bytes.as_ptr() as usize;
    let start_event = b"{\"type\":\"start\"}";
    let length = bytes.len() - seq.len() + digits.len()
        - if start { event.len() } else { 0 }
        + if start { start_event.len() } else { 0 };
    let mut frame = Vec::with_capacity(length);
    frame.extend_from_slice(b"SDE2");
    frame.extend_from_slice(&((length - 8) as u32).to_le_bytes());
    if !start {
        frame.extend_from_slice(&bytes[8..seq_offset]);
        frame.extend_from_slice(digits);
        frame.extend_from_slice(&bytes[seq_offset + seq.len()..]);
    } else if seq_offset < event_offset {
        frame.extend_from_slice(&bytes[8..seq_offset]);
        frame.extend_from_slice(digits);
        frame.extend_from_slice(&bytes[seq_offset + seq.len()..event_offset]);
        frame.extend_from_slice(start_event);
        frame.extend_from_slice(&bytes[event_offset + event.len()..]);
    } else {
        frame.extend_from_slice(&bytes[8..event_offset]);
        frame.extend_from_slice(start_event);
        frame.extend_from_slice(&bytes[event_offset + event.len()..seq_offset]);
        frame.extend_from_slice(digits);
        frame.extend_from_slice(&bytes[seq_offset + seq.len()..]);
    }
    Message::Binary(frame.into())
}

impl SubscriberStream {
    pub(super) fn seed_decode(&mut self, channel:u32, identity:Identity, source:u32, subscriber:u32) {
        insert_cursor(&mut self.decode,&mut self.decode_order,identity,DecodeCursor {
            sequence:SequenceCursor {source,subscriber},channel,snapshot_boundary:Some(source),
        });
    }
    pub(super) fn frames(&mut self, message: Message) -> Result<Vec<Message>, String> {
        let Message::Binary(bytes) = message else { return Ok(vec![message]); };
        if bytes.starts_with(b"AUD2") {
            self.audio_frames(bytes)
        } else if bytes.starts_with(b"SDE2") {
            self.decode_frames(bytes)
        } else {
            Ok(vec![Message::Binary(bytes)])
        }
    }

    fn audio_frames(&mut self, bytes: Bytes) -> Result<Vec<Message>, String> {
        if bytes.len() < AUDIO_HEADER_BYTES {
            return Err("truncated subscriber audio header".into());
        }
        let source = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let frames = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        let identity = (
            u32::from_le_bytes(bytes[32..36].try_into().unwrap()),
            u32::from_le_bytes(bytes[36..40].try_into().unwrap()),
        );
        let format = AudioFormat::read(&bytes);
        if (source == 0) != (frames == 0) {
            return Err("subscriber audio marker must have sequence zero and no samples".into());
        }
        let mut output = Vec::with_capacity(if source > 0 && !self.audio.contains_key(&identity) { 2 } else { 1 });
        if source == 0 {
            insert_cursor(&mut self.audio, &mut self.audio_order, identity, AudioCursor {
                sequence: SequenceCursor { source: 0, subscriber: 0 }, format,
            });
            return Ok(vec![Message::Binary(bytes)]);
        }
        if !self.audio.contains_key(&identity) {
            let mut marker = bytes[..AUDIO_HEADER_BYTES].to_vec();
            marker[8..16].fill(0);
            output.push(Message::Binary(marker.into()));
            insert_cursor(&mut self.audio, &mut self.audio_order, identity, AudioCursor {
                sequence: SequenceCursor { source: source - 1, subscriber: 0 }, format,
            });
        } else if self.audio[&identity].format != format {
            return Err("subscriber audio format changed without a source marker".into());
        }
        let sequence = self.audio.get_mut(&identity).unwrap().sequence.advance(source)?;
        if sequence == source {
            output.push(Message::Binary(bytes));
        } else {
            let mut frame = bytes.to_vec();
            frame[8..12].copy_from_slice(&sequence.to_le_bytes());
            output.push(Message::Binary(frame.into()));
        }
        Ok(output)
    }

    fn decode_frames(&mut self, bytes: Bytes) -> Result<Vec<Message>, String> {
        if bytes.len() < 8 || u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize != bytes.len() - 8 {
            return Err("invalid subscriber decode envelope length".into());
        }
        let envelope: DecodeEnvelope<'_> = serde_json::from_slice(&bytes[8..])
            .map_err(|error| format!("decode subscriber event: {error}"))?;
        let identity = (envelope.channel_id, envelope.epoch);
        let channel = envelope.channel;
        let source: u32 = serde_json::from_str(envelope.seq.get())
            .map_err(|error| format!("decode subscriber sequence: {error}"))?;
        if let Some(cursor)=self.decode.get_mut(&identity) {
            if cursor.snapshot_boundary.is_some_and(|boundary|source<=boundary) {return Ok(Vec::new());}
            cursor.snapshot_boundary=None;
        }
        let first = source == 0 || !self.decode.contains_key(&identity);
        let start = if first {
            let event: EventType<'_> = serde_json::from_str(envelope.event.get())
                .map_err(|error| format!("decode subscriber event type: {error}"))?;
            event.kind == "start"
        } else { false };
        if source == 0 && !start {
            return Err("subscriber decode sequence zero must be a start event".into());
        }
        let mut output = Vec::with_capacity(if first && !start { 2 } else { 1 });
        let sequence = if first {
            self.decode.retain(|_, cursor| cursor.channel != channel);
            self.decode_order.retain(|key| self.decode.contains_key(key));
            let subscriber = if start { 0 } else { 1 };
            insert_cursor(&mut self.decode, &mut self.decode_order, identity, DecodeCursor {
                sequence: SequenceCursor { source, subscriber }, channel, snapshot_boundary:None,
            });
            if !start { output.push(decode_frame(&bytes, &envelope, 0, true)); }
            subscriber
        } else {
            let cursor = self.decode.get_mut(&identity).unwrap();
            if cursor.channel != channel {
                return Err("subscriber decode channel changed within an identity".into());
            }
            cursor.sequence.advance(source)?
        };
        if sequence == source {
            output.push(Message::Binary(bytes));
        } else {
            output.push(decode_frame(&bytes, &envelope, sequence, false));
        }
        Ok(output)
    }
}
