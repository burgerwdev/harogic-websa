use super::*;
use receiver_workers::ChannelConfig;
use websa_channelizer::rate_plan::RatePlan;

pub(super) fn channel_plan(rate:f64, config:&ChannelConfig)->Result<RatePlan,String> {
    let occupied=match config.mode.as_str() {
        "usb"|"lsb" => config.if_bw*2.0,
        "ft8" => config.if_bw.max(3_200.0)*2.0,
        "sstv" => config.if_bw.max(3_000.0)*2.0,
        "cw" => config.if_bw+config.pitch*2.0,
        _ => config.if_bw,
    };
    RatePlan::new(rate,&config.mode,occupied).map_err(str::to_owned)
}

/// Fit a requested RF point to the nearest flat coarse passband inside a valid window.
/// The same relative geometry fits a capture center while keeping its listen frequency fixed.
pub(super) fn fit_passband_frequency(center:f64, requested:f64, minimum:f64, maximum:f64,
    taps:usize, plan:&RatePlan)->Result<f64,String> {
    let flat=coarse_passband_hz(COARSE_RATE,taps).ok_or("invalid PFB prototype")?;
    let radius=(flat-plan.stopband_hz).min(BIN_SPACING/2.0);
    if minimum>maximum || radius<0.0 {
        return Err("channel occupied bandwidth and transition exceed capture or coarse passband".into());
    }
    let requested=requested.clamp(minimum,maximum);
    let nearest=signed_bin(requested,center);
    let mut best=None;
    for bin in nearest-1..=nearest+1 {
        let coarse=center+bin as f64*BIN_SPACING;
        // Keep arithmetic at the edge on the accepted side of the strict guard.
        let margin=(radius-0.000_001).max(0.0);
        let lo=minimum.max(coarse-margin);
        let hi=maximum.min(coarse+margin);
        if lo<=hi {
            let candidate=requested.clamp(lo,hi);
            if best.is_none_or(|previous:f64| (candidate-requested).abs()<(previous-requested).abs()) {
                best=Some(candidate);
            }
        }
    }
    best.ok_or_else(||"channel occupied bandwidth and transition exceed capture or coarse passband".into())
}
impl Ddc {
    pub(super) fn configured_service(center: f64, bandwidth: f64,
        hardware: &mut Option<HardwareCapture>, taps: usize, fs: f64,
        configs: &[ChannelConfig]) -> Result<Self, String> {
        Self::configured_service_inner(center, bandwidth, hardware, taps, fs, configs, true)
    }

    fn configured_service_cached(center: f64, bandwidth: f64,
        hardware: &mut Option<HardwareCapture>, taps: usize, fs: f64,
        configs: &[ChannelConfig]) -> Result<Self, String> {
        Self::configured_service_inner(center, bandwidth, hardware, taps, fs, configs, false)
    }

