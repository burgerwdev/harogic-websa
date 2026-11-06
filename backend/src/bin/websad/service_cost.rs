//! Product planning validates real geometry. Throughput is observed at runtime;
//! controls never benchmark kernels or reserve a fixed fraction of one CPU.
use super::*;

pub(super) struct PreparedCosts;
pub(super) fn prepare_service(configs:&[receiver_workers::ChannelConfig],fs:f64,taps:usize,
    selected:usize,extra_bin:bool)->Result<PreparedCosts,String> {
    admit_service(configs,fs,taps,selected)?;
    if extra_bin {admit_service(configs,fs,taps,selected+1)?;}
    Ok(PreparedCosts)
}
pub(super) fn admit_service(configs:&[receiver_workers::ChannelConfig],fs:f64,taps:usize,selected:usize)->Result<(),String> {
    service_cost_report(configs,fs,taps,selected).map(|_|())
}
pub(super) fn admit_service_cached(configs:&[receiver_workers::ChannelConfig],fs:f64,taps:usize,selected:usize)->Result<(),String> {
    // Kept as an interface name for capture-boundary callers. There is no
    // predictive cache prerequisite, calibration or resource refusal.
    admit_service(configs,fs,taps,selected)
}
fn service_cost_report(configs:&[receiver_workers::ChannelConfig],fs:f64,taps:usize,selected:usize)->Result<Value,String> {
    if !fs.is_finite()||fs<=0.0||!matches!(taps,4|8|16) {return Err("fine/framer: invalid input geometry".into());}
    let bin_count=PFB_BINS.min((fs/BIN_SPACING).round() as i64).max(2) as usize;
    if selected==0||selected>bin_count {return Err("fine/framer: invalid selected-bin count".into());}
    let mut seen=std::collections::BTreeSet::new();let mut output=0.0;
    for config in configs {
        if !(1..=MAX_CHANNELS as u32).contains(&config.channel) {return Err("service channel exceeds capacity".into());}
        if !seen.insert(config.channel) {return Err("duplicate service channel".into());}
        if config.enabled {output+=super::service_ddc::source_plan(fs,config)?.plan().output_rate*8.0;}
    }
    Ok(json!({"policy":"observed_runtime","scope":"fine+framer","required_ns_per_second":null,
        "limit_ns_per_second":null,"reserve":null,"calibration_required":false,
        "planned_cf32_bytes_per_second":output,"publication_limit_bytes_per_second":null,
        "runtime_statistics":"receiver processing_statistics and source_watermarks"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_wide_channels_need_no_profile_or_prediction_budget() {
        let configs:Vec<_>=(1..=MAX_CHANNELS as u32).map(|channel|receiver_workers::ChannelConfig {channel,enabled:true,
            mode:"wfm".into(),freq:100e6,if_bw:250000.0,..Default::default()}).collect();
        let before=websa_channelizer::model_preparation::snapshot();
        prepare_service(&configs,IQ_FS,16,8,true).unwrap();
        admit_service_cached(&configs,IQ_FS,16,9).unwrap();
        let report=service_cost_report(&configs,IQ_FS,16,9).unwrap();
        assert!(report["limit_ns_per_second"].is_null());assert_eq!(before,websa_channelizer::model_preparation::snapshot());
        assert!(admit_service(&configs,IQ_FS,3,8).is_err());
        assert!(admit_service(&configs,IQ_FS,16,0).is_err());
    }
}
