//! Numeric agreement with the Python reference DSP, stage by stage.
//!
//! The fixtures are produced by `tools/gen_dsp_fixtures.py`, which runs the *Python*
//! implementation (`web_sa/demod/filters.py` + `web_sa/measurements/sdr.py::_mix`) over a
//! committed IQ block. Each stage is compared separately so a failure names the stage that
//! drifted instead of "the DDC".
//!
//! The tolerance lives in the fixture manifest, so the two sides cannot disagree about how much
//! difference is acceptable without one of them changing.
use websa_dsp::ddc::{Ddc, DdcConfig, RmsAgc};

const IQ_BYTES: &[u8] = include_bytes!("../../tests/fixtures/dsp/iq_block.bin");
const MIX_BYTES: &[u8] = include_bytes!("../../tests/fixtures/dsp/mix.bin");
const FIR_BYTES: &[u8] = include_bytes!("../../tests/fixtures/dsp/fir.bin");
const DEC_BYTES: &[u8] = include_bytes!("../../tests/fixtures/dsp/dec.bin");
const RES_BYTES: &[u8] = include_bytes!("../../tests/fixtures/dsp/res.bin");
const AGC_BYTES: &[u8] = include_bytes!("../../tests/fixtures/dsp/agc.bin");
const MANIFEST: &str = include_str!("../../tests/fixtures/dsp/manifest.json");

const FS_IN: f64 = 1_000_000.0;
const OFFSET_HZ: f64 = 80_078.125;
const CUTOFF_HZ: f64 = 40_000.0;
const DECIMATE: usize = 10;
const OUT_RATE: f64 = 48_000.0;
const NTAPS: usize = 129;

fn tolerance() -> f64 {
    // The manifest is JSON without a parser dependency: read the one number that matters.
    let key = "\"tolerance\":";
    let start = MANIFEST.find(key).expect("manifest has a tolerance") + key.len();
    let rest = &MANIFEST[start..];
    let end = rest.find(',').unwrap_or(rest.len());
    rest[..end].trim().parse().expect("tolerance is a number")
}

fn as_i16(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

fn as_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|quad| f32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]))
        .collect()
}

/// Compare one stage; returns the worst absolute difference (printed on failure and on demand).
fn compare(stage: &str, got: &[f32], want: &[f32]) -> f64 {
    assert_eq!(got.len(), want.len(), "{stage}: length {} vs reference {}", got.len(), want.len());
    let mut worst = 0.0_f64;
    let mut at = 0;
    for (index, (a, b)) in got.iter().zip(want.iter()).enumerate() {
        let diff = (*a as f64 - *b as f64).abs();
        if diff > worst {
            worst = diff;
            at = index;
        }
    }
    let tol = tolerance();
    assert!(
        worst <= tol,
        "{stage}: worst difference {worst:e} at sample {at} ({} vs {}) exceeds the tolerance {tol:e}",
        got[at],
        want[at]
    );
    worst
}

fn config() -> DdcConfig {
    DdcConfig { fs_in: FS_IN, offset_hz: OFFSET_HZ, cutoff_hz: CUTOFF_HZ, decimate: DECIMATE, out_rate: OUT_RATE, ntaps: NTAPS }
}

#[test]
fn the_mixer_matches_the_python_reference() {
    let iq = as_i16(IQ_BYTES);
    let mut ddc = Ddc::new(config());
    let mut mixed = Vec::new();
    ddc.mix_i16_into(&iq, &mut mixed);

    let reference = as_f32(MIX_BYTES);
    let got: Vec<f32> = mixed.iter().map(|v| *v as f32).collect();
    let worst = compare("mixer", &got, &reference);
    println!("mixer: worst difference {worst:e}");
}

