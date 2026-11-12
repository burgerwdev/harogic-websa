export interface ScanSignal {
  service_id?: string; current_capture_epoch?: string; id: string; generation: string;
  center_hz: number; bandwidth_hz: number; low_hz: number; high_hz: number;
  excess_db: number; activity: string; label: string; verdict: string; identify_state: string;
  suggested_mode: string | null; protocol: string | null; receiver_available: boolean;
  decoder_available: boolean; receiver_uid: string | null; source: string;
  sampled_at_ms: string | null; evidence?: Record<string, unknown>; flags: string[];
  observed_hit_ratio: number;
  timing?: { queue_ms: number; sampling_ms: number; feature_queue_ms: number; processing_ms: number;
    total_ms: number; observation_seconds: number; checkpoints: number; early_exit: boolean };
}

export interface ScanSnapshot {
  service_id: string; scan_id: string; revision: string;
  capture: { epoch: string; center_hz: number; usable_bandwidth_hz: number | null;
    input_rate_hz: number | null; state: string };
  running: boolean; state: string; range: { low_hz: number; high_hz: number } | null;
  signals: ScanSignal[]; reason: string | null; probe_slots_used: number; truncated: number;
  auto_identify: boolean; bin_spacing_hz: number | null; background_reliable: boolean;
  statistics: { observations: number; skipped: number; observed_seconds: number; rate_hz: number;
    analysis_cpu_seconds: number; feature_cpu_seconds: number; elapsed_seconds: number | null };
}
