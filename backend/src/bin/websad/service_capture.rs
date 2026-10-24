use super::*;

pub(super) struct CaptureConfig {
    pub center: f64,
    pub display: i64,
    pub taps: usize,
    pub configs: Vec<receiver_workers::ChannelConfig>,
    pub device_decimate: i32,
    pub source_rate: f64,
    pub bandwidth: f64,
}

impl DevState {
    pub(super) fn parse_service_capture(&self,data:&Value)->Result<CaptureConfig,String> {
        if !data.is_object() {return Err("SDR configuration must be an object".into());}
        let finite=|name:&str,current:f64|->Result<f64,String>{
            match data.get(name) {None=>Ok(current),Some(v)=>v.as_f64().filter(|n|n.is_finite()).ok_or_else(||format!("{name} must be a finite number"))}
        };
        let taps=match data.get("pfb_taps_per_phase") {
            None=>self.sdr_pfb_taps_per_phase,
            Some(v)=>match v.as_u64() {Some(v @ (4|8|16))=>v as usize,_=>return Err("pfb_taps_per_phase must be 4, 8, or 16".into())}
        };
        let requested=finite("decimate",self.sdr_decimate as f64)?;
        let d=(requested as i64).clamp(1,2048);
        let display=1i64<<(63-d.leading_zeros());
        let requested_center=finite("center",self.sdr_center)?;
        self.fit_service_capture(requested_center,display,taps,self.channel_configs(),data.get("center").is_some())
    }

    pub(super) fn preset_service_channels() -> Vec<receiver_workers::ChannelConfig> {
        let mut configs: Vec<_> = (1..=3).map(receiver_workers::ChannelConfig::for_slot).collect();
        configs[0].freq = 1e9;
        configs
    }

    pub(super) fn preset_service_capture(&self) -> Result<CaptureConfig,String> {
        self.fit_service_capture(1e9,16,16,Self::preset_service_channels(),false)
    }

    pub(super) fn fit_service_capture(&self,requested_center:f64,display:i64,taps:usize,
        configs:Vec<receiver_workers::ChannelConfig>,pan:bool)->Result<CaptureConfig,String> {
        let (minimum,maximum)=self.freq_limits();
        let hardware=self.hardware_shim.is_some();
        let plan=service_ddc::channel_plan(COARSE_RATE,&configs[0])?;
        // Fit each candidate's actual window, never the previous capture's width.
        // A retained off-screen receiver can require a wider device capture.
        for power in (0..=6).rev() {
            let device_decimate=1<<power;
            if !hardware && device_decimate!=1 {continue;}
            let fs=IQ_FS/device_decimate as f64;
            let bandwidth=fs*0.8;
            let half=bandwidth/2.0;
            if display_half_width(display as i32)>half || bandwidth>maximum-minimum {continue;}
            let mut center=requested_center.clamp(minimum+half,maximum-half);
            let mut next=configs.clone();
            if (pan || center!=requested_center) && next[0].enabled {
                let occupied_half=half-plan.occupied_bandwidth_hz/2.0;
                let listen_half=if pan {display_half_width(display as i32).min(occupied_half)} else {occupied_half};
                if listen_half<0.0 {return Err("channel occupied bandwidth exceeds capture".into());}
                let lo=(center-listen_half).max(minimum);
                let hi=(center+listen_half).min(maximum);
                if pan && !(lo..=hi).contains(&next[0].freq) {
                    next[0].freq=service_ddc::fit_passband_frequency(center,next[0].freq,lo,hi,taps,&plan)?;
                } else {
                    // Panning a short-tap wide channel can enter a gap between coarse
                    // passbands. Fit the center, preserving the user's listen frequency.
                    center=service_ddc::fit_passband_frequency(next[0].freq,center,
                        (minimum+half).max(next[0].freq-listen_half),
                        (maximum-half).min(next[0].freq+listen_half),taps,&plan)?;
                }
            }
            let frequencies:Vec<_>=next.iter().filter(|c|c.enabled).map(|c|c.freq).collect();
            let listen=frequencies.first().copied().unwrap_or(center);
            let actual_decimate=if hardware {capture_decimate(center,listen,&frequencies,display as i32,taps)} else {1};
            if actual_decimate<device_decimate {continue;}
            let device_decimate=actual_decimate;
            let fs=IQ_FS/device_decimate as f64;
            let bandwidth=fs*0.8;
            for config in next.iter().filter(|c|c.enabled) {
                if !(minimum..=maximum).contains(&config.freq) {return Err("channel frequency outside device limits".into());}
                let plan=service_ddc::channel_plan(COARSE_RATE,config)?;
                Ddc::validate_service_frequency(center,bandwidth,fs,taps,config.freq,&plan)?;
            }
            return Ok(CaptureConfig {center,display,taps,configs:next,device_decimate,source_rate:fs,bandwidth});
        }
        Err("requested capture window exceeds device limits".into())
    }

