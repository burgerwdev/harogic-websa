//! Mode E / DRM+ frequency-offset regression against a locally synthesised 96 kHz
//! baseband. The fixture carries FAC, SDC and an AAC stream; the browser PCM gate
//! is in frontend/src/__tests__/drmPlusAudio.test.ts.
use websa_dsp::digital::drm::DrmReceiver;

const IQ: &[u8] = include_bytes!("../../tests/fixtures/drm/drm_modeE_so0_96k_aac.f32");

#[test]
fn mode_e_survives_fractional_carrier_offset() {
    let baseband: Vec<f32> = IQ.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    for hz in [-100.0_f64, 0.0, 100.0] {
        let mut rx = DrmReceiver::new_at(96_000);
        for (block, iq) in baseband.chunks(3248 * 2).enumerate() {
            let mut shifted = iq.to_vec();
            for (i, c) in shifted.chunks_exact_mut(2).enumerate() {
                let phase = 2.0 * std::f64::consts::PI * hz * (block * 3248 + i) as f64 / 96_000.0;
                let (s, co) = phase.sin_cos();
                let (re, im) = (f64::from(c[0]), f64::from(c[1]));
                c[0] = (re * co - im * s) as f32;
                c[1] = (re * s + im * co) as f32;
            }
            rx.push(&shifted);
            rx.run();
        }
        eprintln!("offset {hz} Hz: FAC {}/{}, SDC {}, MSC {}, AU {}",
            rx.facs.len(), rx.fac_errors, rx.sdc_ok, rx.msc_frames.len(), rx.audio_access_units.len());
        assert!(rx.facs.len() >= 10, "{hz} Hz: FAC must decode");
        assert!(rx.sdc_ok >= 2, "{hz} Hz: SDC must decode");
        assert!(rx.audio_access_units.len() >= 10, "{hz} Hz: AAC must deframe");
    }
}
