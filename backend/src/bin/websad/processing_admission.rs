//! Geometry and publication diagnostics. Selected models prepare on their owner.
use super::*;
use receiver_workers::ChannelConfig;

pub(super) fn prepare(configs:&[ChannelConfig],fs:f64)->Result<Value,String> {
    let mut rows=Vec::new();let mut bytes=0.0;
    for config in configs.iter().filter(|config|config.enabled) {
        config.validate()?;
        let rate=service_ddc::source_plan(fs,config)?.plan().output_rate;
        let iq=rate*8.0;bytes+=iq;
        let descriptor=websa_channelizer::catalog::mode(&config.mode);
        let audio=if config.processing_enabled && descriptor.as_ref().is_some_and(|descriptor|
            descriptor.outputs.iter().any(|kind|kind=="audio")||descriptor.audio_companion.is_some()) {
            config.audio_rate as f64*4.0*if config.mode=="wfm"&&config.stereo {2.0} else {1.0}
        } else {0.0};bytes+=audio;
        rows.push(json!({"channel":config.channel,"worker":receiver_workers::worker_for(config.channel),
            "actual_input_rate":rate,"mode":config.mode,"nr":config.nr,"nr_algorithm":config.nr_algorithm,
            "processor_enabled":config.processing_enabled,"cf32_bytes_per_second_if_subscribed":iq,
            "audio_bytes_per_second_if_published":audio,"predicted_processing_ns_per_second":null}));
    }
    Ok(json!({"accepted":true,"policy":"observed_runtime","channels":rows,
        "publication_bytes_per_second":bytes,"publication_limit_bytes_per_second":null,
        "worker_limit_ns_per_second":null,"heavy_limits":null,
        "diagnostic_precision":"payload at actual rates; subscription framing and protocol results vary",
        "host_resources":websa_channelizer::host_resources::snapshot()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_does_not_prepare_models_or_reject_legacy_908_629_budget() {
        let configs:Vec<_>=(1..=MAX_CHANNELS as u32).map(|channel|ChannelConfig {channel,enabled:true,freq:100e6,
            nr:true,nr_algorithm:["dfn","gtcrn","ul-unas","fastenhancer"][(channel as usize-1)%4].into(),
            ..ChannelConfig::default()}).collect();
        let before=websa_channelizer::model_preparation::snapshot();let settings=configs.clone();
        let report=prepare(&configs,IQ_FS).unwrap();
        assert!(report["accepted"].as_bool().unwrap());assert!(report["worker_limit_ns_per_second"].is_null());
        assert_eq!(configs,settings);assert_eq!(websa_channelizer::model_preparation::snapshot(),before);
        assert_eq!(report["channels"].as_array().unwrap().len(),MAX_CHANNELS);
    }
    #[test]
    fn multiple_searches_images_and_high_iq_rates_have_no_prediction_gate() {
        for mode in ["ft8","sstv","radiofax","drm"] {
            let width=websa_channelizer::catalog::mode(mode).unwrap().default_bandwidth_hz;
            let configs:Vec<_>=(1..=MAX_CHANNELS as u32).map(|channel|ChannelConfig {channel,enabled:true,freq:100e6,
                mode:mode.into(),if_bw:width,..ChannelConfig::default()}).collect();
            assert_eq!(prepare(&configs,IQ_FS).unwrap()["channels"].as_array().unwrap().len(),MAX_CHANNELS);
        }
        let configs:Vec<_>=(1..=MAX_CHANNELS as u32).map(|channel|ChannelConfig {channel,enabled:true,freq:100e6,processing_enabled:false,
            baseband:Some(receiver_workers::BasebandConfig {passband_hz:500000.0,transition_hz:10000.0,minimum_rate_hz:900000.0}),
            ..ChannelConfig::default()}).collect();
        let report=prepare(&configs,IQ_FS).unwrap();assert!(report["publication_bytes_per_second"].as_f64().unwrap()>32.0*1024.0*1024.0);
    }
}
