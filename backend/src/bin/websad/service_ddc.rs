use super::*;
use receiver_workers::ChannelConfig;
use websa_channelizer::rate_plan::RatePlan;

pub(super) fn channel_plan(rate:f64, config:&ChannelConfig)->Result<RatePlan,String> {
    let occupied=match config.mode.as_str() {
        "usb"|"lsb" => config.if_bw*2.0,
        "ft8" => config.if_bw.max(3_200.0)*2.0,
        "sstv" => config.if_bw.max(3_000.0)*2.0,
        "radiofax" => config.if_bw.max(3_000.0)*2.0,
        mode if websa_channelizer::catalog::is_external(mode)
            && websa_channelizer::catalog::mode(mode).is_some_and(|descriptor| descriptor.sideband == "upper")
            => config.if_bw * 2.0,
        "cw" => config.if_bw+config.pitch*2.0,
        _ => config.if_bw,
    };
    if let Some(baseband) = &config.baseband {
        let minimum = if config.processing_enabled {
            if occupied > baseband.passband_hz {
                return Err("processor passband exceeds configured baseband".into());
            }
            RatePlan::new(rate, &config.mode, occupied).map_err(str::to_owned)?.output_rate
                .max(baseband.minimum_rate_hz)
        } else { baseband.minimum_rate_hz };
        RatePlan::baseband(rate, baseband.passband_hz, baseband.transition_hz, minimum).map_err(str::to_owned)
    } else { RatePlan::new(rate,&config.mode,occupied).map_err(str::to_owned) }
}

/// Only sample-producing geometry owns IQ identity; processor settings own a separate branch.
pub(super) fn same_baseband(fs:f64,old:&ChannelConfig,next:&ChannelConfig)->bool {
    old.baseband.is_some() && next.baseband.is_some() && old.enabled==next.enabled && old.freq==next.freq
        && source_plan(fs,old).ok().zip(source_plan(fs,next).ok())
            .is_some_and(|(a,b)|a.is_direct()==b.is_direct() && a.plan()==b.plan())
}

#[derive(Clone, Debug)]
pub(super) enum SourcePlan {
    Coarse(RatePlan),
    Direct(RatePlan),
}
impl SourcePlan {
    pub fn plan(&self) -> &RatePlan { match self { Self::Coarse(p) | Self::Direct(p) => p } }
    pub fn is_direct(&self) -> bool { matches!(self, Self::Direct(_)) }
}

/// Decoder input floors above a coarse channel require original capture samples.
pub(super) fn is_direct(config: &ChannelConfig) -> bool {
    config.processing_enabled && websa_channelizer::catalog::external_rates(&config.mode)
        .is_some_and(|(_, minimum)| minimum as f64 > COARSE_RATE)
}

pub(super) fn source_plan(fs: f64, config: &ChannelConfig) -> Result<SourcePlan, String> {
    if is_direct(config) { channel_plan(fs, config).map(SourcePlan::Direct) }
    else {
        let bins=PFB_BINS.min((fs/BIN_SPACING).round() as i64).max(2);
        channel_plan(fs/(bins/2) as f64, config).map(SourcePlan::Coarse)
    }
}

/// Only active consumers own coarse columns. Native local tuning can replace
/// the selection at a drained boundary without changing the capture clock.
pub(super) fn coarse_bins(center:f64,fs:f64,configs:&[ChannelConfig])->Result<Vec<u32>,String> {
    let count=PFB_BINS.min((fs/BIN_SPACING).round() as i64).max(2);
    let mut bins=Vec::new();
    for config in configs.iter().filter(|config|config.enabled) {
        if source_plan(fs,config)?.is_direct() {continue;}
        let bin=signed_bin(config.freq,center).rem_euclid(count) as u32;
        if !bins.contains(&bin) {bins.push(bin);}
    }
    Ok(bins)
}

pub(super) fn validate_source_frequency(center:f64, bandwidth:f64, fs:f64, taps:usize,
    frequency:f64, source:&SourcePlan)->Result<(),String> {
    if source.is_direct() {
        if !frequency.is_finite() || (frequency-center).abs()+source.plan().stopband_hz>bandwidth/2.0 {
            return Err("channel filter and transition exceed usable capture bandwidth".into());
        }
        Ok(())
    } else { Ddc::validate_service_frequency(center,bandwidth,fs,taps,frequency,source.plan()) }
}

