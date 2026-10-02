//! Probe: run the DRM receiver over a raw interleaved f32 I/Q file at 48 kHz and
//! print what it decoded. Used for real off-air recordings (not part of CI).
use websa_dsp::digital::drm::DrmReceiver;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/tmp/drm_48k.f32".into());
    let bytes = std::fs::read(&path).expect("read fixture");
    let mut iq = Vec::with_capacity(bytes.len() / 4);
    for c in bytes.chunks_exact(4) {
        iq.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
    }
    println!("samples: {} ({} s at 48 kHz)", iq.len() / 2, iq.len() / 2 / 48_000);
    let mut rx = DrmReceiver::new();
    rx.push(&iq);
    rx.run();
    println!("locked: {}", rx.locked());
    println!("mode: {:?}  occupancy: {:?}", rx.mode, rx.occupancy);
    println!("frame_sync_score: {:.3}", rx.frame_sync_score);
    println!("symbols_demodulated: {}", rx.symbols_demodulated);
    println!("FAC blocks: {}  errors: {}", rx.facs.len(), rx.fac_errors);
    if let Some(fac) = rx.facs.first() {
        println!("  FAC[0]: frame {} so {} msc {:?} sdc {:?} interleave {:?} audio {} data {} service 0x{:x}",
            fac.channel.frame_index, fac.channel.occupancy.value(), fac.channel.msc_mode,
            fac.channel.sdc_mode, fac.channel.interleaving, fac.channel.num_audio, fac.channel.num_data,
            fac.service.service_id);
    }
    println!("SDC ok: {}  errors: {}  label: {:?}", rx.sdc_ok, rx.sdc_errors, rx.station_label);
    println!("audio: {:?}", rx.audio.as_ref().map(|a| (a.coding, a.sbr, a.mode, a.sample_rate)));
    println!("FAC SNR: {:?} dB", rx.snr_db);
    println!("MSC bitrate: {:?} kbps", rx.msc_bitrate_kbps);
    println!("MSC frames: {}  audio access units: {}  (wasm32 PCM: {} samples)",
        rx.msc_frames.len(), rx.audio_access_units.len(), rx.audio_pcm.len());
}