    fn configured_service_inner(center: f64, bandwidth: f64,
        hardware: &mut Option<HardwareCapture>, taps: usize, fs: f64,
        configs: &[ChannelConfig], allow_calibration: bool) -> Result<Self, String> {
        if configs.len() != 3 || configs.iter().enumerate().any(|(i, c)| c.channel != i as u32 + 1) {
            return Err("service requires exactly three channel slots".into());
        }
        let m = PFB_BINS.min((fs / BIN_SPACING).round() as i64).max(2);
        let mut bins = Vec::new();
        for config in configs.iter().filter(|c| c.enabled) {
            let plan = channel_plan(COARSE_RATE,config)?;
            Self::validate_service_frequency(center, bandwidth, fs, taps, config.freq, &plan)?;
            let signed = signed_bin(config.freq, center);
            for neighbour in signed - PLAN_NEIGHBOURS..=signed + PLAN_NEIGHBOURS {
                if (-m / 2..m / 2).contains(&neighbour) {
                    let bin = neighbour.rem_euclid(m) as u32;
                    if !bins.contains(&bin) { bins.push(bin); }
                }
            }
        }
        if bins.is_empty() { bins.push(0); }
        if allow_calibration { super::service_cost::admit_service(configs,fs,taps,bins.len())?; }
        else { super::service_cost::admit_service_cached(configs,fs,taps,bins.len())?; }
        let epoch = allocate_dsp_epoch()?;
        let stream = PfbStream::with_taps(fs, 16240, bins, epoch as u64, taps).ok_or("PFB unavailable")?;
        let mut bank = FineBank::new(&stream, bandwidth, 16240 / stream.input_hop() + 2,
            3, COARSE_RATE * 3.0, CONSUMER_BUDGET).ok_or("fine bank unavailable")?;
        let mut framers = BTreeMap::new();
        let mut registry = Vec::new();
        for config in configs.iter().filter(|c| c.enabled) {
            let plan = channel_plan(stream.sample_rate,config)?;
            let signed = signed_bin(config.freq, center);
            let bin = signed.rem_euclid(m) as u32;
            let offset = signed as f64 * BIN_SPACING - (config.freq - center);
            let id = u64::from(config.channel);
            bank.subscribe_planned(id, bin, offset, plan).map_err(str::to_owned)?;
            let (rate, delay) = bank.channel_metadata(id).ok_or("service channel metadata absent")?;
            let framer = if config.channel == 1 { BasebandFramer::new(id, epoch as u64, config.freq, rate) }
                else { BasebandFramer::new_aux(id, epoch as u64, config.freq, rate) }.ok_or("service framer rejected")?;
            framers.insert(config.channel, (id, config.freq, framer));
            registry.push(sdr_pipeline::ChannelRegistryEntry { channel:config.channel, channel_id:config.channel,
                epoch,frequency_hz:config.freq,output_rate:rate,input_rate:fs,group_delay_samples:delay,state:"active" });
        }
        let primary = configs.first().ok_or("primary config missing")?;
        let primary_framer = framers.remove(&1).map(|(_,_,f)| f).unwrap_or(
            BasebandFramer::new(1, epoch as u64, primary.freq, COARSE_RATE).ok_or("primary framer rejected")?);
        let mut fine = sdr_pipeline::FineState::new_service(bank, primary_framer, fs, registry.into());
        fine.aux = framers;
        fine.next_id = 3;
        let registry = fine.registry.clone();
        let generation = hardware.as_ref().map_or(0, |c| c.geometry.generation);
        #[cfg(test)]
        let cost_profile=websa_channelizer::FineCostBudget::new([1.0;23],1.0,f64::MAX).ok_or("cost metadata rejected")?;
        Ok(Self { stream:Box::new(stream),fine:Some(fine),fine_worker:None,carry:Vec::new(),pan:Vec::new(),
            capture_center:center,usable_bandwidth:bandwidth,decimate:1,listen:primary.freq,next_seq:0,
            input_pos:0,stamp_gaps:0,last_stamp:None,generation,device_decimate:if hardware.is_some() {
                ((IQ_FS/fs).round() as u64).max(1) } else {1},hardware:hardware.take(),capture_failed:false,
            pipeline_timing:sdr_pipeline::PipelineTiming::new(fs),registry,
            #[cfg(test)]
            cost_profile,
            service_configs:Some(configs.to_vec()) })
    }


