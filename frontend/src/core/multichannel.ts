/** P2/P3 contract. Transport-independent; logical identities never encode slot positions. */
export type ReceiverUid = string;
export type U64 = string;
export type Purpose = 'listen' | 'digital' | 'iq';
export type TrackKind = 'iq' | 'audio' | 'results';
export type TaskState = 'preparing' | 'waiting' | 'recording' | 'finalizing' | 'completed' | 'failed';
export interface StreamIdentity { source_id: string; epoch: U64; channel_id: U64 }
export interface SourceClock {
  input_index: U64; output_index: U64; fraction_num: U64; fraction_den: U64;
  input_rate_hz: number; actual_rate_hz: number; delay_seconds: number | null;
  utc_reliable: boolean;
}
export interface BasebandConfig {
  frequency_hz: number; passband_hz: number; transition_hz: number;
  minimum_rate_hz: number;
}
export interface ReceiverConfig {
  name: string; color: string; tags: string[]; enabled: boolean; purpose: Purpose;
  baseband: BasebandConfig;
  processors: { id: string; kind: 'demodulator' | 'decoder'; mode: string; minimum_rate_hz: number }[];
}
export interface Fault {
  scope: 'request' | 'consumer' | 'recording' | 'receiver' | 'capture';
  code: string; message: string; receiver_uid?: ReceiverUid; task_id?: string;
  request_id?: string; retryable: boolean;
}
export interface ReceiverState {
  receiver_uid: ReceiverUid; source_id: string; display_number: number; revision: U64;
  confirmed: ReceiverConfig; stream: StreamIdentity | null; clock: SourceClock | null;
  state: 'running' | 'stopped' | 'interrupted' | 'failed'; fault: Fault | null;
}
export interface OutputSubscription {
  subscription_id: string; receiver_uid: ReceiverUid; kind: TrackKind;
  format: 'cf32_le' | 'pcm_f32' | 'jsonl'; actual_rate_hz: number | null;
  state: 'starting' | 'active' | 'ended' | 'failed'; fault: Fault | null;
  samples?: U64; bytes?: U64;
}
export interface RecordingTrack {
  track_id: string; kind: TrackKind; format: 'cf32_le' | 'pcm_f32' | 'jsonl';
  source: 'baseband' | 'analysis_audio' | 'decoder_results';
  compression: 'zstd_auto_lossless' | 'none'; state: TaskState;
  segments: { stream: StreamIdentity; first_input_index: U64; samples: U64;
    actual_rate_hz: number | null; end_reason: string | null }[];
  raw_bytes: U64; stored_bytes: U64;
}
export interface RecordingTask {
  task_id: string; receiver_uid: ReceiverUid; state: TaskState;
  tracks: RecordingTrack[]; fault: Fault | null;
}
export interface ServiceSnapshot {
  contract: 'multichannel-draft-v1'; transport: 'fixture' | 'native'; revision: U64;
  service_id?: string;
  capabilities: { max_channels: number; capacity_target: number; modes: string[]; commands?: string[]; recording?: boolean; monitor_mixing?: boolean };
  capture: { source_id: string; center_hz: number; usable_bandwidth_hz: number;
    input_rate_hz: number; epoch: U64; state: 'running' | 'failed'; source_kind?: 'fixture' | 'hardware' };
  receivers: ReceiverState[]; outputs: OutputSubscription[]; recordings: RecordingTask[];
}
export interface ConfigureCommand {
  type: 'configure'; request_id: string; expected_revision: U64;
  receiver_uid: ReceiverUid; config: ReceiverConfig;
}
export interface Impact {
  accepted: boolean; faults: Fault[]; capture_change: boolean;
  affected_receivers: ReceiverUid[]; affected_tracks: string[];
  candidate_center_hz: number; estimated_cf32_bytes_per_second: number;
}
export interface SceneCommand {
  type: 'scene'; request_id: string; expected_revision: U64;
  configs: { receiver_uid: ReceiverUid; config: ReceiverConfig }[];
  capture_center_hz: number;
}
export type ServiceCommand = ConfigureCommand | SceneCommand
  | { type: 'record_start'; request_id: string; receiver_uid: ReceiverUid; kinds: TrackKind[] }
  | { type: 'record_stop'; request_id: string; task_id: string }
  | { type: 'output'; request_id: string; receiver_uid: ReceiverUid; enabled: boolean }
  | { type: 'create'; request_id: string; config: ReceiverConfig }
  | { type: 'retire'; request_id: string; receiver_uid: ReceiverUid };
export interface CommandReply { request_id: string; snapshot: ServiceSnapshot; fault: Fault | null }
export type ServiceEvent = { type: 'snapshot'; snapshot: ServiceSnapshot }
  | { type: 'stream_start'; receiver_uid: ReceiverUid; source_id: string; stream: StreamIdentity;
      format: 'cf32_le'; baseband: BasebandConfig; first_sequence: U64; clock: SourceClock }
  | { type: 'stream_boundary'; receiver_uid: ReceiverUid; previous: StreamIdentity | null;
      next: StreamIdentity | null; reason: string; clock: SourceClock | null; drained?: boolean }
  | { type: 'fault'; fault: Fault };
export interface ReceiverAdapter {
  snapshot(): ServiceSnapshot;
  subscribe(listener: (snapshot: ServiceSnapshot) => void): () => void;
  preflight(command: ConfigureCommand | SceneCommand): Impact;
  dispatch(command: ServiceCommand): Promise<CommandReply>;
}
/** Browser-only state. Never serializes into receiver configuration or recording commands. */
export interface MonitorRoute { enabled: boolean; gain: number; pan: number; solo: boolean }
export type ViewBinding = { type: 'follow' } | { type: 'fixed'; receiver_uid: ReceiverUid };
export const clone = <T>(value: T): T => structuredClone(value);
