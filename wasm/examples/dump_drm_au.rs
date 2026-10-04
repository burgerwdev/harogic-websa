//! Dump the live-capture audio access units and the SDC type-9 bytes, for cross-checking
//! the AU bytes against DecDRM's decoder:
//!
//!   cargo run --release --example dump_drm_au
//!
//! Writes /tmp/drm_live_au.bin: `type9_len u32 | type9 | records...`, one record per AU:
//! `crc u8 | len u32 LE | data...`.
use websa_dsp::ddc::resampler::ComplexResampler;
use websa_dsp::digital::drm::DrmReceiver;

const FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/drm/drm_live_modeB_so3_48828.f32");
const PUBLISHED_RATE: f64 = 48_828.125;
const CORE_RATE: f64 = 48_000.0;

fn main() {
    let iq: Vec<f32> = FIXTURE
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let mut resampler = ComplexResampler::new(PUBLISHED_RATE, CORE_RATE);
    let mut converted = Vec::new();
    resampler.process_f32_into(&iq, &mut converted);
    let mut rx = DrmReceiver::new();
    rx.push(&converted);
    rx.run();
    println!(
        "locked={} label={:?} aus={} msc_frames={}",
        rx.locked(),
        rx.station_label,
        rx.audio_access_units.len(),
        rx.msc_frames.len()
    );
    let (Some(audio), Some(mux)) = (&rx.audio, &rx.multiplex) else {
        panic!("no SDC audio/multiplex description");
    };
    let type9 = audio.to_type9_bytes();
    let mut out = Vec::new();
    out.extend_from_slice(&(type9.len() as u32).to_le_bytes());
    out.extend_from_slice(&type9);
    for au in &rx.audio_access_units {
        // au is [crc][data]; keep the record split.
        out.push(au[0]);
        out.extend_from_slice(&((au.len() - 1) as u32).to_le_bytes());
        out.extend_from_slice(&au[1..]);
    }
    std::fs::write("/tmp/drm_live_au.bin", &out).unwrap();
    println!("type9={:02x?} written {} AUs, {} bytes", type9, rx.audio_access_units.len(), out.len());
}