pub(super) fn service_capture_decimate(center:f64,display:i32,taps:usize,
    configs:&[ChannelConfig])->Result<i32,String> {
    let listen=configs.iter().find(|config|config.channel==1&&config.enabled).map_or(center,|config|config.freq);
    let maximum=capture_decimate(center,listen,&[],display,taps);
    for power in (0..=6).rev() {
        let decimate=1<<power;
        if decimate>maximum {continue;}
        let fs=IQ_FS/decimate as f64;
        if configs.iter().filter(|c|c.enabled).all(|config|source_plan(fs,config)
            .and_then(|source|validate_source_frequency(center,fs*0.8,fs,taps,config.freq,&source)).is_ok()) {
            return Ok(decimate);
        }
    }
    Err("receiver passbands do not fit a shared device capture".into())
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
        let mut slots = std::collections::BTreeSet::new();
        for config in configs {
            if !(1..=MAX_CHANNELS as u32).contains(&config.channel) {return Err("service channel exceeds capacity".into());}
            if !slots.insert(config.channel) {return Err("duplicate service channel".into());}
        }
        let m = PFB_BINS.min((fs / BIN_SPACING).round() as i64).max(2);
        let bins = coarse_bins(center,fs,configs)?;
        for config in configs.iter().filter(|c| c.enabled) {
            let source = source_plan(fs,config)?;
            validate_source_frequency(center, bandwidth, fs, taps, config.freq, &source)?;
        }
        super::service_cost::admit_service(configs,fs,taps,bins.len())?;
        let epoch = allocate_dsp_epoch()?;
        let packet_capacity = if fs >= 15_625_000.0 { SDR_BATCH_SAMPLES } else { SDR_PACKET_SAMPLES };
        let stream = PfbStream::with_taps(fs, packet_capacity, bins, epoch as u64, taps).ok_or("PFB unavailable")?;
        let mut bank = FineBank::new(&stream, bandwidth, packet_capacity / stream.input_hop() + 2,
            MAX_CHANNELS, COARSE_RATE * MAX_CHANNELS as f64, CONSUMER_BUDGET).ok_or("fine bank unavailable")?;
        let mut framers = BTreeMap::new();
        let mut registry = Vec::new();
        let mut direct = BTreeMap::new();
        for config in configs.iter().filter(|c| c.enabled) {
            let source = source_plan(fs,config)?;
            let id = u64::from(config.channel);
            let (rate,delay) = if source.is_direct() {
                let mut route=sdr_pipeline::DirectChannel::new(config.channel,id,epoch as u64,fs,
                    center,config.freq,source.plan(),0,0)?;
                route.framer.settle_startup(config.recovery_settle);
                let metadata=(route.rate,route.delay);
                direct.insert(config.channel,route);
                metadata
            } else {
                let signed = signed_bin(config.freq, center);
                let bin = signed.rem_euclid(m) as u32;
                let offset = signed as f64 * BIN_SPACING - (config.freq - center);
                bank.subscribe_planned(id, bin, offset, source.plan().clone()).map_err(str::to_owned)?;
                let (rate, delay) = bank.channel_metadata(id).ok_or("service channel metadata absent")?;
                let mut framer = if config.channel == 1 { BasebandFramer::new(id, epoch as u64, config.freq, rate) }
                    else { BasebandFramer::new_aux(id, epoch as u64, config.freq, rate) }
                    .and_then(|framer|framer.with_source_rate(fs)).ok_or("service framer rejected")?;
                framer.settle_startup(config.recovery_settle);
                framers.insert(config.channel, (id, config.freq, framer));
                (rate,delay)
            };
            registry.push(sdr_pipeline::ChannelRegistryEntry { channel:config.channel, channel_id:config.channel,
                epoch,frequency_hz:config.freq,output_rate:rate,input_rate:fs,group_delay_samples:delay,state:"active" });
        }
        let primary = configs.iter().find(|config|config.channel==1).cloned().unwrap_or_else(||ChannelConfig::for_slot(1));
        let primary_framer = framers.remove(&1).map(|(_,_,f)| f).unwrap_or(
            BasebandFramer::new(1, epoch as u64, primary.freq, COARSE_RATE).ok_or("primary framer rejected")?);
        let mut fine = sdr_pipeline::FineState::new_service(bank, primary_framer, fs, registry.into());
        fine.aux = framers;
        fine.next_id = configs.iter().map(|config|u64::from(config.channel)).max().unwrap_or(1);
        let registry = fine.registry.clone();
        let generation = hardware.as_ref().map_or(0, |c| c.geometry.generation);
        #[cfg(test)]
        let cost_profile=None;
        Ok(Self { stream:Box::new(stream),fine:Some(fine),fine_worker:None,direct,carry:Vec::new(),pan:Vec::new(),scan_tap:None,default_fft:None,display_fft:None,display_fft_error:None,
            capture_center:center,usable_bandwidth:bandwidth,decimate:1,listen:primary.freq,next_seq:0,
            input_pos:0,capture_scratch:Vec::new(),stamp_gaps:0,last_stamp:None,generation,device_decimate:if hardware.is_some() {
                ((IQ_FS/fs).round() as u64).max(1) } else {1},hardware:hardware.take(),capture_failed:false,
            pipeline_timing:sdr_pipeline::PipelineTiming::new(fs),registry,
            #[cfg(test)]
            cost_profile,
            service_configs:Some(configs.to_vec()) })
    }


    pub(super) fn recapture_service_configs(&mut self,center:f64,display_decimate:i32,
        front_end:FrontEnd,taps:usize,configs:&[ChannelConfig])->Result<IqFrames,String> {
        if !center.is_finite() || display_decimate<1 || !matches!(taps,4|8|16) {
            return Err("invalid service capture geometry".into());
        }
        let device_decimate=service_capture_decimate(center,display_decimate,taps,configs)?;
        let fs=if self.hardware.is_some() {IQ_FS/device_decimate as f64} else {self.stream.input_sample_rate()};
        let bandwidth=if self.hardware.is_some() {fs*0.8} else {self.usable_bandwidth};
        // Build the entire replacement before touching the capture or old framers.
        let mut next=Self::configured_service(center,bandwidth,&mut None,taps,fs,&configs)?;
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
        let drained=(||->Result<IqFrames,String>{
            self.stream.flush_pending().map_err(str::to_owned)?;
            let mut frames=std::mem::take(&mut self.carry);
            frames.extend(self.drain()?);
            self.finish_framers(&mut frames)?;
            Ok(frames)
        })();
        let frames=match drained {Ok(frames)=>frames,Err(error)=>{self.capture_failed=true;return Err(error);}};
        next.scan_tap=self.scan_tap.take();
        next.default_fft=self.default_fft.take();
        next.display_fft=self.display_fft.take();
        next.display_fft_error=self.display_fft_error.take();
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
            || (frequency-center).abs()+plan.stopband_hz>bandwidth/2.0
            || offset.abs()+plan.stopband_hz>flat {
            return Err("channel occupied bandwidth and transition exceed capture or coarse passband".into());
        }
        Ok(())
    }

    pub(super) fn configure_service_channel(&mut self, config:ChannelConfig)->Result<IqFrames,String> {
        self.configure_service_channel_inner(config,false)
    }

    pub(super) fn recover_service_channel(&mut self,mut config:ChannelConfig)->Result<IqFrames,String> {
        config.recovery_settle=true;
        self.configure_service_channel_inner(config,true)
    }

    pub(super) fn restart_service_receivers(&mut self)->Result<IqFrames,String> {
        let configs=self.service_configs.clone().ok_or("not a service channel plan")?;
        let mut frames=Vec::new();
        for config in configs.into_iter().filter(|config|config.enabled&&config.processing_enabled&&config.baseband.is_none()) {
            frames.extend(self.configure_service_channel_inner(config,true)?);
        }
        Ok(frames)
    }

    fn configure_service_channel_inner(&mut self,config:ChannelConfig,force:bool)->Result<IqFrames,String> {
        let mut configs = self.service_configs.clone().ok_or("not a service channel plan")?;
        if !(1..=MAX_CHANNELS as u32).contains(&config.channel) {return Err("service channel exceeds capacity".into());}
        let slot = configs.iter().position(|old|old.channel==config.channel).ok_or("service channel not defined")?;
        let old = &configs[slot];
        if !force && same_baseband(self.stream.input_sample_rate(),old,&config) {
            configs[slot]=config;self.service_configs=Some(configs);return Ok(Vec::new());
        }
        if !force && old.enabled == config.enabled && old.freq == config.freq && old.mode == config.mode
            && old.baseband == config.baseband && old.processing_enabled == config.processing_enabled
            && old.if_bw == config.if_bw && old.pitch == config.pitch && old.sstv_mode == config.sstv_mode
            && old.radiofax_acquisition == config.radiofax_acquisition && old.radiofax_ioc == config.radiofax_ioc
            && old.radiofax_lpm == config.radiofax_lpm && old.radiofax_modulation == config.radiofax_modulation
            && old.radiofax_polarity == config.radiofax_polarity && old.radiofax_clock_recovery == config.radiofax_clock_recovery
            && old.audio_rate == config.audio_rate && old.stereo == config.stereo {
            configs[slot] = config;
            self.service_configs = Some(configs);
            return Ok(Vec::new());
        }
        let fs=self.stream.input_sample_rate();
        let source = source_plan(if config.enabled {fs} else {IQ_FS},&config)?;
        let old_direct=self.direct.contains_key(&config.channel);
        let old_id = self.registry.iter().find(|r|r.channel==config.channel).map(|r|u64::from(r.channel_id));
        if config.enabled && !source.is_direct() {
            self.bank.check_planned_admission(if old_direct {None} else {old_id},source.plan()).map_err(str::to_owned)?;
        }
        if config.enabled { validate_source_frequency(self.capture_center,self.usable_bandwidth,
            fs,self.stream.taps_per_phase(),config.freq,&source)?; }
        let mut candidate=configs.clone();
        candidate[slot]=config.clone();
        let next_bins=coarse_bins(self.capture_center,fs,&candidate)?;
        let selected=next_bins.len();
        super::service_cost::admit_service(&candidate,self.stream.input_sample_rate(),self.stream.taps_per_phase(),selected)?;
        let next_id = self.next_id.checked_add(1).filter(|&id|id<=u32::MAX as u64).ok_or("channel ID exhausted")?;
        let next_direct=if config.enabled && source.is_direct() {
            Some(sdr_pipeline::DirectChannel::new(config.channel,next_id,self.stream.epoch(),fs,
                self.capture_center,config.freq,source.plan(),self.input_pos,self.next_seq)?)
        } else {None};
        self.stream.flush_pending().map_err(str::to_owned)?;
        let mut frames = std::mem::take(&mut self.carry);
        frames.extend(self.drain()?);
        if let Some(worker)=self.fine_worker.as_mut() {
            if let Err(error)=worker.pause_channel(config.channel,&mut frames) {
                self.carry.extend(frames);return Err(error);
            }
        }
        if let Some(worker)=self.fine_worker.as_mut() {self.direct.extend(worker.take_direct());}
        if config.enabled && !source.is_direct() {
            let signed = signed_bin(config.freq,self.capture_center);
            let bin = signed.rem_euclid(self.stream.bin_count() as i64) as u32;
            if !self.stream.bins().contains(&bin) {
                let mut bins=self.stream.bins().to_vec();
                bins.push(bin);
                self.stream.replace_bins(bins.clone()).map_err(str::to_owned)?;
                self.bank.remap_bins(&bins).map_err(str::to_owned)?;
            }
            let offset = signed as f64*BIN_SPACING-(config.freq-self.capture_center);
            if let Some(id)=old_id.filter(|_|!old_direct) { self.bank.retune_planned(id,next_id,bin,offset,source.plan().clone()) }
            else { self.bank.subscribe_planned(next_id,bin,offset,source.plan().clone()) }.map_err(str::to_owned)?;
        } else if let Some(id)=old_id.filter(|_|!old_direct) { self.bank.unsubscribe(id); }
        if let Some(mut previous)=self.direct.remove(&config.channel) {
            frames.extend(previous.framer.finish_native().map_err(str::to_owned)?);
        }
        if config.channel==1 {
            if old_id.is_some() && !old_direct {
                if let Some(tail)=self.framer.finish_native().map_err(str::to_owned)? { frames.push(tail); }
            }
            if config.enabled {
                if !source.is_direct() {
                    let rate=self.bank.channel_metadata(next_id).ok_or("channel metadata absent")?.0;
                    self.framer=BasebandFramer::new(next_id,self.stream.epoch(),config.freq,rate)
                        .and_then(|framer|framer.with_source_rate(fs)).ok_or("framer rejected")?;
                }
                self.channel_id=next_id; self.listen=config.freq;
            }
        } else {
            if let Some((_,_,mut framer))=self.aux.remove(&config.channel) {
                if let Some(tail)=framer.finish_native().map_err(str::to_owned)? { frames.push(tail); }
            }
            if config.enabled && !source.is_direct() {
                let rate=self.bank.channel_metadata(next_id).ok_or("channel metadata absent")?.0;
                let framer=BasebandFramer::new_aux(next_id,self.stream.epoch(),config.freq,rate)
                    .and_then(|framer|framer.with_source_rate(fs)).ok_or("framer rejected")?;
                self.aux.insert(config.channel,(next_id,config.freq,framer));
            }
        }
        if config.recovery_settle {
            if config.channel==1 {self.framer.settle_startup(true);}
            else if let Some((_,_,framer))=self.aux.get_mut(&config.channel) {framer.settle_startup(true);}
        }
        if let Some(mut route)=next_direct {route.framer.settle_startup(config.recovery_settle);self.direct.insert(config.channel,route);}
        self.next_id=next_id;
        configs[slot]=config;
        if self.stream.bins()!=next_bins.as_slice() {
            self.stream.replace_bins(next_bins.clone()).map_err(str::to_owned)?;
            self.bank.remap_bins(&next_bins).map_err(str::to_owned)?;
        }
        self.service_configs=Some(configs);
        self.refresh_service_registry()?;
        let registry=self.registry.clone();
        self.faults.remove(&(slot as u32+1));
        if let Some(worker)=self.fine_worker.as_mut() {worker.state.registry=registry;worker.sync_lanes()?;worker.sync_directs(&mut self.direct)?;}
        Ok(frames)
    }

    pub(super) fn refresh_service_registry(&mut self)->Result<(),String> {
        let configs=self.service_configs.as_ref().ok_or("service configs missing")?;
        let mut registry=Vec::new();
        for c in configs.iter().filter(|c|c.enabled) {
            let (id,rate,delay)=if let Some(route)=self.direct.get(&c.channel) {
                (route.id,route.rate,route.delay)
            } else {
                let id=if c.channel==1 {self.channel_id} else {self.aux.get(&c.channel).ok_or("aux identity missing")?.0};
                let (rate,delay)=self.bank.channel_metadata(id).ok_or("channel metadata missing")?;
                (id,rate,delay)
            };
            registry.push(sdr_pipeline::ChannelRegistryEntry{channel:c.channel,channel_id:id as u32,
                epoch:self.stream.epoch() as u32,frequency_hz:c.freq,output_rate:rate,
                input_rate:self.stream.input_sample_rate(),group_delay_samples:delay,state:"active"});
        }
        self.registry=registry.into();
        Ok(())
    }
}

