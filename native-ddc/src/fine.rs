//! Fine DDC and ordered subscriptions over selected OS2 PFB bins.
use crate::narrow::{
    channel_clock, channel_close, channel_delay, channel_open, channel_open_coarse,
    channel_open_planned, channel_output_rate, channel_process_float, Channel,
};
use crate::pfb_pool::{PfbOutput, PfbStream};
use crate::rate_plan::RatePlan;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::time::{Duration, Instant};

pub struct FineSpec {
    pub id: u64,
    pub epoch: u64,
    pub first_seq: u64,
    pub bin: u32,
    pub offset_hz: f64,
    pub decimate: i32,
    pub coarse_delay_samples: f64,
    pub fused_fir: bool,
}

pub struct FineChannel {
    id: u64,
    epoch: u64,
    bin: u32,
    column: usize,
    columns: usize,
    expected_seq: u64,
    input_hop: u64,
    decimate: u64,
    handle: *mut Channel,
    scratch: Vec<f32>,
    output: Vec<f32>,
    center_offset_hz: Option<f64>,
    planned: bool,
    pub output_rate: f64,
    pub delay_samples: f64,
}

pub struct FineOutput<'a> {
    pub channel_id: u64,
    pub epoch: u64,
    pub bin: u32,
    pub seq: u64,
    pub stamp: u64,
    /// Raw complex input index of the first output, or the next output when IQ is empty.
    pub input_index: u64,
    pub output_index: u64,
    pub output_rate: f64,
    /// Relative to configured IQS center; populated for FineBank-provisioned channels.
    pub center_offset_hz: Option<f64>,
    pub delay_samples: f64,
    pub iq: &'a [f32],
}

impl FineChannel {
    pub fn new(
        spec: FineSpec,
        selected_bins: &[u32],
        coarse_rate: f64,
        input_hop: usize,
        max_groups: usize,
    ) -> Option<Self> {
        Self::new_with_plan(spec, selected_bins, coarse_rate, input_hop, max_groups, None)
    }

    pub fn new_planned(
        spec: FineSpec,
        selected_bins: &[u32],
        coarse_rate: f64,
        input_hop: usize,
        max_groups: usize,
        plan: &RatePlan,
    ) -> Option<Self> {
        if spec.decimate != plan.decimate { return None; }
        Self::new_with_plan(spec, selected_bins, coarse_rate, input_hop, max_groups, Some(plan))
    }

    fn new_with_plan(
        spec: FineSpec,
        selected_bins: &[u32],
        coarse_rate: f64,
        input_hop: usize,
        max_groups: usize,
        plan: Option<&RatePlan>,
    ) -> Option<Self> {
        if max_groups == 0
            || max_groups > 65536
            || input_hop == 0
            || spec.id == 0
            || spec.id > u32::MAX as u64
            || spec.epoch == 0
            || spec.epoch > u32::MAX as u64
            || !spec.coarse_delay_samples.is_finite()
            || spec.coarse_delay_samples < 0.0
        {
            return None;
        }
        let column = selected_bins
            .iter()
            .position(|&selected| selected == spec.bin)?;
        let handle = if let Some(plan) = plan {
            channel_open_planned(coarse_rate, spec.offset_hz, plan)
        } else if spec.fused_fir {
            channel_open_coarse(coarse_rate, spec.offset_hz, spec.decimate)
        } else {
            channel_open(coarse_rate, spec.offset_hz, spec.decimate)
        };
        if handle.is_null() {
            return None;
        }
        let output_rate = unsafe { channel_output_rate(handle) };
        let delay_samples =
            unsafe { channel_delay(handle) } + spec.coarse_delay_samples / spec.decimate as f64;
        Some(Self {
            id: spec.id,
            epoch: spec.epoch,
            bin: spec.bin,
            column,
            columns: selected_bins.len(),
            expected_seq: spec.first_seq,
            input_hop: input_hop as u64,
            decimate: spec.decimate as u64,
            handle,
            scratch: vec![0.0; 2 * max_groups],
            output: vec![0.0; 2 * (max_groups / spec.decimate as usize + 2)],
            center_offset_hz: None,
            planned: plan.is_some(),
            output_rate,
            delay_samples,
        })
    }

    pub fn process(&mut self, packet: &PfbOutput) -> Result<FineOutput<'_>, &'static str> {
        if packet.epoch != self.epoch || packet.seq != self.expected_seq {
            return Err("fine channel epoch/sequence mismatch");
        }
        let groups = packet.groups as usize;
        if groups > self.scratch.len() / 2 || packet.iq.len() != 2 * groups * self.columns {
            return Err("fine channel input shape mismatch");
        }
        for (target, group) in self
            .scratch
            .chunks_exact_mut(2)
            .take(groups)
            .zip(packet.iq.chunks_exact(self.columns * 2))
        {
            target.copy_from_slice(&group[2 * self.column..2 * self.column + 2]);
        }
        let (mut consumed, mut output_index) = (0, 0);
        if unsafe { channel_clock(self.handle, &mut consumed, &mut output_index) } != 0 {
            return Err("fine channel clock failed");
        }
        // PFB emits at hop-1; the fine kernel emits after a full decimation period.
        // Both phases carry across packet boundaries, including packets with no output.
        let first_coarse = self.input_hop - 1 - packet.input_index % self.input_hop;
        let fine_offset = (self.decimate - 1 - consumed % self.decimate)
            .checked_mul(self.input_hop)
            .ok_or("fine channel input clock overflow")?;
        let input_index = packet.input_index
            .checked_add(first_coarse)
            .and_then(|index| index.checked_add(fine_offset))
            .ok_or("fine channel input clock overflow")?;
        let count = unsafe {
            channel_process_float(
                self.handle,
                self.scratch.as_ptr(),
                groups as i32,
                self.output.as_mut_ptr(),
                (self.output.len() / 2) as i32,
            )
        };
        if count < 0 {
            return Err("fine channel process failed");
        }
        self.expected_seq += 1;
        Ok(FineOutput {
            channel_id: self.id,
            epoch: self.epoch,
            bin: self.bin,
            seq: packet.seq,
            stamp: packet.stamp,
            input_index,
            output_index,
            output_rate: self.output_rate,
            center_offset_hz: self.center_offset_hz,
            delay_samples: self.delay_samples,
            iq: &self.output[..2 * count as usize],
        })
    }
}

