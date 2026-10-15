//! Offline replay of the browser's exact input frames (WEBSA_BROWSER_DUMP=/path.json).
//!
//! The JSON comes from the bench rig (`tools/e2e/wfm_rig.py --dump-iq`): the frames the page's
//! demodulator actually consumed, at the rate the stream reported. A decoder change can then be
//! measured against a real capture without the radio. Skipped unless the env names one.

use websa_dsp::analog::build;
use websa_dsp::plugin::{AnalogDemodulator, DemodConfig};

#[test]
fn browser_dump_replay() {
    let path = std::env::var("WEBSA_BROWSER_DUMP").unwrap_or_default();
    if path.is_empty() {
        eprintln!("skipped: set WEBSA_BROWSER_DUMP");
        return;
    }
    let text = std::fs::read_to_string(&path).unwrap();
    // Minimal JSON pulls: rate, then the frames array-of-arrays.
    let rate: f64 = text
        .split("\"rate\":")
        .nth(1)
        .unwrap()
        .trim_start()
        .split(',')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let frames_start = text.find("\"frames\":").unwrap();
    let frames_text = &text[frames_start..];
    let mut frames: Vec<Vec<f32>> = Vec::new();
    let mut chars = frames_text.chars().peekable();
    let mut depth = 0usize;
    let mut in_frame = false;
    let mut current = String::new();
    while let Some(c) = chars.next() {
        match c {
            '[' => {
                depth += 1;
                if depth == 2 {
                    in_frame = true;
                    current.clear();
                }
            }
            ']' => {
                if in_frame && depth == 2 {
                    frames.push(
                        current
                            .split(',')
                            .map(|v| v.trim().parse::<f32>().unwrap())
                            .collect(),
                    );
                    in_frame = false;
                }
                depth -= 1;
            }
            _ if in_frame => current.push(c),
            _ => {}
        }
    }
    eprintln!("rate={rate} frames={}", frames.len());
    let mut config = DemodConfig::new(rate, 180_000.0);
    config.pitch = 700.0;
    let mut demod = build("wfm", config, 48_000.0).expect("wfm");
    let (mut groups_last, mut stereo_blocks, mut blocks) = (0u32, 0usize, 0usize);
    for frame in &frames {
        let mut out = Vec::new();
        demod.process_into(frame, &mut out);
        let _ = demod.take_side();
        blocks += 1;
        if demod.stereo_detected() {
            stereo_blocks += 1;
        }
        let g = demod.rds_state().map(|s| s.groups).unwrap_or(0);
        if g != groups_last || blocks % 10 == 0 {
            let d = demod.rds_diag();
            eprintln!(
                "block {blocks}: groups {g} stereo={} pll_f={:+.4}Hz ref={:.0} axisI={:.4} axisQ={:.4} bits={}",
                demod.stereo_detected(),
                demod.pll_freq_error_hz(),
                d[0], d[1], d[2], d[3] as u64
            );
            if g != groups_last {
                eprintln!(
                    "   pi={:#06x} ps={:?}",
                    demod.rds_state().map(|s| s.pi).unwrap_or(0),
                    demod.rds_state().map(|s| s.ps_str()).unwrap_or_default()
                );
            }
            groups_last = g;
        }
    }
    eprintln!(
        "RESULT blocks={blocks} stereo_blocks={stereo_blocks} groups={groups_last} ps={:?} rt={:?}",
        demod.rds_state().map(|s| s.ps_str()).unwrap_or_default(),
        demod.rds_state().map(|s| s.rt_str()).unwrap_or_default()
    );
}