    pub(super) fn apply_service_capture(&mut self,data:&Value)->Result<(),String> {
        let next=self.parse_service_capture(data)?;
        self.apply_service_capture_candidate(next,self.front_end(self.sdr_ref),false)
    }

    pub(super) fn apply_service_capture_candidate(&mut self,next:CaptureConfig,front_end:FrontEnd,
        force:bool)->Result<(),String> {
        let CaptureConfig {center,display,taps,configs,device_decimate,..}=next;
        let hardware=self.hardware_shim.is_some();
        let previous_decimate=self.pipeline.as_ref().map(|p|p.snapshot.device_decimate)
            .or_else(||self.ddc.as_ref().map(|d|d.0.device_decimate));
        let changed=force || center!=self.sdr_center || taps!=self.sdr_pfb_taps_per_phase
            || configs!=self.channel_configs()
            || (hardware && previous_decimate!=Some(device_decimate as u64));
        if changed {
            if hardware || self.ddc.is_some() || self.pipeline.is_some() {
                let next=configs.clone();
                self.ddc_command(move|ddc|ddc.recapture_service_configs(center,display as i32,front_end,taps,&next))?;
            } else {
                self.ddc_command(|_|Ok(Vec::new()))?;
                if self.fixture_sources.is_none() {self.fixture_sources=Some(service_fixture::FixtureSources::new());}
                let source=self.fixture_sources.as_mut().ok_or("fixture source unavailable")?;
                source.configure(&configs)?;
                source.restart(None)?;
            }
            self.invalidate_observation();
            self.channel_power=[None;3];
        }
        self.sdr_center=center;
        self.sdr_decimate=display;
        self.sdr_pfb_taps_per_phase=taps;
        let primary=&configs[0];
        self.sdr_listen=primary.freq;
        self.sdr_demod=primary.mode.clone();
        self.sdr_if_bw=primary.if_bw;
        self.sdr_squelch=primary.squelch;
        self.sdr_volume=primary.volume;
        self.sdr_agc=primary.agc;
        self.sdr_pitch=primary.pitch;
        self.sdr_deemph=primary.deemph_us;
        self.sdr_aux=json!({"2":configs[1],"3":configs[2]});
        self.sdr_channels=configs;
        self.config_version+=1;
        self.freq_version+=1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_frequency_widening_fits_requested_capture_not_previous_width() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({}));
        state.hardware_shim=Some("geometry-only".into());
        state.sdr_center=11e6;
        state.sdr_listen=11e6;
        state.sdr_decimate=2048;
        let narrow=state.parse_service_capture(&json!({"center":11e6,"decimate":2048})).unwrap();
        assert_eq!(narrow.center,11e6);
        assert_eq!(narrow.device_decimate,16);
        let wide=state.parse_service_capture(&json!({"decimate":1})).unwrap();
        assert_eq!(wide.bandwidth,50e6);
        assert_eq!(wide.center,state.freq_limits().0+25e6);
        assert_eq!(wide.configs[0].freq,11e6);
        assert_eq!(state.sdr_center,11e6);
        assert_eq!(state.sdr_decimate,2048);
    }

    #[test]
    fn pan_fits_primary_occupied_band_inside_capture_edge() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({}));
        state.sdr_center=100e6;
        state.sdr_listen=100e6;
        state.sdr_decimate=1;
        let next=state.parse_service_capture(&json!({"center":140e6,"decimate":1})).unwrap();
        let primary=&next.configs[0];
        let plan=service_ddc::channel_plan(COARSE_RATE,primary).unwrap();
        assert!(primary.freq>=115e6+plan.occupied_bandwidth_hz/2.0);
        assert!(primary.freq<116e6);
        Ddc::validate_service_frequency(next.center,next.bandwidth,next.source_rate,next.taps,primary.freq,&plan).unwrap();
        assert_eq!(state.sdr_listen,100e6);
    }

    #[test]
    fn short_tap_pan_fits_center_without_moving_listen_or_relaxing_tune_guard() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({}));
        state.sdr_center=100e6;
        state.sdr_listen=100e6;
        state.sdr_decimate=1;
        state.sdr_demod="wfm".into();
        state.sdr_if_bw=200_000.0;
        state.sdr_pfb_taps_per_phase=4;
        let requested=100e6+BIN_SPACING*0.49;
        let plan=service_ddc::channel_plan(COARSE_RATE,&state.channel_configs()[0]).unwrap();
        assert!(Ddc::validate_service_frequency(requested,50e6,IQ_FS,4,100e6,&plan).is_err());
        let next=state.parse_service_capture(&json!({"center":requested})).unwrap();
        assert_eq!(next.configs[0].freq,100e6);
        assert!((next.center-requested).abs()>1.0);
        assert!((next.center-requested).abs()<BIN_SPACING/2.0);
        Ddc::validate_service_frequency(next.center,next.bandwidth,next.source_rate,4,100e6,&plan).unwrap();
        let tune=state.parse_channel_config(&json!({"freq":requested})).unwrap();
        assert!(Ddc::validate_service_frequency(100e6,50e6,IQ_FS,4,tune.freq,&plan).is_err());
    }

    #[test]
    fn incompatible_auxiliary_pan_rejects_without_changing_capture_or_listen() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({}));
        state.sdr_center=100e6;
        state.sdr_listen=100e6;
        state.sdr_decimate=1;
        let mut configs=state.channel_configs();
        configs[1].enabled=true;
        configs[1].freq=90e6;
        state.sdr_channels=configs.clone();
        assert!(state.apply_service_capture(&json!({"center":140e6})).is_err());
        assert_eq!(state.sdr_center,100e6);
        assert_eq!(state.sdr_listen,100e6);
        assert_eq!(state.channel_configs(),configs);
        assert_eq!(state.config_version,0);
    }

    #[test]
    fn explicit_low_frequency_recapture_preserves_listen_outside_display() {
        let mut state=DevState::new(json!({"preset_defaults":{}}),json!({}));
        state.hardware_shim=Some("geometry-only".into());
        state.sdr_center=100e6;
        state.sdr_listen=100e6;
        state.sdr_decimate=2048;
        let config=state.parse_channel_config(&json!({"channel":1,"freq":1e6})).unwrap();
        let capture=state.channel_recapture(&config).unwrap().unwrap();
        assert_eq!(capture.configs[0].freq,1e6);
        assert!(capture.center-display_half_width(2048)>1e6);
        assert!(capture.center-capture.bandwidth/2.0>=state.freq_limits().0);
        let plan=service_ddc::channel_plan(COARSE_RATE,&config).unwrap();
        Ddc::validate_service_frequency(capture.center,capture.bandwidth,capture.source_rate,capture.taps,1e6,&plan).unwrap();
    }
}