impl Drop for FineChannel {
    fn drop(&mut self) {
        unsafe { channel_close(self.handle) };
    }
}

/// OS2 half-bandwidth limits with less than 0.25 dB prototype attenuation.
pub fn coarse_passband_hz(coarse_rate: f64, taps: usize) -> Option<f64> {
    if !coarse_rate.is_finite() || coarse_rate <= 0.0 {
        return None;
    }
    let reference = match taps {
        16 => 430_000.0,
        8 => 380_000.0,
        4 => 275_000.0,
        _ => return None,
    };
    Some(reference * coarse_rate / 976_562.5)
}

/// Measured fine processing and framing cost, indexed by decimation 2 through 24.
/// Costs are nanoseconds per coarse sample for one channel.
#[derive(Clone, Debug)]
pub struct FineCostBudget {
    pub ns_per_coarse_sample: [f64; 23],
    pub reserve: f64,
    pub limit_ns_per_second: f64,
}

impl FineCostBudget {
    pub fn new(ns_per_coarse_sample: [f64; 23], reserve: f64, limit_ns_per_second: f64) -> Option<Self> {
        if ns_per_coarse_sample.iter().any(|cost| !cost.is_finite() || *cost <= 0.0)
            || !reserve.is_finite() || reserve <= 0.0
            || !limit_ns_per_second.is_finite() || limit_ns_per_second <= 0.0 {
            return None;
        }
        Some(Self { ns_per_coarse_sample, reserve, limit_ns_per_second })
    }

    pub fn required_ns_per_second(&self, coarse_rate: f64, decimations: impl IntoIterator<Item = i32>) -> Result<f64, &'static str> {
        if !coarse_rate.is_finite() || coarse_rate <= 0.0
            || self.ns_per_coarse_sample.iter().any(|cost| !cost.is_finite() || *cost <= 0.0)
            || !self.reserve.is_finite() || self.reserve <= 0.0
            || !self.limit_ns_per_second.is_finite() || self.limit_ns_per_second <= 0.0 {
            return Err("fine cost budget invalid");
        }
        let mut cost = 0.0;
        for decimate in decimations {
            if !(2..=24).contains(&decimate) { return Err("fine cost budget has no calibration for decimation"); }
            cost += coarse_rate * self.ns_per_coarse_sample[(decimate - 2) as usize];
        }
        let required = cost * self.reserve;
        if !required.is_finite() || required < 0.0 { return Err("fine cost budget demand invalid"); }
        Ok(required)
    }

    pub fn check(&self, coarse_rate: f64, decimations: impl IntoIterator<Item = i32>) -> Result<(), &'static str> {
        if self.required_ns_per_second(coarse_rate, decimations)? > self.limit_ns_per_second {
            Err("fine cost budget exceeded")
        } else { Ok(()) }
    }
}

/// Ordered, synchronous subscribers over a provisioned coarse-bin plan.
/// Budget is supplied by the caller from measurements for this exact plan/load.
pub struct FineBank {
    channels: Vec<FineChannel>,
    bins: Vec<u32>,
    epoch: u64,
    next_seq: u64,
    coarse_rate: f64,
    coarse_delay: f64,
    bin_count: usize,
    usable_half_bandwidth: f64,
    flat_half_bandwidth: f64,
    max_groups: usize,
    channel_budget: usize,
    output_rate_budget: f64,
    output_rate_used: f64,
    cost_budget: Option<FineCostBudget>,
    delivery_budget: Duration,
    last_id: Option<u64>,
    failed: bool,
}

pub enum ChannelAction {
    Subscribe {
        id: u64,
        bin: u32,
        offset_hz: f64,
        decimate: i32,
    },
    Unsubscribe {
        id: u64,
    },
    Retune {
        old_id: u64,
        id: u64,
        bin: u32,
        offset_hz: f64,
        decimate: i32,
    },
}

pub struct ChannelCommand {
    pub epoch: u64,
    pub action: ChannelAction,
}

/// Bounded command ingress; replies carry first ordered packet sequence on acceptance.
pub struct ControlRequest {
    pub command: ChannelCommand,
    pub reply: SyncSender<Result<u64, &'static str>>,
}

pub fn control_channel() -> (SyncSender<ControlRequest>, Receiver<ControlRequest>) {
    sync_channel(2)
}

impl FineBank {
    pub fn new(
        stream: &PfbStream,
        usable_bandwidth: f64,
        max_groups: usize,
        channel_budget: usize,
        output_rate_budget: f64,
        delivery_budget: Duration,
    ) -> Option<Self> {
        let input_rate = stream.input_sample_rate();
        if !usable_bandwidth.is_finite()
            || stream.epoch() == 0
            || stream.epoch() > u32::MAX as u64
            || usable_bandwidth <= 0.0
            || usable_bandwidth > input_rate
            || max_groups == 0
            || channel_budget == 0
            || !output_rate_budget.is_finite()
            || output_rate_budget <= 0.0
            || delivery_budget.is_zero()
        {
            return None;
        }
        Some(Self {
            channels: Vec::new(),
            bins: stream.bins().to_vec(),
            epoch: stream.epoch(),
            next_seq: 0,
            coarse_rate: stream.sample_rate,
            coarse_delay: stream.delay_samples,
            bin_count: stream.bin_count(),
            usable_half_bandwidth: usable_bandwidth / 2.0,
            flat_half_bandwidth: coarse_passband_hz(stream.sample_rate, stream.taps_per_phase())?,
            max_groups,
            channel_budget,
            output_rate_budget,
            output_rate_used: 0.0,
            cost_budget: None,
            delivery_budget,
            last_id: None,
            failed: false,
        })
    }

