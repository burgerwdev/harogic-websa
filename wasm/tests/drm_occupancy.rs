//! DRM30 bandwidth acquisition: the same mode B signal in 4.5 and 20 kHz occupancies
//! must follow its FAC layout rather than continuing with the default 10 kHz cell map.
use websa_dsp::ddc::resampler::ComplexResampler;
use websa_dsp::digital::drm::params::{RobustnessMode, SpectrumOccupancy};
use websa_dsp::digital::drm::DrmReceiver;

fn load(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
}

#[test]
fn mode_b_follows_fac_occupancy_in_batch_and_worker_blocks() {
    for (so, bytes) in [
        (SpectrumOccupancy::SO_0, include_bytes!("../../tests/fixtures/drm/drm_modeB_so0_48k_xhe.f32").as_slice()),
        (SpectrumOccupancy::SO_5, include_bytes!("../../tests/fixtures/drm/drm_modeB_so5_48k_xhe.f32").as_slice()),
    ] {
        let iq = load(bytes);
        let mut batch = DrmReceiver::new();
        batch.push(&iq);
        batch.run();
        let mut streamed = DrmReceiver::new();
        for chunk in iq.chunks(3248 * 2) {
            streamed.push(chunk);
            streamed.run();
        }
        for (kind, rx) in [("batch", &batch), ("blocks", &streamed)] {
            eprintln!("SO{} {kind}: mode={:?} occupancy={} FAC={}/{} SDC={} MSC={} AU={} label={:?}",
                so.value(), rx.mode, rx.occupancy().value(), rx.facs.len(), rx.fac_errors,
                rx.sdc_ok, rx.msc_frames.len(), rx.audio_access_units.len(), rx.station_label);
            assert_eq!(rx.mode, Some(RobustnessMode::B), "SO{} {kind}", so.value());
            assert_eq!(rx.occupancy(), so, "SO{} {kind}", so.value());
            assert!(rx.facs.len() >= 8, "SO{} {kind} FAC", so.value());
            assert!(rx.facs.iter().all(|fac| fac.channel.occupancy == so));
            assert!(rx.sdc_ok >= 1, "SO{} {kind} SDC", so.value());
            assert!(rx.multiplex.is_some() && rx.audio.is_some(), "SO{} {kind} SDC metadata", so.value());
            assert!(!rx.msc_frames.is_empty(), "SO{} {kind} MSC", so.value());
            assert!(!rx.audio_access_units.is_empty(), "SO{} {kind} xHE audio", so.value());
        }
        assert_eq!(streamed.msc_frames, batch.msc_frames, "SO{} MSC parity", so.value());
        assert_eq!(streamed.audio_access_units, batch.audio_access_units, "SO{} audio parity", so.value());
    }
}

#[test]
fn live_xhe_capture_survives_timing_tracking_startup() {
    let iq = load(include_bytes!("../../tests/fixtures/drm/drm_live_xhe_modeB_so3_48828.f32"));
    let mut resampler = ComplexResampler::new(48_828.0, 48_000.0);
    let mut baseband = Vec::new();
    let mut rx = DrmReceiver::new();
    for block in iq.chunks(3248 * 2) {
        resampler.process_f32_into(block, &mut baseband);
        rx.push(&baseband);
        rx.run();
    }
    eprintln!("xHE FAC={}/{} SDC={} MSC={} AU={} MER={:?}", rx.facs.len(), rx.fac_errors,
        rx.sdc_ok, rx.msc_frames.len(), rx.audio_access_units.len(), rx.snr_db());
    assert!(rx.facs.len() >= 13, "live xHE FAC lock");
    assert!(rx.audio_access_units.len() >= 40, "live xHE audio continuity");
}

#[test]
fn a_capture_spanning_different_occupancies_does_not_recurse_indefinitely() {
    let a = load(include_bytes!("../../tests/fixtures/drm/drm_modeB_so0_48k_xhe.f32"));
    let b = load(include_bytes!("../../tests/fixtures/drm/drm_modeB_so5_48k_xhe.f32"));
    let mut rx = DrmReceiver::new();
    rx.push(&a[..3 * 48_000 * 2]);
    rx.push(&b[..3 * 48_000 * 2]);
    rx.run();
    assert_eq!(rx.mode, Some(RobustnessMode::B));
    assert!(rx.buffered() <= 400_000);
}
