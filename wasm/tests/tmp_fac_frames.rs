//! Scratch (temporary): find the carriers whose power is constant across symbols.
use websa_dsp::ddc::resampler::ComplexResampler;
use websa_dsp::digital::drm::cellmap::CellMap;
use websa_dsp::digital::drm::ofdm::{carrier_at, FullFft};
use websa_dsp::digital::drm::params::SpectrumOccupancy;
use websa_dsp::digital::drm::timesync;
use websa_dsp::digital::drm::Cplx;

const LIVE: &[u8] = include_bytes!("../../tests/fixtures/drm/drm_live_modeB_so3_48828.f32");
const FIX: &[u8] = include_bytes!("../../tests/fixtures/drm/drm_modeB_so3_48k.f32");

fn f32s(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn constant_carriers(name: &str, iq: &[f32]) {
    let buf: Vec<Cplx> = iq
        .chunks_exact(2)
        .map(|c| Cplx::new(f64::from(c[0]), f64::from(c[1])))
        .collect();
    let acq = timesync::acquire(&buf).expect("acquire");
    let mode = acq.mode;
    let map = CellMap::new(mode, SpectrumOccupancy::SO_3).expect("map");
    let n = mode.fft_size();
    let nc = map.num_carriers;
    let spsf = map.symbols_per_superframe;
    let spf = map.symbols_per_frame;
    let mut fft = FullFft::new(n);
    let mut rows: Vec<Vec<Cplx>> = Vec::new();
    let mut start = acq.guard_start + mode.guard_len();
    while start + n <= buf.len() {
        let mut row = vec![Cplx::new(0.0, 0.0); n];
        fft.forward(&buf[start..start + n], &mut row);
        rows.push(row);
        start += mode.symbol_len();
    }
    // Per carrier: mean power and relative standard deviation across symbols.
    let mut stats: Vec<(usize, f64, f64)> = Vec::new(); // (k, mean, rel_std)
    for c in 0..nc {
        let k = map.kmin + c as i32;
        let mut vals = Vec::with_capacity(rows.len());
        for row in &rows {
            vals.push(carrier_at(row, n, k).norm_sqr());
        }
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        let var = vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / vals.len() as f64;
        if mean > 0.0 {
            stats.push((k as usize, mean, var.sqrt() / mean));
        }
    }
    let mut sorted = stats.clone();
    sorted.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
    let steady: Vec<i32> = sorted.iter().take(20).map(|(k, _, _)| *k as i32).collect();
    // The expected pilot union for the best frame phase.
    let mut best = (0usize, 0.0f64);
    for r in 0..spf {
        let mut exp: Vec<i32> = Vec::new();
        for (i, _) in rows.iter().enumerate() {
            let sym = (i + r) % spsf;
            for c in 0..nc {
                let ty = map.cell(sym, c);
                if ty.is_scattered() && !ty.is_dc() {
                    exp.push(map.kmin + c as i32);
                }
            }
        }
        exp.sort_unstable();
        exp.dedup();
        let hit = steady.iter().filter(|k| exp.contains(k)).count();
        let frac = hit as f64 / steady.len() as f64;
        if frac > best.1 {
            best = (r, frac);
        }
    }
    println!("{name}: most constant carriers k = {steady:?}");
    println!("  best frame phase r={} : {:.0}% of them are expected pilots", best.0, best.1 * 100.0);
}

#[test]
fn constant_power() {
    let mut r = ComplexResampler::new(48_828.125, 48_000.0);
    let mut live = Vec::new();
    r.process_f32_into(&f32s(LIVE), &mut live);
    constant_carriers("live   ", &live);
    constant_carriers("fixture", &f32s(FIX));
}