#[test]
fn the_anti_alias_filter_matches_the_python_reference() {
    let iq = as_i16(IQ_BYTES);
    let mut ddc = Ddc::new(config());
    let mut mixed = Vec::new();
    let mut filtered = Vec::new();
    ddc.mix_i16_into(&iq, &mut mixed);
    ddc.filter_into(&mixed, &mut filtered);

    let reference = as_f32(FIR_BYTES);
    let got: Vec<f32> = filtered.iter().map(|v| *v as f32).collect();
    let worst = compare("fir", &got, &reference);
    println!("fir: worst difference {worst:e}");
}

#[test]
fn the_decimator_matches_the_python_reference() {
    let iq = as_i16(IQ_BYTES);
    let mut ddc = Ddc::new(config());
    let mut mixed = Vec::new();
    let mut filtered = Vec::new();
    let mut decimated = Vec::new();
    ddc.mix_i16_into(&iq, &mut mixed);
    ddc.filter_into(&mixed, &mut filtered);
    ddc.decimate_into(&filtered, &mut decimated);

    let reference = as_f32(DEC_BYTES);
    let got: Vec<f32> = decimated.iter().map(|v| *v as f32).collect();
    let worst = compare("decimate", &got, &reference);
    println!("decimate: worst difference {worst:e}");
}

#[test]
fn the_resampler_matches_the_python_reference() {
    let iq = as_i16(IQ_BYTES);
    let mut ddc = Ddc::new(config());
    let mut mixed = Vec::new();
    let mut filtered = Vec::new();
    let mut decimated = Vec::new();
    let mut resampled = Vec::new();
    ddc.mix_i16_into(&iq, &mut mixed);
    ddc.filter_into(&mixed, &mut filtered);
    ddc.decimate_into(&filtered, &mut decimated);
    ddc.resample_into(&decimated, &mut resampled);

    let reference = as_f32(RES_BYTES);
    let worst = compare("resample", &resampled, &reference);
    println!("resample: worst difference {worst:e}");
}

#[test]
fn the_agc_matches_the_python_reference() {
    // The reference AGC runs on the real (audio-side) signal, so it is fed the reference's own
    // resampled real part: that isolates this stage from the ones above it.
    let reference_res = as_f32(RES_BYTES);
    let real: Vec<f32> = reference_res.chunks(2).map(|pair| pair[0]).collect();
    let mut agc = RmsAgc::reference();
    let mut out = Vec::new();
    agc.process_into(&real, false, &mut out);

    let reference = as_f32(AGC_BYTES);
    let worst = compare("agc", &out, &reference);
    println!("agc: worst difference {worst:e} (gain {})", agc.gain());
}

#[test]
fn the_whole_chain_through_one_call_agrees_with_the_reference() {
    // The end-to-end entry point the worker uses: int16 IQ in, levelled real samples out.
    let iq = as_i16(IQ_BYTES);
    let mut ddc = Ddc::new(config());
    let mut out = Vec::new();
    ddc.process_i16_to_real_into(&iq, &mut out);

    let reference = as_f32(AGC_BYTES);
    let worst = compare("full chain", &out, &reference);
    println!("full chain: worst difference {worst:e}");
}

#[test]
fn every_complex_stage_agrees_within_one_tolerance() {
    // A single guard that all four complex stages stay inside the documented bound, so the
    // tolerance is asserted as a property of the chain and not only per test.
    let iq = as_i16(IQ_BYTES);
    let mut ddc = Ddc::new(config());
    let mut mixed = Vec::new();
    let mut filtered = Vec::new();
    let mut decimated = Vec::new();
    let mut resampled = Vec::new();
    ddc.mix_i16_into(&iq, &mut mixed);
    ddc.filter_into(&mixed, &mut filtered);
    ddc.decimate_into(&filtered, &mut decimated);
    ddc.resample_into(&decimated, &mut resampled);

    let worst = [
        compare("mixer", &mixed.iter().map(|v| *v as f32).collect::<Vec<f32>>(), &as_f32(MIX_BYTES)),
        compare("fir", &filtered.iter().map(|v| *v as f32).collect::<Vec<f32>>(), &as_f32(FIR_BYTES)),
        compare("decimate", &decimated.iter().map(|v| *v as f32).collect::<Vec<f32>>(), &as_f32(DEC_BYTES)),
        compare("resample", &resampled, &as_f32(RES_BYTES)),
    ]
    .into_iter()
    .fold(0.0_f64, f64::max);
    println!("worst over the complex stages: {worst:e} (tolerance {})", tolerance());
}

