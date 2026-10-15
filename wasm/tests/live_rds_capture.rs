//! The RDS side chain against a real on-air capture.
//!
//! The synthetic fixtures pin the decoder's logic, but they cannot pin what a real receiver
//! chain does to the signal: the device's DDC filters and decimates before the browser sees a
//! sample, and the discriminator's noise rises with frequency, so the 57 kHz subcarrier arrives
//! with far less margin than a fixture has. This test runs a capture taken from the analyzer
//! (`tools/bench/hil_audio_check.py --mode wfm ...`) through the committed kernels and reports
//! what the side chain made of it.
//!
//! Skipped unless `WEBSA_RDS_IQ` names a capture sidecar (the JSON written by the tool; the raw
//! IQ sits next to it in `<name>.iq`):
//!
//! ```text
//! python3 tools/bench/hil_audio_check.py --mode wfm --frequency 99.0027e6 --seconds 3 \
//!     --tinysa-port '' --decimate 16 --ifbw 180000 --out /tmp/hil_wfm_rds.json
//! WEBSA_RDS_IQ=/tmp/hil_wfm_rds.json cargo test --release --test live_rds_capture -- --nocapture
//! ```
use websa_dsp::analog::build;
use websa_dsp::plugin::{AnalogDemodulator, DemodConfig};

/// The capture sidecar: only the fields this test needs.
fn parse_meta(text: &str) -> (f64, f64, f64) {
    let field = |key: &str| -> f64 {
        text.split(&format!("\"{}\":", key))
            .nth(1)
            .and_then(|rest| {
                let rest = rest.trim_start();
                let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == 'e' || c == 'E')).unwrap_or(rest.len());
                rest[..end].parse().ok()
            })
            .unwrap_or(0.0)
    };
    (field("fs_in"), field("if_bw"), field("out_rate"))
}

#[test]
fn real_capture_yields_stereo_and_rds() {
    let Some(path) = std::env::var("WEBSA_RDS_IQ").ok() else {
        eprintln!("skipped: set WEBSA_RDS_IQ to a capture sidecar");
        return;
    };
    let meta = std::fs::read_to_string(&path).expect("read the capture sidecar");
    let (fs, if_bw, out_rate) = parse_meta(&meta);
    assert!(fs > 0.0 && if_bw > 0.0, "the sidecar must carry fs_in and if_bw");
    let iq_path = path.replace(".json", ".iq");
    let bytes = std::fs::read(&iq_path).expect("read the capture IQ");
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    assert!(samples.len() > 200_000, "too short a capture: {}", samples.len());

    let mut demod = build("wfm", DemodConfig::new(fs, if_bw), out_rate).expect("wfm builds");
    let block = 4_800_usize;
    let mut stereo = false;
    let mut peak = 0.0_f64;
    let mut ratio = 0.0_f64;
    let mut groups = 0_u32;
    let mut ps = String::new();
    let mut rt = String::new();
    let mut pi = 0_u16;
    for chunk in samples.chunks(block * 2) {
        let mut out = Vec::new();
        demod.process_into(chunk, &mut out);
        stereo = demod.stereo_detected();
        peak = demod.peak_deviation_hz();
        ratio = demod.pilot_ratio();
        if let Some(state) = demod.rds_state() {
            groups = state.groups;
            pi = state.pi;
            ps = state.ps_str();
            rt = state.rt_str();
        }
    }
    eprintln!(
        "capture {}: fs={fs:.0} if_bw={if_bw:.0} | stereo={stereo} peak_dev={peak:.0} Hz \
         pilot_ratio={ratio:.4} | rds groups={groups} PI={pi:#06x} PS={ps:?} RT={rt:?}",
        iq_path
    );
    if std::env::var("WEBSA_RDS_TORTURE").is_ok() {
        // Loop the capture for the equivalent of minutes of audio through ONE demodulator, the
        // way a browser session runs: a stall or a trap here is what kills the worker's stream.
        let mut out = Vec::new();
        let mut side_seen = 0_usize;
        let mut non_finite_audio = 0_usize;
        let mut non_finite_side = 0_usize;
        for pass in 0..60 {
            for chunk in samples.chunks(block * 2) {
                demod.process_into(chunk, &mut out);
                if out.iter().any(|v| !v.is_finite()) {
                    non_finite_audio += 1;
                }
                let side = demod.take_side();
                side_seen += side.len();
                if side.iter().any(|v| !v.is_finite()) {
                    non_finite_side += 1;
                }
            }
            let st = demod.rds_state().unwrap_or_default();
            eprintln!(
                "pass {pass:2}: groups={} pi={:#06x} stereo={} ratio={:.4} peak={:.0}",
                st.groups, st.pi, demod.stereo_detected(), demod.pilot_ratio(), demod.peak_deviation_hz()
            );
        }
        eprintln!("torture done: side samples={side_seen} non-finite audio blocks={non_finite_audio} side blocks={non_finite_side}");
    }
    // The capture's own expectation: a piloted transmission must declare stereo and decode RDS;
    // a mono one must stay mono and produce no groups. Testing both directions is the point —
    // a threshold that always says STEREO passes half of this and fails the bench.
    let expect_mono = std::env::var("WEBSA_RDS_EXPECT").map(|v| v == "mono").unwrap_or(false);
    if expect_mono {
        assert!(!stereo, "a transmission without a pilot must stay MONO (ratio={ratio:.4})");
        assert_eq!(groups, 0, "a mono transmission carries no RDS groups");
        assert!(ratio < 0.02, "a mono composite's pilot ratio must stay near zero (ratio={ratio:.4})");
    } else {
        assert!(
            ratio > 0.03,
            "the pilot must be detected on a real stereo broadcast (ratio={ratio:.4})"
        );
        assert!(stereo, "a piloted broadcast must declare stereo");
        assert!(groups > 0, "the RDS groups must decode on a real transmission");
        assert_eq!(pi, 0x1234, "the transmitter's PI code");
    }
}
