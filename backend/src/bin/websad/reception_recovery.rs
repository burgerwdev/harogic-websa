//! Bounded interruption supervision. A retry never implies continuous samples.
use super::*;
use std::collections::VecDeque;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

// Processor recovery policy (not browser subscription retries).
const RETRY_LIMIT: u8 = 3;
const RETRY_INITIAL_MS: u64 = 500;
const RETRY_FAILURE_MS: u64 = 1000;
const PROGRESS_MAX_GAP: Duration = Duration::from_secs(1);
const STABLE_PROGRESS: Duration = Duration::from_secs(30);
const CHECK_PERIOD: Duration = Duration::from_millis(100);
const PREPARING_POLL: Duration = Duration::from_millis(500);
const PREPARING_TIMEOUT: Duration = Duration::from_secs(10);
const HISTORY_LIMIT: usize = 7;

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Policy {
    #[default]
    Continuous,
    Strict,
}
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum FaultKind {
    Overload,
    SourceGap,
    Device,
    Internal,
}
impl FaultKind {
    pub fn fine(error: &str) -> Self {
        if [
            "fine input queue full;",
            "fine output queue full;",
            "fine output exceeds ",
            "fine input exceeds ",
            "direct fine input exceeds ",
            "publisher IQ FIFO full;",
        ]
        .iter()
        .any(|prefix| error.starts_with(prefix))
        {
            Self::Overload
        } else {
            Self::Internal
        }
    }
    pub fn processing(error: &str) -> Self {
        if error
            .starts_with("receiver input queue failure: retained input including execution exceeds")
            || error.starts_with("per-channel receiver output exceeds ")
            || error == "per-channel receiver output queue full"
            || [
                "receiver input queue failure: sending on a full channel",
                "FT8 search queue failure: sending on a full channel",
                "FT8 result queue failure: sending on a full channel",
            ]
            .iter()
            .any(|p| error.starts_with(p))
        {
            Self::Overload
        } else {
            Self::Internal
        }
    }
    pub fn capture(error: &str) -> Self {
        // -130 is emitted before another vendor call, ONLY for our full USB
        // handoff. Generic USB errors (-8/-9), timeouts and panics are not retries.
        if error.contains("status=-130 ") || error.contains("IQS stamp discontinuity ") {
            Self::SourceGap
        } else if error.contains("SDK") {
            Self::Device
        } else {
            Self::Internal
        }
    }
    fn recoverable(self) -> bool {
        matches!(self, Self::Overload | Self::SourceGap)
    }
}
#[derive(Clone, Debug, serde::Serialize)]
pub(super) struct Position {
    pub channel_id: u32,
    pub epoch: u32,
    #[serde(serialize_with = "decimal")]
    pub input_index: u64,
}
#[derive(Clone, Debug, serde::Serialize)]
pub(super) struct Interruption {
    pub number: u64,
    pub failed_identity: String,
    pub resumed_processing_stream: Option<Position>,
    pub kind: FaultKind,
    pub scope: &'static str,
    pub reason: String,
    pub at_ms: u64,
    pub before: Option<Position>,
    pub after: Option<Position>,
    #[serde(serialize_with = "optional_decimal")]
    pub missing_input_samples: Option<u64>,
    pub recovered_at_ms: Option<u64>,
    pub precision: &'static str,
}
struct Lane {
    uid: String,
    policy: Policy,
    key: String,
    count: u64,
    attempts: u8,
    phase: &'static str,
    since: Instant,
    due: Instant,
    stable: Option<Instant>,
    last_progress: Option<Instant>,
    progress: u64,
    source: bool,
    needs_processing: bool,
    shared: bool,
    manual: bool,
    interruption: Interruption,
    history: VecDeque<Interruption>,
    last_error: Option<String>,
}
impl Lane {
    fn note_progress(&mut self, now: Instant, progressing: bool) {
        if progressing {
            self.last_progress = Some(now);
        }
        if self
            .last_progress
            .is_none_or(|last| now.duration_since(last) > PROGRESS_MAX_GAP)
        {
            self.stable = None;
            return;
        }
        if now.duration_since(*self.stable.get_or_insert(now)) >= STABLE_PROGRESS {
            self.attempts = 0;
        }
    }
}
#[derive(Default)]
pub(super) struct Supervisor {
    lanes: BTreeMap<u32, Lane>,
    positions: BTreeMap<u32, Position>,
    shared_retry: bool,
    shared_serial: u64,
    next_check: Option<Instant>,
}
fn decimal<S: serde::Serializer>(value: &u64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&value.to_string())
}
fn optional_decimal<S: serde::Serializer>(value: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
    serde::Serialize::serialize(&value.map(|v| v.to_string()), s)
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
impl Supervisor {
    pub fn snapshot(&self, slot: u32) -> Value {
        self.lanes.get(&slot).map_or(Value::Null,|r|json!({"policy":r.policy,"state":r.phase,
            "interruptions":r.count,"attempts":r.attempts,"retry_limit":RETRY_LIMIT,
            "retry_in_ms":(r.phase=="recovering").then(||r.due.saturating_duration_since(Instant::now()).as_millis() as u64),
            "manual_recovery_allowed":r.phase=="stopped" && r.interruption.kind.recoverable(),
            "last_error":r.last_error,"last_interruption":r.interruption,"history":r.history,
            "startup":"fresh_filter_and_protocol_state; discard_two_FIR_group_delays; first_valid_output_boundary"}))
    }
    pub fn bind(&mut self, slot: u32, uid: &str) {
        if self.lanes.get(&slot).is_some_and(|r| r.uid != uid) {
            self.lanes.remove(&slot);
            self.positions.remove(&slot);
        }
    }
    pub fn configured(&mut self, slot: u32, needs_processing: bool) {
        if let Some(r) = self.lanes.get_mut(&slot) {
            r.needs_processing = needs_processing;
            if r.phase == "stopped" || r.phase == "recovering" {
                r.phase = "preparing";
                r.shared = false;
                r.last_error = None;
            }
        }
    }
    pub fn known(&self, slot: u32) -> bool {
        self.lanes.contains_key(&slot)
    }
    pub fn source_stopped(&self, slot: u32) -> bool {
        self.lanes
            .get(&slot)
            .is_some_and(|r| r.source && r.phase == "stopped")
    }
    pub fn source_interrupted(&self, slot: u32) -> bool {
        self.lanes
            .get(&slot)
            .is_some_and(|r| r.source && r.phase != "receiving")
    }
    fn observe(
        &mut self,
        slot: u32,
        uid: String,
        policy: Policy,
        key: String,
        kind: FaultKind,
        reason: String,
        source: bool,
        shared: bool,
        before: Option<Position>,
    ) -> bool {
        if self
            .lanes
            .get(&slot)
            .is_some_and(|r| r.uid == uid && r.key == key)
        {
            return false;
        }
        let old = self.lanes.remove(&slot).filter(|r| r.uid == uid);
        let (count, attempts, mut history) = old.map_or((1, 0, VecDeque::new()), |r| {
            let mut history = r.history;
            history.push_back(r.interruption);
            (r.count + 1, r.attempts, history)
        });
        while history.len() > HISTORY_LIMIT {
            history.pop_front();
        }
        let recover = policy == Policy::Continuous && kind.recoverable() && attempts < RETRY_LIMIT;
        self.lanes.insert(
            slot,
            Lane {
                uid,
                policy,
                key: key.clone(),
                count,
                attempts,
                phase: if recover { "recovering" } else { "stopped" },
                since: Instant::now(),
                due: Instant::now() + Duration::from_millis(RETRY_INITIAL_MS << attempts.min(RETRY_LIMIT)),
                stable: None,
                last_progress: None,
                progress: 0,
                source,
                needs_processing: false,
                shared,
                manual: false,
                interruption: Interruption {
                    number: count,
                    failed_identity: key,
                    resumed_processing_stream: None,
                    kind,
                    scope: if shared {
                        "capture"
                    } else if source {
                        "baseband"
                    } else {
                        "processing"
                    },
                    reason,
                    at_ms: now_ms(),
                    before,
                    after: None,
                    missing_input_samples: None,
                    recovered_at_ms: None,
                    precision: "unknown_gap",
                },
                history,
                last_error: None,
            },
        );
        true
    }
    pub fn observe_iq(&mut self, slot: u32, id: u32, epoch: u32, input: u64, end: u64) {
        let current = Position {
            channel_id: id,
            epoch,
            input_index: input,
        };
        if let Some(r) = self
            .lanes
            .get_mut(&slot)
            .filter(|r| r.phase == "preparing" && (r.source || !r.needs_processing)
                && r.interruption.after.is_none())
        {
            // Removing a failed processor leaves the healthy IQ source intact.
            // Its next real packet confirms IQ-only reception, not PCM recovery.
            if !r.source || r.interruption
                .before
                .as_ref()
                .is_none_or(|old| old.channel_id != id || old.epoch != epoch)
            {
                r.interruption.after = Some(current.clone());
                // An epoch belongs to one capture. Never subtract clocks across it.
                if r.source && !r.shared {
                    if let Some(old) = r
                        .interruption
                        .before
                        .as_ref()
                        .filter(|old| old.epoch == epoch)
                    {
                        r.interruption.missing_input_samples = input.checked_sub(old.input_index);
                        r.interruption.precision = "input_sample_boundary";
                    }
                }
            }
        }
        // The new source may already have arrived while a processor was still
        // preparing. Removing that processor needs a subsequent packet from the
        // confirmed source; an old identity cannot complete this transition.
        if let Some(r) = self.lanes.get_mut(&slot).filter(|r| {
            r.phase == "preparing" && !r.needs_processing && r.interruption.after.as_ref()
                .is_some_and(|after| after.channel_id == id && after.epoch == epoch)
        }) {
            r.phase = "receiving";
            r.interruption.recovered_at_ms = Some(now_ms());
            r.stable = Some(Instant::now());
        }
        if let Some(r) = self.lanes.get_mut(&slot).filter(|r| {
            (r.source || !r.needs_processing)
                && r.phase == "receiving"
                && r.interruption
                    .after
                    .as_ref()
                    .is_some_and(|a| a.channel_id == id && a.epoch == epoch)
        }) {
            r.note_progress(Instant::now(), true);
        }
        self.positions.insert(
            slot,
            Position {
                input_index: end,
                ..current
            },
        );
    }
    pub fn capture_allowed(&self) -> bool {
        self.shared_retry
            && self
                .lanes
                .values()
                .any(|r| r.shared && r.phase == "recovering" && r.due <= Instant::now())
    }
    pub fn begin_capture(&mut self) {
        for lane in self
            .lanes
            .values_mut()
            .filter(|r| r.shared && r.phase == "recovering")
        {
            lane.attempts += 1;
        }
    }
    pub fn manual(&mut self, slot: u32) -> Result<(), String> {
        let Some(r) = self.lanes.get_mut(&slot) else {
            return Ok(());
        };
        if matches!(r.phase, "recovering" | "preparing") {
            return Err("reception recovery already pending".into());
        }
        if !r.interruption.kind.recoverable() {
            return Err("device or internal failure requires source repair; automatic receiver recovery is unavailable".into());
        }
        r.attempts = 0;
        r.manual = true;
        r.since = Instant::now();
        r.phase = "recovering";
        r.due = Instant::now();
        if r.shared {
            self.shared_retry = true;
        }
        Ok(())
    }
}
impl DevState {
    pub(super) fn reception_capture_failed(&mut self, error: &str) {
        let kind = FaultKind::capture(error);
        self.reception.shared_serial += 1;
        self.reception.shared_retry = kind == FaultKind::SourceGap;
        for config in self.channel_configs().into_iter().filter(|c| c.enabled) {
            let slot = config.channel;
            let Some(uid) = self.receiver_uid_for_slot(slot).map(str::to_owned) else {
                continue;
            };
            if let Some(manager) = &self.recordings {
                manager.source_failed(&uid, error);
            }
            if let Some(workers) = &mut self.receiver_workers {
                workers.fail_source(slot, error);
            }
            self.decode_snapshots.clear_channel(slot);
            self.reception.observe(
                slot,
                uid,
                config.continuity,
                format!("capture:{}", self.reception.shared_serial),
                kind,
                error.into(),
                true,
                true,
                self.reception.positions.get(&slot).cloned(),
            );
            if let Some(lane) = self.reception.lanes.get_mut(&slot) {
                lane.needs_processing = config.processing_enabled;
            }
        }
    }
    pub(super) fn advance_reception_recovery(&mut self) {
        let now = Instant::now();
        if self.reception.next_check.is_some_and(|next| next > now) {
            return;
        }
        self.reception.next_check = Some(now + CHECK_PERIOD);
        let configs = self.channel_configs();
        let registry = self.stream_registry();
        for config in configs.iter().filter(|c| c.enabled) {
            let slot = config.channel;
            let Some(uid) = self.receiver_uid_for_slot(slot).map(str::to_owned) else {
                continue;
            };
            if self
                .reception
                .lanes
                .get(&slot)
                .is_some_and(|r| r.uid != uid)
            {
                self.reception.lanes.remove(&slot);
                self.reception.positions.remove(&slot);
            }
            let fine = self
                .fine_faults()
                .iter()
                .find(|f| {
                    f.channel == slot
                        && registry
                            .iter()
                            .any(|e| e.channel_id == f.channel_id && e.epoch == f.epoch)
                })
                .cloned();
            if let Some(error) = self
                .receiver_failure
                .as_ref()
                .filter(|_| config.processing_enabled)
            {
                self.reception.observe(
                    slot,
                    uid,
                    config.continuity,
                    "global-processing".into(),
                    FaultKind::Internal,
                    error.clone(),
                    false,
                    false,
                    None,
                );
            } else if let Some(f) = fine {
                self.reception.observe(
                    slot,
                    uid,
                    config.continuity,
                    format!("iq:{}/{}", f.epoch, f.channel_id),
                    f.kind,
                    f.error,
                    true,
                    false,
                    self.reception.positions.get(&slot).cloned(),
                );
            } else if let Some(f) = self
                .receiver_workers
                .as_ref()
                .and_then(|w| w.channel_failure(slot))
            {
                if !self.reception.lanes.get(&slot).is_some_and(|r| {
                    (r.shared || r.source) && f.error.starts_with("fine source failed:")
                }) {
                    let before = registry.iter().find(|e| e.channel == slot).and_then(|e| {
                        let c = &f.last_delivered;
                        let input = c["input_index"].as_str()?.parse::<u64>().ok()?;
                        let samples = c["sample_count"].as_u64()?;
                        let rate = c["actual_rate"].as_f64()?;
                        Some(Position {
                            channel_id: e.channel_id,
                            epoch: e.epoch,
                            input_index: input.checked_add(
                                (samples as f64 * e.input_rate / rate).round() as u64,
                            )?,
                        })
                    });
                    self.reception.observe(
                        slot,
                        uid,
                        config.continuity,
                        format!("processing:{}", f.generation),
                        f.kind,
                        f.error,
                        false,
                        false,
                        before,
                    );
                }
            }
            let Some(lane) = self.reception.lanes.get_mut(&slot) else {
                continue;
            };
            lane.needs_processing = config.processing_enabled;
            lane.policy = config.continuity;
            if lane.policy == Policy::Strict && lane.phase == "recovering" && !lane.manual {
                lane.phase = "stopped";
            }

            if matches!(lane.phase, "preparing" | "receiving") {
                let progress = self
                    .receiver_workers
                    .as_ref()
                    .filter(|_| lane.needs_processing)
                    .and_then(|w| w.recovery_progress(slot));

                if let Some((count, position)) = progress {
                    if lane.phase == "preparing"
                        && count > 0
                        && (!lane.source || lane.interruption.after.is_some())
                    {
                        lane.phase = "receiving";
                        lane.interruption.resumed_processing_stream = position.clone();
                        if !lane.source {
                            lane.interruption.after = position.and_then(|p| {
                                registry
                                    .iter()
                                    .find(|e| e.channel == slot)
                                    .map(|e| Position {
                                        channel_id: e.channel_id,
                                        epoch: e.epoch,
                                        input_index: p.input_index,
                                    })
                            });
                            if let (Some(before), Some(after)) =
                                (&lane.interruption.before, &lane.interruption.after)
                            {
                                if before.epoch == after.epoch
                                    && before.channel_id == after.channel_id
                                {
                                    // The processing failure boundary may precede the most
                                    // recent IQ boundary, so only report an exact interval
                                    // when both are processing-delivered positions.
                                    lane.interruption.missing_input_samples =
                                        after.input_index.checked_sub(before.input_index);
                                    lane.interruption.precision = "input_sample_boundary";
                                }
                            }
                        }
                        lane.interruption.recovered_at_ms = Some(now_ms());
                        lane.stable = Some(now);
                    }
                    if !lane.source {
                        lane.note_progress(now, count > lane.progress);
                    }
                    lane.progress = count;
                }
                if lane.source || !lane.needs_processing {
                    lane.note_progress(now, false);
                }
                continue;
            }
            if lane.phase != "recovering"
                || lane.due > now
                || lane.shared && self.pipeline.is_none()
            {
                continue;
            }
            // A native call still executing owns its state. Do not spawn a replacement.
            if self
                .receiver_workers
                .as_ref()
                .is_some_and(|w| !w.restart_ready(slot))
            {
                lane.last_error = Some("waiting for previous execution owner to exit".into());
                lane.due = now + PREPARING_POLL;
                if lane.since.elapsed() > PREPARING_TIMEOUT {
                    lane.phase = "stopped";
                }
                continue;
            }
            if !lane.shared {
                lane.attempts += 1;
            }
            let result = self.recover_reception_channel(slot);
            let lane = self.reception.lanes.get_mut(&slot).unwrap();
            match result {
                Ok(()) => {
                    lane.phase = "preparing";
                    lane.last_error = None;
                }
                Err(error) => {
                    lane.last_error = Some(error);
                    lane.due = now + Duration::from_millis(RETRY_FAILURE_MS << lane.attempts.min(RETRY_LIMIT));
                    if lane.attempts >= RETRY_LIMIT
                        || !lane
                            .last_error
                            .as_ref()
                            .is_some_and(|e| e.contains("retiring"))
                    {
                        lane.phase = "stopped";
                    }
                }
            }
        }
    }
    fn recover_reception_channel(&mut self, slot: u32) -> Result<(), String> {
        let config = self.channel_config(slot).ok_or("receiver unavailable")?;
        let source = self.reception.lanes.get(&slot).is_some_and(|r| {
            r.source && (!r.shared || !self.stream_registry().iter().any(|e| e.channel == slot))
        });
        if source {
            self.ddc_channel_command(move |ddc| ddc.recover_service_channel(config))?;
        }
        if let Some(workers) = &mut self.receiver_workers {
            workers.restart_channel(slot)?;
        }
        self.processing_branches.restart_channel(slot);
        self.decode_snapshots.clear_channel(slot);
        self.config_version += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fault(s: &mut Supervisor, n: u32, policy: Policy, kind: FaultKind) {
        s.observe(
            1,
            "receiver-A".into(),
            policy,
            format!("failure-{n}"),
            kind,
            "overload".into(),
            true,
            false,
            Some(Position {
                channel_id: n,
                epoch: 5,
                input_index: 100,
            }),
        );
    }
    #[test]
    fn bounded_retries_keep_gap_records_and_require_a_new_identity() {
        let mut s = Supervisor::default();
        for n in 1..=u32::from(RETRY_LIMIT)+1 {
            fault(&mut s, n, Policy::Continuous, FaultKind::Overload);
            assert_eq!(s.snapshot(1)["interruptions"], n);
            assert_eq!(s.snapshot(1)["retry_limit"], RETRY_LIMIT);
            if n == u32::from(RETRY_LIMIT)+1 {
                assert_eq!(s.snapshot(1)["state"], "stopped");
                break;
            }
            assert!(s.lanes[&1].due > Instant::now());
            let r = s.lanes.get_mut(&1).unwrap();
            r.phase = "preparing";
            r.attempts += 1;
            s.observe_iq(1, n, 5, 110, 120);
            assert_eq!(
                s.snapshot(1)["state"],
                "preparing",
                "old source cannot complete recovery"
            );
            s.observe_iq(1, n + 10, 5, 140, 150);
            assert_eq!(s.snapshot(1)["state"], "receiving");
            assert_eq!(
                s.snapshot(1)["last_interruption"]["missing_input_samples"],
                "40"
            );
        }
        assert_eq!(s.snapshot(1)["history"].as_array().unwrap().len(), usize::from(RETRY_LIMIT));
        s.manual(1).unwrap();
        assert!(
            s.manual(1).is_err(),
            "duplicate resume must not reset the retry budget"
        );
    }
    #[test]
    fn repeated_failure_during_preparation_is_counted_and_only_live_progress_resets_budget() {
        let mut s = Supervisor::default();
        fault(&mut s, 1, Policy::Continuous, FaultKind::Overload);
        s.lanes.get_mut(&1).unwrap().phase = "preparing";
        fault(&mut s, 2, Policy::Continuous, FaultKind::Overload);
        assert_eq!(s.snapshot(1)["interruptions"], 2);
        let now = Instant::now();
        let lane = s.lanes.get_mut(&1).unwrap();
        lane.attempts = RETRY_LIMIT;
        lane.note_progress(now, true);
        lane.note_progress(now + Duration::from_secs(31), false);
        assert_eq!(lane.attempts, RETRY_LIMIT, "idle time must not reset the budget");
        for n in 32..=62 {
            lane.note_progress(now + Duration::from_secs(n), true);
        }
        assert_eq!(lane.attempts, 0);
        s.bind(1, "replacement-uid");
        assert!(s.snapshot(1).is_null());
    }
    #[test]
    fn configuration_repair_iq_only_keeps_the_already_observed_source_identity() {
        let mut s = Supervisor::default();
        fault(&mut s, 1, Policy::Continuous, FaultKind::SourceGap);
        s.configured(1, true);
        s.observe_iq(1, 1, 5, 110, 120);
        assert!(s.snapshot(1)["last_interruption"]["after"].is_null(), "old IQ cannot establish source recovery");
        s.observe_iq(1, 2, 5, 140, 150);
        let observed = s.snapshot(1);
        assert_eq!(observed["state"], "preparing", "new IQ does not establish processor recovery");
        assert_eq!(observed["last_interruption"]["after"]["channel_id"], 2);
        assert_eq!(observed["last_interruption"]["missing_input_samples"], "40");

        s.configured(1, false);
        assert_eq!(s.snapshot(1)["state"], "preparing", "removal itself is not observed reception");
        s.observe_iq(1, 1, 5, 160, 170);
        assert_eq!(s.snapshot(1)["state"], "preparing", "a late old-source packet cannot complete recovery");
        s.observe_iq(1, 2, 5, 180, 190);
        let resumed = s.snapshot(1);
        assert_eq!(resumed["state"], "receiving");
        assert_eq!(resumed["interruptions"], observed["interruptions"]);
        assert_eq!(resumed["last_interruption"]["after"], observed["last_interruption"]["after"]);
        assert_eq!(resumed["last_interruption"]["missing_input_samples"], "40");
        assert!(resumed["last_interruption"]["resumed_processing_stream"].is_null());
        assert!(resumed["last_interruption"]["recovered_at_ms"].is_u64());
    }
    #[test]
    fn failed_source_does_not_finish_closed_mailboxes_or_claim_pcm_from_iq_alone() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        let mut source = service_fixture::FixtureSources::new();
        source.configure(&state.channel_configs()).unwrap();
        let registry = source.registry();
        let raw = source.step().unwrap();
        state.fixture_sources = Some(source);
        state.remember_receiver_streams();
        state
            .process_receiver_frames_with_registry(&raw, &registry, true)
            .unwrap();
        state
            .receiver_workers
            .as_mut()
            .unwrap()
            .fail_source(1, "test source gap");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !state.receiver_workers.as_ref().unwrap().restart_ready(1) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        state.hardware_presentation_failed("SDK IQS batch status=-130 samples=0".into());
        assert!(
            state.receiver_failure.is_none(),
            "source gap must not become global closed-mailbox failure"
        );
        state.reception.lanes.get_mut(&1).unwrap().phase = "preparing";
        state.reception.observe_iq(1, 90, 90, 100, 200);
        assert_eq!(
            state.reception.snapshot(1)["state"],
            "preparing",
            "new IQ alone does not confirm processing recovery"
        );
    }
    #[test]
    fn shared_source_gap_notifies_every_enabled_receiver_and_preserves_strict_stop() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.swp.mode = "sdr".into();
        state.sdr_channels[1].enabled = true;
        state.sdr_channels[1].continuity = Policy::Strict;
        state.remember_receiver_streams();
        state.reception_capture_failed("SDK IQS batch status=-130 samples=0");
        for slot in [1, 2] {
            let r = state.reception.snapshot(slot);
            assert_eq!(r["last_interruption"]["scope"], "capture");
            assert_eq!(r["interruptions"], 1);
            assert!(r["last_interruption"]["missing_input_samples"].is_null());
        }
        assert_eq!(state.reception.snapshot(1)["state"], "recovering");
        assert_eq!(state.reception.snapshot(2)["state"], "stopped");
        assert!(state.reception.source_stopped(2));
        assert!(!state.reception.capture_allowed(), "backoff is enforced");
    }
    #[test]
    fn strict_device_and_internal_errors_do_not_auto_retry_or_forge_clocks() {
        let mut s = Supervisor::default();
        fault(&mut s, 1, Policy::Strict, FaultKind::Overload);
        assert_eq!(s.snapshot(1)["state"], "stopped");
        s.manual(1).unwrap();
        s.lanes.get_mut(&1).unwrap().phase = "preparing";
        s.observe_iq(1, 2, 6, 0, 20);
        assert!(s.snapshot(1)["last_interruption"]["missing_input_samples"].is_null());
        for kind in [FaultKind::Internal, FaultKind::Device] {
            let mut s = Supervisor::default();
            fault(&mut s, 1, Policy::Continuous, kind);
            assert_eq!(s.snapshot(1)["state"], "stopped");
            assert!(s.manual(1).is_err());
        }
        assert_eq!(
            FaultKind::capture("SDK IQS batch status=-130 samples=0"),
            FaultKind::SourceGap
        );
        assert_eq!(
            FaultKind::capture("SDK IQS next/FFT status=-8 samples=0"),
            FaultKind::Device
        );
        assert_eq!(
            FaultKind::processing("FT8 result queue failure: sending on a closed channel"),
            FaultKind::Internal
        );
        assert_eq!(
            FaultKind::processing("FT8 result queue failure: sending on a full channel"),
            FaultKind::Overload
        );
    }
    #[test]
    fn auto_source_resume_changes_only_failed_identity_and_strict_neighbor_stays_stopped() {
        let mut state = DevState::new(json!({"preset_defaults":{}}), json!({}));
        state.swp.mode = "sdr".into();
        state.sdr_center = 100e6;
        state.sdr_listen = 100e6;
        state.hardware_shim = Some("test no hardware opened".into());
        for i in 0..3 {
            state.sdr_channels[i].enabled = true;
            state.sdr_channels[i].freq = 100e6;
            state.sdr_channels[i].processing_enabled = false;
        }
        state.sdr_channels[2].continuity = Policy::Strict;
        let mut ddc =
            Ddc::configured_service(100e6, 50e6, &mut None, 16, IQ_FS, &state.channel_configs())
                .unwrap();
        let before = ddc.registry.clone();
        for slot in [2, 3] {
            let e = &before[(slot - 1) as usize];
            ddc.faults.insert(
                slot,
                fine_workers::FineFault {
                    channel: slot,
                    channel_id: e.channel_id,
                    epoch: e.epoch,
                    kind: FaultKind::Overload,
                    error: "fine input queue full; branch stopped without skipping samples".into(),
                    first_unavailable_input: Some(0),
                    last_completed_input: None,
                },
            );
        }
        state.ddc = Some(SendDdc(ddc));
        state.remember_receiver_streams();
        state.advance_reception_recovery();
        assert_eq!(state.reception.snapshot(2)["state"], "recovering");
        assert_eq!(state.reception.snapshot(3)["state"], "stopped");
        state.reception.lanes.get_mut(&2).unwrap().due = Instant::now();
        state.reception.next_check = None;
        state.advance_reception_recovery();
        assert_eq!(state.reception.snapshot(2)["state"], "preparing");
        let after = state.stream_registry();
        assert_eq!(
            before[0], after[0],
            "healthy neighbor retains its identity and clock"
        );
        assert_ne!(before[1].channel_id, after[1].channel_id);
        assert_eq!(before[2], after[2], "strict receiver does not resume");
        assert!(state.fine_faults().iter().all(|f| f.channel == 3));
        assert!(
            state
                .ddc
                .as_ref()
                .unwrap()
                .0
                .service_configs
                .as_ref()
                .unwrap()[1]
                .recovery_settle
        );
    }
}