#[test]
fn the_abi_handle_path_produces_the_reference_output() {
    // The exported functions are what the browser calls: allocate, fill, process, read back.
    use websa_dsp::abi::{websa_dsp_alloc, websa_dsp_free};
    use websa_dsp::ddc::abi::*;
    let iq = as_i16(IQ_BYTES);
    let handle = websa_dsp_ddc_new(FS_IN, OFFSET_HZ, CUTOFF_HZ, DECIMATE as u32, OUT_RATE, NTAPS as u32);
    assert_ne!(handle, 0, "the DDC handle must be created");

    let in_bytes = iq.len() * 2;
    let in_ptr = websa_dsp_alloc(in_bytes) as *mut i16;
    let capacity = 4096_u32;
    let out_ptr = websa_dsp_alloc(capacity as usize * 2 * 4) as *mut f32;
    assert!(!in_ptr.is_null() && !out_ptr.is_null(), "the loader must get its blocks");

    // SAFETY: both blocks were just allocated with the sizes used here.
    unsafe {
        core::ptr::copy_nonoverlapping(iq.as_ptr(), in_ptr, iq.len());
        let written = websa_dsp_ddc_process(handle, in_ptr, (iq.len() / 2) as u32, out_ptr, capacity);
        assert!(written > 0, "the chain must produce output");
        let got = core::slice::from_raw_parts(out_ptr, written as usize * 2).to_vec();
        let reference = as_f32(RES_BYTES);
        compare("abi output", &got, &reference);

        assert_eq!(websa_dsp_ddc_reset(handle), 1);
        assert_eq!(websa_dsp_ddc_retune(handle, OFFSET_HZ), 1);
        assert_eq!(websa_dsp_ddc_free(handle), 1);
        assert_eq!(websa_dsp_ddc_free(handle), 0, "a freed handle is gone");
        assert_eq!(websa_dsp_ddc_process(handle, in_ptr, 16, out_ptr, capacity), 0,
                   "a freed handle must not process");
        websa_dsp_free(in_ptr as *mut u8, in_bytes);
        websa_dsp_free(out_ptr as *mut u8, capacity as usize * 2 * 4);
    }
}

#[test]
fn the_abi_refuses_a_block_that_would_overflow_the_output() {
    use websa_dsp::abi::{websa_dsp_alloc, websa_dsp_free};
    use websa_dsp::ddc::abi::*;
    let iq = as_i16(IQ_BYTES);
    let handle = websa_dsp_ddc_new(FS_IN, OFFSET_HZ, CUTOFF_HZ, DECIMATE as u32, OUT_RATE, NTAPS as u32);
    let in_ptr = websa_dsp_alloc(iq.len() * 2) as *mut i16;
    let out_ptr = websa_dsp_alloc(2 * 4) as *mut f32;
    // SAFETY: both blocks are allocated above; the output is deliberately far too small.
    unsafe {
        core::ptr::copy_nonoverlapping(iq.as_ptr(), in_ptr, iq.len());
        let written = websa_dsp_ddc_process(handle, in_ptr, (iq.len() / 2) as u32, out_ptr, 1);
        assert_eq!(written, 0, "an output that cannot hold the block must be refused");
        websa_dsp_ddc_free(handle);
        websa_dsp_free(in_ptr as *mut u8, iq.len() * 2);
        websa_dsp_free(out_ptr as *mut u8, 8);
    }
}
