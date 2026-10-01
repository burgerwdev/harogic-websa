#!/usr/bin/env python3
"""Compare our FT8 decoder against the ft8_lib reference decoder on the same capture.

The acceptance question "does another receiver's decoder hear more than ours in this window" is
answered headless here: ft8_lib is the implementation lineage under WSJT-X and most FT8 apps
(FT8CN included), so running it on the *same* capture audio is the closest stand-in for a phone
side-by-side -- and, on a quiet band, two independent decoders both reporting zero is objective
proof that the band was silent rather than that the decoder missed.

    python3 tools/ft8_capture_check.py --frequency 7.074e6 --seconds 150
    python3 tools/ft8_reference_compare.py /tmp/ft8_capture.json

The reference side runs `tools/ft8_reference/build.sh` at its maximum search density
(time_osr 4, freq_osr 4); our side runs the shipped dsp.wasm through the browser pipeline via
`src/__tests__/ft8LiveReport.test.ts`, writing its per-slot report to `WEBSA_FT8_OUT`.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import wave
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent.parent
OUT_RATE = 48_000.0


def load_capture(json_path: str) -> tuple[np.ndarray, float]:
    meta = json.loads(Path(json_path).read_text())
    iq = np.fromfile(Path(json_path).parent / meta["iq_file"], dtype="<f4")
    fs_in = float(meta["fs_in"])
    real = iq[0::2].astype(np.float64)  # zero-IF capture of a real signal: I is the audio
    n_out = round(len(real) * OUT_RATE / fs_in)
    spectrum = np.fft.rfft(real)
    resampled = np.fft.irfft(spectrum, n_out)
    return resampled.astype(np.float32), fs_in


def write_wav(samples: np.ndarray, path: Path) -> None:
    peak = float(np.max(np.abs(samples))) or 1.0
    pcm = (samples / peak * 32_000).astype("<i2")
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(int(OUT_RATE))
        wav.writeframes(pcm.tobytes())


def run_reference(samples: np.ndarray) -> list[str]:
    # The reference's own buffer cap is 20 s (960k samples), so whole-burst coverage needs sliding:
    # 15 s windows stepped 2.25 s guarantee any 12.64 s burst falls complete inside at least one
    # window (step <= 15 - 12.64), whatever its phase against the capture grid.
    window = int(OUT_RATE * 15.0)
    step = int(OUT_RATE * 2.25)
    texts: list[str] = []
    for start in range(0, max(1, len(samples) - window + 1), step):
        wav = Path("/tmp/ft8_reference_compare.wav")
        write_wav(samples[start:start + window], wav)
        proc = subprocess.run(["bash", str(ROOT / "tools" / "ft8_reference" / "build.sh"),
                               str(wav), "4", "4", "48000"],
                              capture_output=True, text=True, check=True)
        for line in proc.stdout.splitlines():
            if "DECODED:" in line:
                texts.append(line.split("DECODED:", 1)[1].strip())
    return texts


def run_ours(json_path: str) -> list[str]:
    out = Path("/tmp/ft8_reference_compare_ours.txt")
    env = {"PATH": subprocess.os.environ.get("PATH", ""), "WEBSA_FT8_IQ": str(json_path),
           "WEBSA_FT8_OUT": str(out)}
    subprocess.run(["npx", "vitest", "run", "src/__tests__/ft8LiveReport.test.ts"],
                   cwd=ROOT / "frontend" / "modern", env=env, capture_output=True, text=True,
                   check=True)
    lines = out.read_text().splitlines()
    return [line.split(": ", 1)[1].split(" @ ")[0] for line in lines if line.startswith("slot ")]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("capture", help="capture JSON written by tools/ft8_capture_check.py")
    args = parser.parse_args()

    samples, fs_in = load_capture(args.capture)
    reference = run_reference(samples)
    ours = run_ours(args.capture)

    def tally(texts: list[str]) -> tuple[int, set[str]]:
        stations = set()
        for text in texts:
            stations.update(part for part in text.split() if any(c.isdigit() for c in part))
        return len(texts), stations

    ref_n, ref_stations = tally(reference)
    our_n, our_stations = tally(ours)
    print(f"capture: {args.capture} ({len(samples) / OUT_RATE:.1f} s of audio)")
    print(f"ft8_lib reference (time_osr 4, freq_osr 4): {ref_n} decodes, stations {sorted(ref_stations)}")
    print(f"websa dsp.wasm (shipped artifact):          {our_n} decodes, stations {sorted(our_stations)}")
    missed = sorted(ref_stations - our_stations)
    extra = sorted(our_stations - ref_stations)
    if missed:
        print(f"stations the reference decoded and we did not: {missed}")
    if extra:
        print(f"stations we decoded and the reference did not: {extra}")
    if not missed:
        print("parity: nothing the reference heard was missed by our decoder")
        return 0
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