    /// Enables measured admission without changing budgets for other library consumers.
    /// A rejected budget leaves the existing configuration unchanged.
    pub fn set_cost_budget(&mut self, budget: FineCostBudget) -> Result<(), &'static str> {
        if self.channels.iter().any(|channel| channel.planned) {
            return Err("legacy decimation costs do not calibrate a bandwidth-aware plan");
        }
        budget.check(self.coarse_rate, self.channels.iter().map(|channel| channel.decimate as i32))?;
        self.cost_budget = Some(budget);
        Ok(())
    }

    /// Checks an add or replacement before draining or changing a caller's stream.
    pub fn check_admission(&self, replacing: Option<u64>, decimate: i32) -> Result<(), &'static str> {
        if self.failed { return Err("fine bank failed"); }
        let old = replacing.map(|id| self.channels.iter().find(|channel| channel.id == id)
            .ok_or("fine channel not active")).transpose()?;
        if old.is_none() && self.channels.len() >= self.channel_budget {
            return Err("fine channel budget exceeded");
        }
        if decimate <= 0 { return Err("fine channel configuration rejected"); }
        self.check_rate(self.output_rate_used - old.map_or(0.0, |channel| channel.output_rate)
            + self.coarse_rate / decimate as f64)?;
        if let Some(budget) = &self.cost_budget {
            budget.check(self.coarse_rate, self.channels.iter()
                .filter(|channel| Some(channel.id) != replacing)
                .map(|channel| channel.decimate as i32).chain(std::iter::once(decimate)))?;
        }
        Ok(())
    }

    /// Takes effect at the next ordered delivery boundary, including already-submitted IQ.
    pub fn subscribe(
        &mut self,
        id: u64,
        bin: u32,
        offset_hz: f64,
        decimate: i32,
    ) -> Result<(), &'static str> {
        self.check_admission(None, decimate)?;
        self.check_id(id)?;
        let channel = self.prepare(id, bin, offset_hz, decimate)?;
        let total = self.output_rate_used + channel.output_rate;
        self.check_rate(total)?;
        self.channels.push(channel);
        self.output_rate_used = total;
        self.last_id = Some(id);
        Ok(())
    }

    /// Failed retunes keep the old channel, its FIR history and its delivery clock.
    pub fn retune(
        &mut self,
        old_id: u64,
        id: u64,
        bin: u32,
        offset_hz: f64,
        decimate: i32,
    ) -> Result<(), &'static str> {
        if self.failed {
            return Err("fine bank failed");
        }
        let at = self
            .channels
            .iter()
            .position(|ch| ch.id == old_id)
            .ok_or("fine channel not active")?;
        self.check_admission(Some(old_id), decimate)?;
        self.check_id(id)?;
        let channel = self.prepare(id, bin, offset_hz, decimate)?;
        let total = self.output_rate_used - self.channels[at].output_rate + channel.output_rate;
        self.check_rate(total)?;
        self.channels[at] = channel;
        self.output_rate_used = total;
        self.last_id = Some(id);
        Ok(())
    }

    /// Installs one independently clocked, bandwidth-aware cascade at the next packet boundary.
    pub fn subscribe_planned(&mut self, id: u64, bin: u32, offset_hz: f64, plan: RatePlan) -> Result<(), &'static str> {
        self.check_planned_admission(None, &plan)?;
        self.check_id(id)?;
        let channel = self.prepare_planned(id, bin, offset_hz, &plan)?;
        self.output_rate_used += channel.output_rate;
        self.channels.push(channel);
        self.last_id = Some(id);
        Ok(())
    }

    /// Replaces only the requested channel; all other phases, histories and clocks survive.
    pub fn retune_planned(&mut self, old_id: u64, id: u64, bin: u32, offset_hz: f64, plan: RatePlan) -> Result<(), &'static str> {
        self.check_planned_admission(Some(old_id), &plan)?;
        self.check_id(id)?;
        let at = self.channels.iter().position(|channel| channel.id == old_id)
            .ok_or("fine channel not active")?;
        let channel = self.prepare_planned(id, bin, offset_hz, &plan)?;
        self.output_rate_used += channel.output_rate - self.channels[at].output_rate;
        self.channels[at] = channel;
        self.last_id = Some(id);
        Ok(())
    }

    pub fn check_planned_admission(&self, replacing: Option<u64>, plan: &RatePlan) -> Result<(), &'static str> {
        if !plan.valid_for(self.coarse_rate) { return Err("fine rate plan invalid"); }
        if self.cost_budget.is_some() { return Err("legacy decimation costs do not calibrate a bandwidth-aware plan"); }
        self.check_admission(replacing, plan.decimate)
    }

    /// Caller applies commands between ordered packet deliveries.
    pub fn apply(&mut self, command: ChannelCommand) -> Result<(), &'static str> {
        if command.epoch != self.epoch {
            return Err("fine bank command epoch mismatch");
        }
        if self.failed {
            return Err("fine bank failed");
        }
        match command.action {
            ChannelAction::Subscribe {
                id,
                bin,
                offset_hz,
                decimate,
            } => self.subscribe(id, bin, offset_hz, decimate),
            ChannelAction::Unsubscribe { id } => {
                if self.unsubscribe(id) {
                    Ok(())
                } else {
                    Err("fine channel not active")
                }
            }
            ChannelAction::Retune {
                old_id,
                id,
                bin,
                offset_hz,
                decimate,
            } => self.retune(old_id, id, bin, offset_hz, decimate),
        }
    }

    fn check_id(&self, id: u64) -> Result<(), &'static str> {
        if id == 0 || id > u32::MAX as u64 {
            return Err("fine channel id must be a nonzero u32");
        }
        if self.last_id.is_some_and(|last| id <= last) {
            Err("fine channel id must increase within epoch")
        } else {
            Ok(())
        }
    }

    fn check_rate(&self, total: f64) -> Result<(), &'static str> {
        if total > self.output_rate_budget + self.output_rate_budget * 1e-12 {
            Err("fine output rate budget exceeded")
        } else {
            Ok(())
        }
    }

    fn prepare(
        &self,
        id: u64,
        bin: u32,
        offset_hz: f64,
        decimate: i32,
    ) -> Result<FineChannel, &'static str> {
        self.prepare_with_plan(id, bin, offset_hz, decimate, None)
    }

    fn prepare_planned(&self, id: u64, bin: u32, offset_hz: f64, plan: &RatePlan) -> Result<FineChannel, &'static str> {
        self.prepare_with_plan(id, bin, offset_hz, plan.decimate, Some(plan))
    }

    fn prepare_with_plan(&self, id: u64, bin: u32, offset_hz: f64, decimate: i32, plan: Option<&RatePlan>) -> Result<FineChannel, &'static str> {
        if bin as usize >= self.bin_count {
            return Err("coarse bin not provisioned");
        }
        if !offset_hz.is_finite() {
            return Err("fine frequency outside nearest coarse bin");
        }
        if decimate <= 0 {
            return Err("fine channel configuration rejected");
        }
        let signed_bin = if bin as usize >= self.bin_count / 2 {
            bin as i64 - self.bin_count as i64
        } else {
            bin as i64
        };
        let tuned_hz = signed_bin as f64 * self.coarse_rate / 2.0 - offset_hz;
        let passband = plan.map_or(self.coarse_rate / (2.0 * decimate as f64), |plan| plan.occupied_bandwidth_hz / 2.0);
        if tuned_hz.abs() + passband > self.usable_half_bandwidth
        {
            return Err("fine channel outside usable IQ bandwidth");
        }
        let spacing = self.coarse_rate / 2.0;
        let nearest = (tuned_hz / spacing).round() as i64;
        if nearest < -(self.bin_count as i64) / 2 || nearest > self.bin_count as i64 / 2 {
            return Err("fine channel outside usable IQ bandwidth");
        }
        let bin = nearest.rem_euclid(self.bin_count as i64) as u32;
        if !self.bins.contains(&bin) {
            return Err("coarse bin not provisioned");
        }
        let offset_hz = nearest as f64 * spacing - tuned_hz;
        if offset_hz.abs() + passband
            > self.flat_half_bandwidth
        {
            return Err("fine channel outside coarse passband");
        }
        let spec = FineSpec {
            id,
            epoch: self.epoch,
            first_seq: self.next_seq,
            bin,
            offset_hz,
            decimate,
            coarse_delay_samples: self.coarse_delay,
            fused_fir: true,
        };
        let mut channel = FineChannel::new_with_plan(
            spec, &self.bins, self.coarse_rate, self.bin_count / 2, self.max_groups, plan,
        ).ok_or("fine channel configuration rejected")?;
        channel.center_offset_hz = Some(tuned_hz);
        Ok(channel)
    }

    pub fn unsubscribe(&mut self, id: u64) -> bool {
        if let Some(at) = self.channels.iter().position(|channel| channel.id == id) {
            let removed = self.channels.remove(at);
            self.output_rate_used = (self.output_rate_used - removed.output_rate).max(0.0);
            true
        } else {
            false
        }
    }

    /// Output rate and group delay in output samples for an active channel.
    pub fn channel_metadata(&self, id: u64) -> Option<(f64, f64)> {
        self.channels.iter().find(|channel| channel.id == id)
            .map(|channel| (channel.output_rate, channel.delay_samples))
    }

    /// Native kernel clocks, sampled only at an ordered control/stop boundary.
    /// Values are (channel ID, consumed coarse samples, produced fine samples).
    pub fn clocks(&self) -> Result<Vec<(u64, u64, u64)>, &'static str> {
        self.channels.iter().map(|channel| {
            let (mut input, mut output) = (0, 0);
            if unsafe { channel_clock(channel.handle, &mut input, &mut output) } != 0 {
                return Err("fine channel clock failed");
            }
            Ok((channel.id, input, output))
        }).collect()
    }

    /// Updates only column selection after a drained shared-PFB bin replacement.
    /// All active bins must remain selected; phases, histories and clocks are untouched.
    pub fn remap_bins(&mut self, bins: &[u32]) -> Result<(), &'static str> {
        if bins.is_empty() || bins.iter().enumerate().any(|(at, &bin)|
            bin as usize >= self.bin_count || bins[..at].contains(&bin)) {
            return Err("invalid coarse bin selection");
        }
        if self.channels.iter().any(|channel| !bins.contains(&channel.bin)) {
            return Err("active fine channel missing from coarse selection");
        }
        for channel in &mut self.channels {
            channel.column = bins.iter().position(|&bin| bin == channel.bin).unwrap();
            channel.columns = bins.len();
        }
        self.bins.clear();
        self.bins.extend_from_slice(bins);
        Ok(())
    }

    /// Sequence of the next packet accepted by every active channel.
    pub fn next_sequence(&self) -> u64 {
        self.next_seq
    }

    /// Callback must consume borrowed IQ immediately. Slow/erroring consumers fail the stream;
    /// no packet is skipped, retried partially or queued without bound.
    pub fn deliver(
        &mut self,
        packet: &PfbOutput,
        mut consume: impl FnMut(&FineOutput<'_>) -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        if self.failed {
            return Err("fine bank failed");
        }
        if packet.epoch != self.epoch || packet.seq != self.next_seq {
            return Err("fine bank epoch/sequence mismatch");
        }
        let started = Instant::now();
        for channel in &mut self.channels {
            let result = channel.process(packet).and_then(|output| consume(&output));
            if let Err(error) = result {
                self.failed = true;
                return Err(error);
            }
            if started.elapsed() > self.delivery_budget {
                self.failed = true;
                return Err("fine consumer over budget");
            }
        }
        self.next_seq += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pfb_pool::PfbStream;
    use std::f64::consts::PI;

    #[test]
    fn planned_retune_and_bin_remap_preserve_unaffected_filter_and_clock() {
        let stream = PfbStream::new(62_500_000.0, 16240, vec![0, 1], 1).unwrap();
        let rate = stream.sample_rate;
        let plan = RatePlan::new(rate, "am", 6_000.0).unwrap();
        let make = || FineBank::new(&stream, 50e6, 255, 3, 2e6, Duration::from_secs(10)).unwrap();
        let mut bank = make();
        let mut reference = make();
        bank.subscribe_planned(1, 0, 0.0, plan.clone()).unwrap();
        bank.subscribe_planned(2, 1, 0.0, plan.clone()).unwrap();
        reference.subscribe_planned(1, 0, 0.0, plan.clone()).unwrap();
        let mut raw = 0;
        for seq in 0..8 {
            if seq == 3 {
                bank.retune_planned(2, 3, 1, -1_000.0,
                    RatePlan::new(rate, "cw", 2_000.0).unwrap()).unwrap();
                assert!(bank.remap_bins(&[1]).is_err());
                bank.remap_bins(&[1, 0]).unwrap();
            }
            let target: Vec<f32> = (0..255).flat_map(|sample| {
                let phase = 2.0 * PI * 1_100.0 * (seq * 255 + sample) as f64 / rate;
                [phase.cos() as f32, phase.sin() as f32]
            }).collect();
            let iq: Vec<f32> = target.chunks_exact(2).flat_map(|pair| {
                if seq < 3 { [pair[0], pair[1], 0.0, 0.0] }
                else { [0.0, 0.0, pair[0], pair[1]] }
            }).collect();
            let packet = PfbOutput { epoch: 1, seq: seq as u64, stamp: 0,
                input_index: raw, groups: 255, process_ns: 0, iq };
            let mut actual = None;
            bank.deliver(&packet, |output| {
                if output.channel_id == 1 { actual = Some((output.input_index, output.output_index, output.iq.to_vec())); }
                Ok(())
            }).unwrap();
            let mut expected = None;
            // The reference keeps its initial selection and untouched channel.
            let packet = PfbOutput { iq: target.chunks_exact(2).flat_map(|pair|
                [pair[0], pair[1], 0.0, 0.0]).collect(), ..packet };
            reference.deliver(&packet, |output| {
                expected = Some((output.input_index, output.output_index, output.iq.to_vec())); Ok(())
            }).unwrap();
            assert_eq!(actual, expected);
            raw += 255 * 64;
        }
        assert_eq!(bank.clocks().unwrap()[0], reference.clocks().unwrap()[0]);
    }

    #[test]
    fn planned_narrow_channel_has_exact_causal_indices_through_empty_packets() {
        let plan = RatePlan::new(976_562.5, "cw", 500.0).unwrap();
        let decimate = plan.decimate as u64;
        let mut channel = FineChannel::new_planned(FineSpec {
            id: 1, epoch: 1, first_seq: 0, bin: 0, offset_hz: 0.0,
            decimate: plan.decimate, coarse_delay_samples: 0.0, fused_fir: true,
        }, &[0], 976_562.5, 64, 4096, &plan).unwrap();
        let mut coarse = 0;
        let mut produced = 0;
        for (seq, groups) in [0, 1, 17, 255, 4093, 0, 19, 4096].into_iter().enumerate() {
            let packet = PfbOutput { epoch: 1, seq: seq as u64, stamp: 0,
                input_index: coarse * 64, groups, process_ns: 0, iq: vec![0.0; 2 * groups as usize] };
            let output = channel.process(&packet).unwrap();
            assert_eq!(output.input_index, (produced + 1) * decimate * 64 - 1);
            assert_eq!(output.output_index, produced);
            produced += (output.iq.len() / 2) as u64;
            coarse += groups as u64;
            assert_eq!(produced, coarse / decimate);
        }
    }

    #[test]
    fn selected_bin_fine_tune_preserves_phase_and_clock() {
        let fs = 62_500_000.0;
        let bins = [127, 5, 0];
        let mut stream = PfbStream::new(fs, 16240, bins.to_vec(), 11).unwrap();
        let mut channel = FineChannel::new(
            FineSpec {
                id: 73,
                epoch: 11,
                first_seq: 0,
                bin: 5,
                offset_hz: -10_000.0,
                decimate: 12,
                coarse_delay_samples: stream.delay_samples,
                fused_fir: true,
            },
            &bins,
            stream.sample_rate,
            stream.bin_count() / 2,
            255,
        )
        .unwrap();
        let mut seq_seen = 0;
        let mut input_clock = 0;
        let mut fine_clock = 0;
        let mut late_magnitudes = Vec::new();
        let mut consume = |stream: &mut PfbStream| {
            while let Some(packet) = stream.take() {
                assert_eq!(packet.seq, seq_seen);
                let result = channel.process(&packet).unwrap();
                assert_eq!(
                    (result.channel_id, result.epoch, result.bin, result.stamp),
                    (73, 11, 5, 1000 + seq_seen * 32480)
                );
                assert_eq!(result.input_index, (fine_clock + 1) * 12 * 64 - 1);
                assert_eq!(result.output_index, fine_clock);
                fine_clock += (result.iq.len() / 2) as u64;
                input_clock += packet.groups as u64;
                if seq_seen > 3 {
                    late_magnitudes.extend(result.iq.chunks_exact(2).map(|z| z[0].hypot(z[1])));
                }
                seq_seen += 1;
            }
        };
        // Bin 5 tone at +10k within coarse bin; fine mixer moves it to DC.
        for seq in 0..16 {
            let iq: Vec<i16> = (0..16240)
                .flat_map(|sample| {
                    let time = (seq * 16240 + sample) as f64 / fs;
                    let phase = 2.0 * PI * (5.0 * fs / 128.0 + 10_000.0) * time;
                    [
                        (phase.cos() * 12000.0) as i16,
                        (phase.sin() * 12000.0) as i16,
                    ]
                })
                .collect();
            loop {
                match stream.submit(seq as u64, 1000 + seq as u64 * 32480, &iq) {
                    Ok(()) => break,
                    Err(-2) => consume(&mut stream),
                    err => panic!("PFB submit: {err:?}"),
                }
            }
            consume(&mut stream);
        }
        stream.finish().unwrap();
        consume(&mut stream);
        drop(consume);
        assert_eq!(seq_seen, 16);
        assert!(
            (channel.delay_samples
                - unsafe { channel_delay(channel.handle) }
                - stream.delay_samples / 12.0)
                .abs()
                < 1e-12
        );
        assert!(fine_clock > 300);
        let (mut fine_in, mut fine_out) = (0, 0);
        unsafe { channel_clock(channel.handle, &mut fine_in, &mut fine_out) };
        assert_eq!((fine_in, fine_out), (input_clock, fine_clock));
        assert!(late_magnitudes.iter().all(|&v| v.is_finite()));
        assert!(
            late_magnitudes.iter().copied().sum::<f32>() / late_magnitudes.len() as f32 > 100.0
        );
        let bad = PfbOutput {
            seq: 16,
            epoch: 12,
            stamp: 0,
            input_index: 0,
            groups: 0,
            process_ns: 0,
            iq: vec![],
        };
        assert!(channel.process(&bad).is_err());
    }

    #[test]
    fn new_subscription_starts_at_requested_seq() {
        let mut channel = FineChannel::new(
            FineSpec {
                id: 17,
                epoch: 9,
                first_seq: 42,
                bin: 5,
                offset_hz: 0.0,
                decimate: 12,
                coarse_delay_samples: 0.0,
                fused_fir: false,
            },
            &[5],
            976_562.5,
            64,
            255,
        )
        .unwrap();
        let packet = PfbOutput {
            seq: 42,
            epoch: 9,
            stamp: 123,
            input_index: 3_000_000,
            groups: 0,
            process_ns: 0,
            iq: vec![],
        };
        let result = channel.process(&packet).unwrap();
        assert_eq!(
            (
                result.channel_id,
                result.seq,
                result.epoch,
                result.input_index,
                result.output_index
            ),
            (17, 42, 9, 3_000_767, 0)
        );
        assert!(channel.process(&packet).is_err());
        let wrong_epoch = PfbOutput {
            seq: 43,
            epoch: 10,
            ..packet
        };
        assert!(channel.process(&wrong_epoch).is_err());
    }

    #[test]
    fn input_clock_tracks_pfb_hop_and_fine_phase_through_empty_outputs() {
        for hop in [32, 64] {
            for fused_fir in [false, true] {
                let mut channel = FineChannel::new(
                    FineSpec {
                        id: 1,
                        epoch: 1,
                        first_seq: 0,
                        bin: 0,
                        offset_hz: 0.0,
                        decimate: 12,
                        coarse_delay_samples: 0.0,
                        fused_fir,
                    },
                    &[0], 976_562.5, hop, 64,
                ).unwrap();
                let mut raw_index = (10 * hop + 7) as u64;
                let mut produced = 0;
                for (seq, samples) in [0, 1, hop - 8, 5 * hop + 3, 6 * hop - 3, 13 * hop + 5, 2 * hop]
                    .into_iter().enumerate()
                {
                    let groups = (raw_index % hop as u64 + samples as u64) / hop as u64;
                    let packet = PfbOutput {
                        seq: seq as u64,
                        epoch: 1,
                        stamp: 0,
                        input_index: raw_index,
                        groups: groups as u32,
                        process_ns: 0,
                        iq: vec![0.0; 2 * groups as usize],
                    };
                    let output = channel.process(&packet).unwrap();
                    // The subscription starts after ten complete PFB hops.
                    assert_eq!(output.input_index, (10 + (produced + 1) * 12) * hop as u64 - 1);
                    assert_eq!(output.output_index, produced);
                    produced += (output.iq.len() / 2) as u64;
                    raw_index += samples as u64;
                    assert_eq!(produced, (raw_index / hop as u64 - 10) / 12);
                }
            }
        }
    }

    #[test]
    fn two_fine_channels_share_bin_with_independent_phase_and_start() {
        let fs = 976_562.5;
        let make = |id, first_seq, offset_hz| {
            FineChannel::new(
                FineSpec {
                    id,
                    epoch: 3,
                    first_seq,
                    bin: 5,
                    offset_hz,
                    decimate: 12,
                    coarse_delay_samples: 16.0,
                    fused_fir: true,
                },
                &[5],
                fs,
                64,
                192,
            )
            .unwrap()
        };
        let mut first = make(1, 0, 0.0);
        let mut second = make(2, 2, -10_000.0);
        for seq in 0..4u64 {
            let iq: Vec<f32> = (0..192)
                .flat_map(|j| {
                    let phase = 2.0 * PI * 10_000.0 * (seq * 192 + j) as f64 / fs;
                    [1200.0 * phase.cos() as f32, 1200.0 * phase.sin() as f32]
                })
                .collect();
            let packet = PfbOutput {
                seq,
                epoch: 3,
                stamp: 1000 + seq * 384,
                input_index: seq * 192 * 64,
                groups: 192,
                process_ns: 0,
                iq,
            };
            let original = first.process(&packet).unwrap();
            assert_eq!(
                (
                    original.channel_id,
                    original.output_index,
                    original.iq.len()
                ),
                (1, seq * 16, 32)
            );
            if seq < 2 {
                assert!(second.process(&packet).is_err());
                continue;
            }
            let shifted = second.process(&packet).unwrap();
            assert_eq!(
                (shifted.channel_id, shifted.output_index, shifted.iq.len()),
                (2, (seq - 2) * 16, 32)
            );
            if seq == 3 {
                use rustfft::num_complex::Complex32;
                let last_pair = |iq: &[f32]| {
                    let a = Complex32::new(iq[28], iq[29]);
                    let b = Complex32::new(iq[30], iq[31]);
                    (b * a.conj()).arg()
                };
                assert!(
                    (last_pair(original.iq) - (2.0 * PI * 10_000.0 * 12.0 / fs) as f32).abs()
                        < 0.03
                );
                assert!(last_pair(shifted.iq).abs() < 0.03);
            }
        }
    }

    #[test]
    fn bank_subscribes_at_delivery_boundary_and_fails_closed() {
        let stream = PfbStream::new(62_500_000.0, 16240, vec![5, 127], 8).unwrap();
        let mut bank = FineBank::new(
            &stream,
            50_000_000.0,
            255,
            2,
            stream.sample_rate * 2.0 / 12.0,
            Duration::from_secs(1),
        )
        .unwrap();
        assert!(bank.subscribe(0, 5, 0.0, 12).is_err());
        assert!(bank.subscribe(u32::MAX as u64 + 1, 5, 0.0, 12).is_err());
        assert_eq!(bank.channel_metadata(1), None);
        assert_eq!(
            bank.subscribe(1, 6, 0.0, 12),
            Err("coarse bin not provisioned")
        );
        assert!(bank.subscribe(1, 5, 0.0, 12).is_ok());
        let metadata = bank.channel_metadata(1).unwrap();
        assert_eq!(metadata.0, stream.sample_rate / 12.0);
        assert!(metadata.1 > stream.delay_samples / 12.0);
        assert_eq!(
            bank.subscribe(1, 5, 0.0, 12),
            Err("fine channel id must increase within epoch")
        );
        assert!(bank.subscribe(2, 5, -10_000.0, 12).is_ok());
        assert_eq!(
            bank.subscribe(3, 127, 0.0, 12),
            Err("fine channel budget exceeded")
        );
        let packet = |seq, epoch| PfbOutput {
            seq,
            epoch,
            stamp: 1000 + seq * 32480,
            input_index: seq * 16240,
            groups: 253,
            process_ns: 0,
            iq: vec![0.0; 2 * 253 * 2],
        };
        assert_eq!(
            bank.deliver(&packet(0, 9), |_| Ok(())),
            Err("fine bank epoch/sequence mismatch")
        );
        let mut seen = Vec::new();
        bank.deliver(&packet(0, 8), |out| {
            seen.push((out.channel_id, out.seq, out.output_index));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [(1, 0, 0), (2, 0, 0)]);
        assert!(bank.unsubscribe(1));
        assert_eq!(bank.channel_metadata(1), None);
        assert!(!bank.unsubscribe(1));
        assert_eq!(
            bank.subscribe(1, 127, 0.0, 12),
            Err("fine channel id must increase within epoch")
        );
        assert!(bank.subscribe(3, 127, 0.0, 12).is_ok());
        seen.clear();
        bank.deliver(&packet(1, 8), |out| {
            seen.push((out.channel_id, out.seq, out.output_index));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [(2, 1, 21), (3, 1, 0)]);
        assert_eq!(
            bank.deliver(&packet(1, 8), |_| Ok(())),
            Err("fine bank epoch/sequence mismatch")
        );

        let new_stream = PfbStream::new(62_500_000.0, 16240, vec![5], 9).unwrap();
        let mut next = FineBank::new(
            &new_stream,
            50_000_000.0,
            255,
            1,
            new_stream.sample_rate / 12.0,
            Duration::from_micros(1),
        )
        .unwrap();
        next.subscribe(3, 5, 0.0, 12).unwrap();
        assert_eq!(
            next.deliver(&packet(0, 8), |_| Ok(())),
            Err("fine bank epoch/sequence mismatch")
        );
        assert_eq!(
            next.deliver(
                &PfbOutput {
                    iq: vec![0.0; 2 * 253],
                    ..packet(0, 9)
                },
                |_| {
                    std::thread::sleep(Duration::from_millis(1));
                    Ok(())
                }
            ),
            Err("fine consumer over budget")
        );
        assert_eq!(
            next.deliver(&packet(1, 9), |_| Ok(())),
            Err("fine bank failed")
        );
    }

    #[test]
    fn bank_joins_already_submitted_ordered_pfb_output() {
        let mut stream = PfbStream::new(62_500_000.0, 16240, vec![5], 12).unwrap();
        let mut bank = FineBank::new(
            &stream,
            50_000_000.0,
            255,
            2,
            stream.sample_rate * 2.0 / 12.0,
            Duration::from_secs(1),
        )
        .unwrap();
        bank.subscribe(1, 5, 0.0, 12).unwrap();
        let iq = vec![0i16; 2 * 16240];
        for seq in 0..3u64 {
            stream.submit(seq, 100 + seq * 32480, &iq).unwrap();
        }
        stream.flush_pending().unwrap();
        let first = stream.take().unwrap();
        let mut seen = Vec::new();
        bank.deliver(&first, |out| {
            seen.push((out.channel_id, out.seq));
            Ok(())
        })
        .unwrap();
        // Drain an ordered output before submitting beyond the bounded backlog.
        stream.submit(3, 100 + 3 * 32480, &iq).unwrap();
        stream.finish().unwrap();
        bank.subscribe(2, 5, 10_000.0, 12).unwrap();
        let second = stream.take().unwrap();
        bank.deliver(&second, |out| {
            seen.push((out.channel_id, out.seq));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [(1, 0), (1, 1), (2, 1)]);
        assert_eq!(
            bank.subscribe(3, 5, 0.0, 12),
            Err("fine channel budget exceeded")
        );
        bank.apply(ChannelCommand {
            epoch: 12,
            action: ChannelAction::Retune {
                old_id: 1,
                id: 3,
                bin: 5,
                offset_hz: -10_000.0,
                decimate: 12,
            },
        })
        .unwrap();
        let third = stream.take().unwrap();
        bank.deliver(&third, |out| {
            seen.push((out.channel_id, out.seq));
            assert_eq!(out.output_index, if out.channel_id == 3 { 0 } else { 21 });
            Ok(())
        })
        .unwrap();
        assert!(bank.unsubscribe(2));
        bank.subscribe(4, 5, 0.0, 12).unwrap();
        bank.deliver(&stream.take().unwrap(), |out| {
            seen.push((out.channel_id, out.seq));
            assert_eq!(out.output_index, if out.channel_id == 4 { 0 } else { 21 });
            Ok(())
        })
        .unwrap();
        assert_eq!(
            seen,
            [(1, 0), (1, 1), (2, 1), (3, 2), (2, 2), (3, 3), (4, 3)]
        );
        assert!(stream.stats().max_pending <= crate::pfb_pool::MAX_IN_FLIGHT as u32);
    }
    #[test]
    fn measured_cost_admission_is_transactional_and_releases_capacity() {
        let stream = PfbStream::new(62_500_000.0, 16240, vec![0], 81).unwrap();
        let rate = stream.sample_rate;
        let make = |count, output| FineBank::new(&stream, 50e6, 255, count, output,
            Duration::from_secs(1)).unwrap();
        let mut count = make(1, rate);
        count.subscribe(1, 0, 0.0, 12).unwrap();
        assert_eq!(count.subscribe(2, 0, 0.0, 12), Err("fine channel budget exceeded"));
        let mut output = make(3, rate / 12.0);
        output.subscribe(1, 0, 0.0, 12).unwrap();
        assert_eq!(output.subscribe(2, 0, 0.0, 12), Err("fine output rate budget exceeded"));
        assert_eq!(output.retune(1, 2, 0, 0.0, 6), Err("fine output rate budget exceeded"));
        assert_eq!(output.clocks().unwrap(), [(1, 0, 0)]);

        let mut costs = [1.0; 23];
        costs[4] = 3.0; // d6 costs more than two d12 channels.
        let budget = FineCostBudget::new(costs, 2.0, rate * 4.0).unwrap();
        let mut bank = make(4, rate);
        bank.set_cost_budget(budget).unwrap();
        bank.subscribe(1, 0, 0.0, 12).unwrap();
        bank.subscribe(2, 0, 0.0, 12).unwrap(); // Equality is admitted.
        let packet = PfbOutput { seq:0, epoch:81, stamp:0, input_index:0, groups:253,
            process_ns:0, iq:vec![1200.0; 506] };
        bank.deliver(&packet, |_| Ok(())).unwrap();
        let before = bank.clocks().unwrap();
        assert_eq!(bank.subscribe(3, 0, 0.0, 12), Err("fine cost budget exceeded"));
        assert_eq!(bank.retune(1, 3, 0, 0.0, 6), Err("fine cost budget exceeded"));
        assert_eq!(bank.clocks().unwrap(), before);
        assert_eq!(bank.next_sequence(), 1);
        assert_eq!(bank.last_id, Some(2));
        assert_eq!(bank.channel_metadata(3), None);
        let too_small = FineCostBudget::new(costs, 2.0, rate).unwrap();
        assert_eq!(bank.set_cost_budget(too_small), Err("fine cost budget exceeded"));
        assert!(bank.unsubscribe(2));
        bank.subscribe(3, 0, 0.0, 12).unwrap();
        bank.retune(1, 4, 0, 1000.0, 12).unwrap();
        assert_eq!(bank.clocks().unwrap(), [(4, 0, 0), (3, 0, 0)]);
        assert!(FineCostBudget::new([0.0; 23], 2.0, rate).is_none());
        assert!(FineCostBudget::new([f64::NAN; 23], 2.0, rate).is_none());
    }
}

#[cfg(test)]
mod am_audio_tests;
#[cfg(test)]
mod lifecycle_tests;