    pub(super) fn recapture_service_configs(&mut self,center:f64,display_decimate:i32,
        front_end:FrontEnd,taps:usize,configs:&[ChannelConfig])->Result<Vec<Vec<u8>>,String> {
        if !center.is_finite() || display_decimate<1 || !matches!(taps,4|8|16) {
            return Err("invalid service capture geometry".into());
        }
        let frequencies:Vec<_>=configs.iter().filter(|c|c.enabled).map(|c|c.freq).collect();
        let listen=frequencies.first().copied().unwrap_or(center);
        let device_decimate=capture_decimate(center,listen,&frequencies,display_decimate,taps);
        let fs=if self.hardware.is_some() {IQ_FS/device_decimate as f64} else {self.stream.input_sample_rate()};
        let bandwidth=if self.hardware.is_some() {fs*0.8} else {self.usable_bandwidth};
        // Build the entire replacement before touching the capture or old framers.
        let mut next=Self::configured_service_cached(center,bandwidth,&mut None,taps,fs,&configs)?;
        next.stamp_gaps=self.stamp_gaps;
        next.generation=self.generation;
        next.device_decimate=self.device_decimate;
        if let Some(capture)=self.hardware.as_ref() {
            let geometry=match capture.reconfigure(center,front_end,device_decimate) {
                Ok(value)=>value,
                Err(error)=>{self.capture_failed=true;return Err(error);}
            };
            if !geometry.center.is_finite() || !geometry.bandwidth.is_finite()
                || (geometry.center-center).abs()>0.5 || (geometry.bandwidth-bandwidth).abs()>0.5 {
                self.capture_failed=true;
                return Err(format!("IQS geometry mismatch: requested center={center} bandwidth={bandwidth}, actual center={} bandwidth={}",geometry.center,geometry.bandwidth));
            }
            next.capture_center=geometry.center;
            next.usable_bandwidth=geometry.bandwidth;
            next.generation=geometry.generation;
            next.device_decimate=device_decimate as u64;
        }
        let drained=(||->Result<Vec<Vec<u8>>,String>{
            self.stream.flush_pending().map_err(str::to_owned)?;
            let mut frames=std::mem::take(&mut self.carry);
            frames.extend(self.drain()?);
            self.finish_framers(&mut frames)?;
            Ok(frames)
        })();
        let frames=match drained {Ok(frames)=>frames,Err(error)=>{self.capture_failed=true;return Err(error);}};
        next.hardware=self.hardware.take();
        *self=next;
        Ok(frames)
    }


    pub(super) fn validate_service_frequency(center:f64,bandwidth:f64,fs:f64,taps:usize,frequency:f64,
        plan:&RatePlan)->Result<(),String> {
        let signed = signed_bin(frequency,center);
        let m = PFB_BINS.min((fs/BIN_SPACING).round() as i64).max(2);
        let offset = signed as f64*BIN_SPACING-(frequency-center);
        let flat = coarse_passband_hz(COARSE_RATE,taps).ok_or("invalid PFB prototype")?;
        if !frequency.is_finite() || !(-m/2..m/2).contains(&signed)
            || (frequency-center).abs()+plan.occupied_bandwidth_hz/2.0>bandwidth/2.0
            || offset.abs()+plan.stopband_hz>flat {
            return Err("channel occupied bandwidth and transition exceed capture or coarse passband".into());
        }
        Ok(())
    }

    pub(super) fn configure_service_channel(&mut self, config:ChannelConfig)->Result<Vec<Vec<u8>>,String> {
        self.configure_service_channel_inner(config,false)
    }

    pub(super) fn restart_service_receivers(&mut self)->Result<Vec<Vec<u8>>,String> {
        let configs=self.service_configs.clone().ok_or("not a service channel plan")?;
        let mut frames=Vec::new();
        for config in configs.into_iter().filter(|config|config.enabled) {
            frames.extend(self.configure_service_channel_inner(config,true)?);
        }
        Ok(frames)
    }

