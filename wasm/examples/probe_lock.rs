// Quick probe: streaming lock quality vs whole-buffer lock quality.
use websa_dsp::ddc::resampler::ComplexResampler;
use websa_dsp::digital::drm::DrmReceiver;

const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/drm/drm_live_modeB_so3_48828.f32");
fn load_iq() -> Vec<f32> {
    FIXTURE.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn main() {
    let iq = load_iq();
    let mut resampler = ComplexResampler::new(48_828.125, 48_000.0);
    let mut converted = Vec::new();
    resampler.process_f32_into(&iq, &mut converted);
    println!("converted samples: {}", converted.len());

    let mut rx = DrmReceiver::new();
    let mut fed: u64 = 0;
    for block in converted.chunks(3248 * 2) {
        rx.push(block);
        rx.run();
        fed += block.len() as u64;
        if rx.locked() {
            println!(
                "streaming: locked at {:.2} s: snr={:?} offset={:?} mode={:?} facs={} fac_errors={} label={:?}",
                fed as f64 / 2.0 / 48000.0,
                rx.snr_db,
                rx.carrier_offset_hz,
                rx.mode,
                rx.facs.len(),
                rx.fac_errors,
                rx.station_label
            );
            break;
        }
    }
    // Continue streaming to the end; print state after each second.
    let mut last_report = 0;
    while (fed as usize) < converted.len() {
        let pos = fed as usize;
        let end = (pos + 3248 * 2).min(converted.len());
        rx.push(&converted[pos..end]);
        rx.run();
        fed = end as u64;
        if pos / 2 / 48000 != last_report {
            last_report = pos / 48000;
            println!(
                "t={:.1}s snr={:?} au={} pcm={} facs={} fac_err={}",
                pos as f64 / 2.0 / 48000.0, rx.snr_db, rx.audio_access_units.len(), rx.audio_pcm.len(), rx.facs.len(), rx.fac_errors
            );
        }
    }

    let mut full = DrmReceiver::new();
    full.push(&converted);
    full.run();
    println!(
        "whole: locked={} snr={:?} offset={:?} facs={} fac_errors={} au={} pcm={} sdc_ok={}",
        full.locked(), full.snr_db, full.carrier_offset_hz, full.facs.len(), full.fac_errors,
        full.audio_access_units.len(), full.audio_pcm.len(), full.sdc_ok
    );
}
