//! RTA profile transactions and applied trigger metadata.
//! DevState retains ownership; SDK acceptance precedes confirmed trigger changes.
use super::{hardware_rta, DevState, RtaParams, RtaTrigger, SdkFailure, LINK_LOST_STATUS};
use serde_json::{json, Value};
use std::time::Duration;

impl DevState {
    pub(super) fn rta_params(&self) -> RtaParams {
        RtaParams {
            center: self.rta_center,
            decimate: self.rta_decimate,
            front_end: self.front_end(self.rta_ref),
            rbw_manual: self.rta_rbw_mode == "manual",
            rbw: self.rta_rbw,
            vbw_mode: self.rta_vbw_mode.clone(),
            vbw: self.rta_vbw,
            sweep_time_mode: self.rta_sweep_time_mode,
            sweep_time: self.rta_sweep_time,
            trigger: self.trigger.clone(),
        }
    }

    /// Apply a profile through the mode owner, retaining requests on rejection.
    pub(super) fn configure_rta(&mut self) -> Result<(), String> {
        if self.rta_hw.is_none() {
            if self.hardware_shim.is_some() { return Err("RTA hardware is not ready".into()); }
            self.invalidate_observation();
            self.config_version += 1;
            self.freq_version += 1;
            return Ok(());
        }
        self.apply_rta_request(self.rta_params())
    }

    fn apply_rta_request(&mut self, params: RtaParams) -> Result<(), String> {
        let candidate = params.trigger.clone();
        let rta = self.rta_hw.as_mut().ok_or("RTA hardware is not ready")?;
        match rta.reconfigure(params) {
            Ok(geometry) => {
                self.trigger = candidate;
                self.rta_error = None;
                self.invalidate_observation();
                self.config_version += 1;
                self.freq_version += 1;
                self.apply_rta_geometry(geometry);
            }
            Err(failure) => {
                let message = failure.message.clone();
                self.rta_error = Some(message.clone());
                if LINK_LOST_STATUS.contains(&failure.status) {
                    // Do not query an owner which has already lost its device.
                    self.retire_rta(failure);
                } else {
                    match rta.trigger_actual() {
                        Ok(actual) if actual.valid != 0 => {
                            self.rta_trigger_generation += 1;
                            self.apply_rta_trigger_actual(actual, false);
                        }
                        Ok(_) => self.clear_rta_trigger(false),
                        // A rejected profile can retain its owner only when its
                        // restored capture can still be queried successfully.
                        Err(readback) => self.retire_rta(readback),
                    }
                }
                return Err(message);
            }
        }
        Ok(())
    }

    /// Every fatal acquisition or trigger-query path retires the same owner and
    /// invalidates completion before allowing a bounded retry with saved requests.
    pub(super) fn retire_rta(&mut self, failure: SdkFailure) {
        if LINK_LOST_STATUS.contains(&failure.status) { self.link_up = false; }
        self.rta_error = Some(failure.message);
        self.clear_rta_trigger(false);
        self.rta_hw = None;
        self.rta_retry_at = Some(std::time::Instant::now() + Duration::from_secs(1));
    }
    pub(super) fn unknown_rta_trigger(simulated: bool) -> Value {
        json!({"source":null,"edge":null,"level":null,"safetime":null,"delay":null,
            "pretime":null,"acqtime":null,"retrigger":null,"retriggerperiod":null,
            "out":null,"outpolarity":null,"poi":null,"frames":0,"generation":0,
            "capture_id":null,"packet_index":null,"packet_count":null,"waiting":null,
            "complete":null,"last_sdk_status":null,"first_ts":null,"edges":null,
            "simulated":simulated,"supported":false,"valid":false})
    }
    pub(super) fn clear_rta_trigger(&mut self, simulated: bool) {
        self.rta_completed_frame = None;
        self.rta_trigger_actual = Self::unknown_rta_trigger(simulated);
        self.rta_trigger_actual["frames"] = json!(self.rta_frames);
        self.rta_trigger_actual["generation"] = json!(self.rta_trigger_generation);
    }


    pub(super) fn apply_rta_trigger_actual(&mut self, a: hardware_rta::RtaTriggerActual, has_frame: bool) {
        if !has_frame { self.rta_completed_frame = None; }
        if a.valid == 0 {
            self.clear_rta_trigger(false);
            return;
        }
        let source = match a.source {0=>Some("freerun"),1=>Some("external"),2=>Some("bus"),3=>Some("level"),4=>Some("timer"),_=>None};
        let edge = match a.edge {0=>Some("rising"),1=>Some("falling"),2=>Some("double"),_=>None};
        let out = match a.out {0=>Some("none"),1=>Some("per_hop"),2=>Some("per_sweep"),3=>Some("per_profile"),_=>None};
        let polarity = match a.polarity {0=>Some("positive"),1=>Some("negative"),_=>None};
        self.rta_trigger_actual = json!({"source":source,"edge":edge,"level":a.level,
            "safetime":a.safetime,"delay":a.delay,"pretime":a.pretime,"acqtime":a.acqtime,
            "retrigger":a.retrigger,"retriggerperiod":a.retriggerperiod,"out":out,"outpolarity":polarity,
            "poi":a.poi,"frames":self.rta_frames,"generation":self.rta_trigger_generation,
            "capture_id":null,"packet_index":if a.packet_index>0 {Some(a.packet_index)} else {None},
            "packet_count":if a.packet_count>0 {Some(a.packet_count)} else {None},
            "waiting":if has_frame {Some(false)} else {None},
            "complete":match a.complete {0=>Some(false),1=>Some(true),_=>None},
            "trigger_missed":a.last_sdk_status == -302 || a.last_sdk_status == -303,
            "last_sdk_status":a.last_sdk_status,"first_ts":if has_frame {Some(a.first_data_timestamp)} else {None},
            "edges":if has_frame {Some(a.trigger_edges)} else {None},"simulated":false,"supported":true,"valid":true});
    }

    pub(super) fn configure_trigger(&mut self, candidate: RtaTrigger) -> Result<(), String> {
        if self.swp.mode == "rta" && self.hardware_shim.is_some() {
            let mut params = self.rta_params();
            params.trigger = candidate;
            self.apply_rta_request(params)?;
        } else {
            self.trigger = candidate;
            self.config_version += 1;
            if self.swp.mode == "rta" {
                self.rta_trigger_generation += 1;
                self.clear_rta_trigger(true);
            }
        }
        Ok(())
    }

    pub(super) fn attach_rta_trigger(&self, mut frame: Vec<u8>) -> Vec<u8> {
        if frame.is_empty() { return frame; }
        frame[..4].copy_from_slice(b"RTA2");
        let metadata = self.rta_trigger_actual.to_string();
        frame.extend_from_slice(b"TRIG");
        frame.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
        frame.extend_from_slice(metadata.as_bytes());
        frame
    }


    pub(super) fn apply_rta_geometry(&mut self, g: hardware_rta::RtaGeometry) {
        self.rta_span = g.stop - g.start;
        self.rta_trigger_generation += 1;
        self.apply_rta_trigger_actual(g.trigger, false);
        self.rta_actual = json!({
            "center": self.rta_center,
            "span": g.stop - g.start,
            "start": g.start,
            "stop": g.stop,
            "ref": g.ref_dbm, "rbw": g.rbw, "vbw": g.vbw,
            "points": g.points, "frame_points": g.points,
            "refclk": g.refclk, "refclk_src": g.refclk_src, "refclk_out": self.refclk_out,
            "atten": g.atten, "preamp": g.preamp, "ifgain": g.ifgain,
            "pan_available": true,
        });
    }

}