    fn configure_service_channel_inner(&mut self,config:ChannelConfig,force:bool)->Result<Vec<Vec<u8>>,String> {
        let mut configs = self.service_configs.clone().ok_or("not a service channel plan")?;
        if !(1..=3).contains(&config.channel) { return Err("channel must be 1, 2, or 3".into()); }
        let slot = config.channel as usize-1;
        let old = &configs[slot];
        if !force && old.enabled == config.enabled && old.freq == config.freq && old.mode == config.mode
            && old.if_bw == config.if_bw && old.pitch == config.pitch && old.sstv_mode == config.sstv_mode
            && old.audio_rate == config.audio_rate && old.stereo == config.stereo {
            configs[slot] = config;
            self.service_configs = Some(configs);
            return Ok(Vec::new());
        }
        let plan = channel_plan(self.stream.sample_rate,&config)?;
        let old_id = self.registry.iter().find(|r|r.channel==config.channel).map(|r|u64::from(r.channel_id));
        if config.enabled { self.bank.check_planned_admission(old_id,&plan).map_err(str::to_owned)?; }
        if config.enabled { Self::validate_service_frequency(self.capture_center,self.usable_bandwidth,
            self.stream.input_sample_rate(),self.stream.taps_per_phase(),config.freq,&plan)?; }
        let mut candidate=configs.clone();
        candidate[slot]=config.clone();
        let bin=signed_bin(config.freq,self.capture_center).rem_euclid(self.stream.bin_count() as i64) as u32;
        let selected=self.stream.bins().len()+usize::from(config.enabled && !self.stream.bins().contains(&bin));
        super::service_cost::admit_service_cached(&candidate,self.stream.input_sample_rate(),self.stream.taps_per_phase(),selected)?;
        self.stream.flush_pending().map_err(str::to_owned)?;
        let mut frames = std::mem::take(&mut self.carry);
        frames.extend(self.drain()?);
        let next_id = self.next_id.checked_add(1).filter(|&id|id<=u32::MAX as u64).ok_or("channel ID exhausted")?;
        if config.enabled {
            let signed = signed_bin(config.freq,self.capture_center);
            let bin = signed.rem_euclid(self.stream.bin_count() as i64) as u32;
            if !self.stream.bins().contains(&bin) {
                let mut bins=self.stream.bins().to_vec();
                bins.push(bin);
                self.stream.replace_bins(bins.clone()).map_err(str::to_owned)?;
                self.bank.remap_bins(&bins).map_err(str::to_owned)?;
            }
            let offset = signed as f64*BIN_SPACING-(config.freq-self.capture_center);
            if let Some(id)=old_id { self.bank.retune_planned(id,next_id,bin,offset,plan) }
            else { self.bank.subscribe_planned(next_id,bin,offset,plan) }.map_err(str::to_owned)?;
        } else if let Some(id)=old_id { self.bank.unsubscribe(id); }
        if config.channel==1 {
            if old_id.is_some() {
                if let Some(tail)=self.framer.finish().map_err(str::to_owned)? { frames.push(tail); }
            }
            if config.enabled {
                let rate=self.bank.channel_metadata(next_id).ok_or("channel metadata absent")?.0;
                self.framer=BasebandFramer::new(next_id,self.stream.epoch(),config.freq,rate).ok_or("framer rejected")?;
                self.channel_id=next_id; self.listen=config.freq;
            }
        } else {
            if let Some((_,_,mut framer))=self.aux.remove(&config.channel) {
                if let Some(tail)=framer.finish().map_err(str::to_owned)? { frames.push(tail); }
            }
            if config.enabled {
                let rate=self.bank.channel_metadata(next_id).ok_or("channel metadata absent")?.0;
                let framer=BasebandFramer::new_aux(next_id,self.stream.epoch(),config.freq,rate).ok_or("framer rejected")?;
                self.aux.insert(config.channel,(next_id,config.freq,framer));
            }
        }
        self.next_id=next_id;
        configs[slot]=config;
        self.service_configs=Some(configs);
        self.refresh_service_registry()?;
        Ok(frames)
    }

    pub(super) fn refresh_service_registry(&mut self)->Result<(),String> {
        let configs=self.service_configs.as_ref().ok_or("service configs missing")?;
        let mut registry=Vec::new();
        for c in configs.iter().filter(|c|c.enabled) {
            let id=if c.channel==1 {self.channel_id} else {self.aux.get(&c.channel).ok_or("aux identity missing")?.0};
            let (rate,delay)=self.bank.channel_metadata(id).ok_or("channel metadata missing")?;
            registry.push(sdr_pipeline::ChannelRegistryEntry{channel:c.channel,channel_id:id as u32,
                epoch:self.stream.epoch() as u32,frequency_hz:c.freq,output_rate:rate,
                input_rate:self.stream.input_sample_rate(),group_delay_samples:delay,state:"active"});
        }
        self.registry=registry.into();
        Ok(())
    }
}
