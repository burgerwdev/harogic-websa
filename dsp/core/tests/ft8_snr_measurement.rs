//! Same quantized GFSK+AWGN input as the independent WSJT-X jt9 decoder.
//! Prepare with tools/bench/compare_ft8_snr.py; never use enhanced audio as truth.
use websa_dsp::digital::ft8::{measure_decoded_snr, Ft8Decoder};
#[test]
#[ignore = "run tools/bench/compare_ft8_snr.py with unpacked WSJT-X jt9 and ft8sim"]
fn compare_identical_iq_against_wsjtx() {
    let path = std::path::PathBuf::from(
        std::env::var("WEBSA_FT8_REFERENCE_DIR").expect("reference directory"),
    );
    let manifest = include_str!("../../../tests/fixtures/ft8/manifest.json");
    let bits = manifest
        .split("\"codeword_bits\": [")
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap();
    let codeword: Vec<u8> = bits.split(',').map(|v| v.trim().parse().unwrap()).collect();
    let codeword: [u8; 174] = codeword.try_into().unwrap();
    let mut rows = String::new();
    for expected in [-20_i32, -16, -10, 0, 8] {
        let bytes = std::fs::read(path.join(format!("case_{expected:+03}.iq"))).unwrap();
        let iq: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
            .collect();
        let reference: f64 =
            std::fs::read_to_string(path.join(format!("case_{expected:+03}.reference")))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
        let mut decoder = Ft8Decoder::new(12000.0);
        for chunk in iq.chunks(274) {
            decoder.push_iq(chunk);
        }
        let decoded = decoder.decode();
        let message = decoded.iter().find(|m| m.text == "CQ JO1WKO PM95");
        let measurement = message.map(|m| m.snr.clone()).unwrap_or_else(|| {
            assert!(
                expected <= -20,
                "native must decode the stronger reference inputs: {expected}"
            );
            measure_decoded_snr(&iq, 12000.0, &codeword, 1000.37, 0.5)
        });
        let value = measurement
            .value
            .unwrap_or_else(|| panic!("{expected}: {measurement:?}"));
        let error = (value - expected as f64).abs();
        println!(
            "FT8 GFSK known={expected} native={value:.3} WSJT-X={reference} error={error:.3} dB"
        );
        assert!(
            error < if expected < -20 { 3.0 } else { 1.5 },
            "known injected SNR"
        );
        assert!(
            (value - reference).abs() < 3.0,
            "independent decoder comparison"
        );
        rows.push_str(&format!("{expected},{value:.6},{reference},{error:.6}\n"));
    }
    std::fs::write(path.join("native-results.csv"), rows).unwrap();
}