#[cfg(test)]
mod source_plan_tests {
    use super::*;

    #[test]
    fn remembered_mode_basebands_validate_at_every_capture_rate() {
        for mode in ["am", "dsb", "usb", "lsb", "cw", "nfm", "wfm", "pm", "ft8", "sstv", "drm", "drmplus", "radiofax"] {
            let descriptor = websa_channelizer::catalog::mode(mode).unwrap();
            for width in descriptor.bandwidth_presets_hz {
                let mut config = ChannelConfig { mode: mode.into(), if_bw: width, ..ChannelConfig::default() };
                let original = channel_plan(COARSE_RATE, &config).unwrap();
                config.baseband = Some(receiver_workers::BasebandConfig {
                    passband_hz: original.occupied_bandwidth_hz,
                    transition_hz: original.transition_hz,
                    minimum_rate_hz: 1.0,
                });
                config.validate().unwrap_or_else(|e| panic!("{mode} {width}: {e}"));
                for fs in [976_562.5, 3_906_250.0, 62_500_000.0] {
                    source_plan(fs, &config).unwrap_or_else(|e| panic!("{mode} {width} at {fs}: {e}"));
                }
            }
        }
    }

    #[test]
    fn rtty_upper_sideband_plan_retains_both_protocol_tones() {
        use websa_channelizer::{channel_open_planned,channel_process_float,channel_close};
        for mode in ["rtty","rtty-reverse"] {
            let mut config=ChannelConfig::for_slot(1);
            config.mode=mode.into();
            config.if_bw=3_400.0;
            let plan=channel_plan(COARSE_RATE,&config).unwrap();
            assert_eq!(plan.occupied_bandwidth_hz,6_800.0);
            for tone in [2_125.0,2_295.0] {
                let handle=channel_open_planned(COARSE_RATE,0.0,&plan);
                assert!(!handle.is_null());
                let input:Vec<f32>=(0..32_768).flat_map(|index| {
                    let phase=2.0*std::f64::consts::PI*tone*index as f64/COARSE_RATE;
                    [phase.cos() as f32,phase.sin() as f32]
                }).collect();
                let capacity=32_768/plan.decimate as usize+2;
                let mut output=vec![0.0;2*capacity];
                let count=unsafe {channel_process_float(handle,input.as_ptr(),32_768,
                    output.as_mut_ptr(),capacity as i32)};
                unsafe {channel_close(handle);}
                assert!(count>512);
                let power=output[2*512..2*count as usize].chunks_exact(2)
                    .map(|iq|f64::from(iq[0]).powi(2)+f64::from(iq[1]).powi(2))
                    .sum::<f64>()/(count as usize-512) as f64;
                assert!((0.98..1.02).contains(&power),"{mode} tone {tone} power {power}");
            }
        }
    }
}
