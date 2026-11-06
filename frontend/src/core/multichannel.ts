import type { ModeControls } from "./receiverControls";
/** P2/P3 contract. Transport-independent; logical identities never encode slot positions. */
export type ReceiverUid = string;
export type U64 = string;
export type Purpose = 'listen' | 'digital' | 'iq';
export type TrackKind = 'iq' | 'audio' | 'results';
export type AudioRecordingFormat = 'wav' | 'flac' | 'opus';
export type TaskState = 'preparing' | 'waiting' | 'recording' | 'finalizing' | 'completed' | 'failed';
export type NoiseReductionAlgorithm = 'wiener' | 'dfn' | 'gtcrn' | 'ul-unas' | 'fastenhancer';
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
  processors: (ModeControls & { id: string; kind: 'demodulator' | 'decoder'; mode: string; minimum_rate_hz: number;
    bandwidth_hz?: number; pcm_gain?: number; agc?: boolean; squelch_dbm?: number;
    nr?: boolean; nr_algorithm?: NoiseReductionAlgorithm; nr_strength?: number; nr_atten_db?: number })[];
}
export interface Fault {
  scope: 'request' | 'consumer' | 'recording' | 'receiver' | 'capture';
  code: string; message: string; receiver_uid?: ReceiverUid; task_id?: string;
  request_id?: string; retryable: boolean;
}
export interface ReceiverState {
  /** Confirmed audio settings, excluding logical identity and independent IQ planning. */
  audio_signature?: string;
  processing_state?: 'disabled'|'waiting'|'preparing'|'active'|'backlogged'|'retiring'|'failed';
  processing_fault?: {error:string;generation:number;receiver_uid:string;service_id:string}|null;
  processing_runtime?: {state:string;processing_generation:string;oldest_input_wait_ns:number;input_held_bytes:number;input_held_media_ns:number;
    observed_receiver?: {nr_requested:string;nr_effective:string|null;nr_state:string;nr_reason?:string|null;protocol_waiting?:boolean;model_sessions?:unknown}|null;
    fault?: {error:string}|null}|null;
  control_receipts?: {receiver_uid:string;service_id:string;revision:string;request_id:string;processing_generation:string|null;status:string;reason:string|null;boundary: Record<string,unknown>|null}[];
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
  track_id: string; kind: TrackKind; format: 'cf32_le' | 'pcm_f32' | 'f32_le' | 'jsonl';
  source: 'baseband' | 'analysis_audio' | 'published_audio' | 'decoder_results';
  compression: 'zstd_auto_lossless' | 'none'; state: TaskState;
  segments: { segment_id?: string; frequency_hz?: number; baseband?: { passband_hz: number }; stream: StreamIdentity; first_input_index: U64; samples: U64;
    actual_rate_hz: number | null; end_reason: string | null; events?: U64; mode?: string; prefix_missing?: boolean; channels?: number; first_audio_index?: U64; next_audio_index?: U64; source?: Record<string, unknown> }[];
  raw_bytes: U64; stored_bytes: U64;
}
export interface RecordingTask {
  name?: string; audio_format?: AudioRecordingFormat;
  task_id: string; receiver_uid: ReceiverUid; state: TaskState;
  tracks: RecordingTrack[]; fault: Fault | null;
  service_id?: string; created_at_ms?: U64; accepted_bytes?: U64; queued_bytes?: U64;
}
export interface ReceiverMode {
  id: string; kind: 'analog' | 'digital'; label: string; provider: string; available: boolean;
  unavailableReason: string | null; defaultBandwidthHz: number; minimumRateHz: number;
  minBandwidthHz?: number; maxBandwidthHz?: number; bandwidthPresetsHz?: number[]; bandwidthEditable?: boolean;
  audioCompanion: string | null; outputs: string[]; sideband: string;
}
export function receiverHasAudio(snapshot: ServiceSnapshot, config: ReceiverConfig): boolean {
  if (snapshot.transport === 'fixture') return config.purpose === 'listen';
  return config.processors.some(processor => snapshot.capabilities.mode_descriptors?.some(mode =>
    mode.id === processor.mode && mode.available && (mode.outputs.includes('audio') || mode.audioCompanion !== null)));
}
export function receiverHasResults(snapshot: ServiceSnapshot, config: ReceiverConfig): boolean {
  if (snapshot.transport === 'fixture') return config.purpose === 'digital';
  return config.processors.some(processor => snapshot.capabilities.mode_descriptors?.some(mode =>
    mode.id === processor.mode && mode.available && mode.outputs.some(kind => kind !== 'audio')));
}
export interface ServiceSnapshot {
  contract: 'multichannel-draft-v1'; transport: 'fixture' | 'native'; revision: U64;
  service_id?: string; state_revision?: U64; action_token?: string;
  capabilities: { receiver_presets?: boolean; max_channels: number; capacity_target: number; modes: string[]; commands?: string[]; recording?: boolean; max_recording_tasks?: number; recording_kinds?: TrackKind[]; recording_unavailable_reason?: string | null; results_archive_export?: boolean; audio_archive_export?: boolean; audio_flac_export?: boolean; audio_opus_export?: boolean; audio_archive_read?: boolean; audio_range_max_samples?: U64; audio_range_max_seconds?: number; iq_archive_read?: boolean; iq_range_max_samples?: U64; iq_file_demodulate?: boolean; iq_file_demodulate_modes?: string[]; iq_file_demodulate_max_seconds?: number; iq_file_demodulate_offset?: boolean; iq_archive_collection?: boolean; iq_collection_max_samples?: U64; iq_collection_max_segments?: number; iq_archive_export?: boolean; iq_export_max_samples?: U64; monitor_mixing?: boolean; mode_descriptors?: ReceiverMode[] };
  capture: { source_id: string; center_hz: number; usable_bandwidth_hz: number | null;
    input_rate_hz: number | null; epoch: U64; state: 'running' | 'failed' | 'idle';
    parameters_state?: 'live' | 'last_known' | 'unknown' | 'simulated'; source_kind?: 'fixture' | 'hardware' };
  receivers: ReceiverState[]; outputs: OutputSubscription[]; recordings: RecordingTask[];
  recording_resources?: { active_tasks: number; max_tasks: number; available_tasks: number } | null;
}
export interface ConfigureCommand {
  type: 'configure'; request_id: string; expected_revision: U64;
  receiver_uid: ReceiverUid; config: ReceiverConfig; recover?:boolean;
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
  | {type:'action';request_id:string;receiver_uid:ReceiverUid;expected_revision:U64;action_id:string;action_token:string;action:'radiofax_new_page'|'radiofax_reset_correction'}
  | { type: 'record_start'; request_id: string; receiver_uid: ReceiverUid; kinds: TrackKind[]; audio_format?: AudioRecordingFormat }
  | { type: 'record_stop'; request_id: string; task_id: string }
  | { type: 'record_rename'; request_id: string; task_id: string; name: string }
  | { type: 'record_delete'; request_id: string; task_id: string }
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
