#!/usr/bin/env python3
"""Real vendor DSP over deterministic synthetic IQ. Opens no RF device.
Run with the browser-venv Python (numpy, matplotlib) after make sdk-shim.
"""

from __future__ import annotations

import argparse
import ctypes as c
import json
import re
import resource
import time
from pathlib import Path

import matplotlib
import numpy as np

matplotlib.use("Agg")
import matplotlib.pyplot as plt

FS = 62_500_000 / 64
CENTER = 100e6
SCALE = 1e-6
SIZES = [16240, 32768, 65536]


class Dsp:
    def __init__(self, library, size):
        self.lib, self.size = library, size
        self.handle, points, rbw = c.c_void_p(), c.c_uint(), c.c_double()
        status = library.san90_display_fft_open_v1(
            c.byref(self.handle), size, c.byref(points), c.byref(rbw)
        )
        assert status == 0, (size, status)
        self.points, self.rbw = points.value, rbw.value
        self.freq, self.power = np.zeros(self.points, "float64"), np.zeros(self.points, "float32")

    def process(self, iq):
        assert iq.dtype == np.int16 and iq.shape == (self.size, 2) and iq.flags.c_contiguous
        status = self.lib.san90_display_fft_execute_v1(
            self.handle,
            iq.ctypes.data,
            self.size,
            SCALE,
            CENTER,
            FS,
            FS * 0.8,
            self.freq.ctypes.data,
            self.power.ctypes.data,
            self.points,
        )
        assert status == 0 and np.isfinite(self.power).all(), status
        return self.freq.copy(), self.power.copy()

    def close(self):
        self.lib.san90_display_fft_close_v1(self.handle)


def library(path):
    lib = c.CDLL(str(path))
    lib.san90_display_fft_open_v1.argtypes = [
        c.POINTER(c.c_void_p),
        c.c_uint,
        c.POINTER(c.c_uint),
        c.POINTER(c.c_double),
    ]
    lib.san90_display_fft_execute_v1.argtypes = [
        c.c_void_p,
        c.c_void_p,
        c.c_uint,
        c.c_float,
        c.c_double,
        c.c_double,
        c.c_double,
        c.c_void_p,
        c.c_void_p,
        c.c_uint,
    ]
    lib.san90_display_fft_close_v1.argtypes = [c.c_void_p]
    return lib


def synthetic(duration=2.0):
    count = int(FS * duration)
    t = np.arange(count) / FS
    rng = np.random.default_rng(20261104)
    # Coloured noise from actual IQ, a quiet 8..14 kHz region inside a noisier capture.
    noise = rng.normal(size=count) + 1j * rng.normal(size=count)
    hz = np.fft.fftfreq(count, 1 / FS)
    shape = np.where((hz > 8000) & (hz < 14000), 0.08, 1.0)
    noise = np.fft.ifft(np.fft.fft(noise) * shape) * 120
    pair = [10000.0, 10000.0 + FS / 65536 * 10]
    z = noise.copy()

    def tone(f, a, gate=1):
        nonlocal z
        z += a * np.exp(2j * np.pi * f * t) * gate

    for f in pair:
        tone(f, 1200)
    tone(20000, 8000)
    tone(21000, 65)
    tone(12000, 0.8)
    cw = ((t % 0.32) < 0.08) | (((t + 0.16) % 0.64) < 0.08)
    tone(-10000, 2000, cw)
    fsk = np.where((t % 0.16) < 0.08, -30000.0, -28000.0)
    phase = np.cumsum(fsk) * 2 * np.pi / FS
    z += 1500 * np.exp(1j * phase)
    tone(-20000, 2500, (t % 0.4) < 0.02)
    # One genuine noise-floor change at 1 second.
    z[t >= 1] += noise[t >= 1] * 2
    iq = np.rint(np.column_stack((z.real, z.imag))).clip(-32768, 32767).astype("int16")
    return iq, pair


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--shim", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    args = p.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    lib = library(args.shim)
    iq, pair = synthetic()
    # Read the application's actual Classic Colour anchors, without another palette copy.
    source = (
        Path(__file__).resolve().parents[2] / "frontend/src/render/waterfallPalette.ts"
    ).read_text()
    body = source.split("classic: [", 1)[1].split("  blue:", 1)[0]
    anchors = np.array(
        [[float(v) for v in row.split(",")] for row in re.findall(r"\[([0-9., ]+)\]", body)]
    )
    from matplotlib.colors import ListedColormap

    ramp = (
        np.floor(
            np.stack(
                [
                    np.interp(np.linspace(0, 1, 1024), anchors[:, 0], anchors[:, i])
                    for i in [1, 2, 3]
                ],
                axis=1,
            )
            + 0.5
        )
        / 255
    )
    cmap = ListedColormap(ramp, name="WebSA Classic Colour")
    report = {
        "source": "deterministic synthetic IQ processed by installed SDK 0.55.100; no RF hardware",
        "sample_rate_hz": FS,
        "scale_to_v": SCALE,
        "input_complex_samples": len(iq),
        "seed": 20261104,
        "tiers": [],
    }
    fig, axs = plt.subplots(3, 2, figsize=(13, 10), layout="constrained")
    waves = []
    for index, size in enumerate(SIZES):
        dsp = Dsp(lib, size)
        # Same source samples and epoch, longer genuinely measured windows.
        f, power = dsp.process(np.ascontiguousarray(iq[int(0.6 * FS) : int(0.6 * FS) + size]))
        t = np.arange(size) / FS
        a = 10000
        tone = np.rint(
            np.column_stack(
                (a * np.cos(2 * np.pi * FS / 4096 * t), a * np.sin(2 * np.pi * FS / 4096 * t))
            )
        ).astype("int16")
        _, cal = dsp.process(tone)
        expected = 10 * np.log10((a * SCALE) ** 2 * 20)
        error = float(cal.max() - expected)
        assert abs(error) < 0.02, (size, error)
        times = []
        cpu = time.process_time()
        for _ in range(100):
            started = time.perf_counter()
            dsp.process(np.ascontiguousarray(iq[:size]))
            times.append(time.perf_counter() - started)
        cpu = (time.process_time() - cpu) / 100
        hop = max(size, int(FS / 20))
        starts = list(range(0, len(iq) - size + 1, hop))
        rows = np.array(
            [dsp.process(np.ascontiguousarray(iq[start : start + size]))[1] for start in starts]
        )
        sel = (f - CENTER > 9000) & (f - CENTER < 11200)
        axs[index, 0].plot((f[sel] - CENTER) / 1000, power[sel], lw=1.2, marker=".", ms=2)
        axs[index, 0].set(
            xlabel="RF offset (kHz)",
            ylabel="dBm",
            ylim=(-120, -25),
            title=f"{size} real samples · Δf {FS / size:.2f} Hz · ENBW {FS * dsp.rbw:.2f} Hz",
        )
        for v in pair:
            axs[index, 0].axvline(v / 1000, color="gray", lw=0.5, ls="--")
        # A single colour scale for all rows; observations never averaged in time.
        sel = (f - CENTER > -33000) & (f - CENTER < -7000)
        axs[index, 1].imshow(
            rows[:, sel],
            origin="lower",
            aspect="auto",
            interpolation="nearest",
            cmap=cmap,
            vmin=-110,
            vmax=-40,
            extent=(
                (f[sel][0] - CENTER) / 1000,
                (f[sel][-1] - CENTER) / 1000,
                (size - hop) / 2 / FS,
                (starts[-1] + (size + hop) / 2) / FS,
            ),
        )
        axs[index, 1].set(
            xlabel="RF offset (kHz)",
            ylabel="Window centre (s)",
            ylim=(0, 2),
            title="FSK (80 ms) · burst (20 ms) · CW (80 ms)",
        )
        # A resolved valley must lie between two local maxima near the known pair.
        sel = (f - CENTER > pair[0] - 80) & (f - CENTER < pair[1] + 80)
        y = power[sel]
        peaks = [
            i for i in range(1, len(y) - 1) if y[i] > y[i - 1] and y[i] >= y[i + 1] and y[i] > -55
        ]
        valley = None
        if len(peaks) >= 2:
            left, right = peaks[0], peaks[-1]
            valley = float(min(y[left], y[right]) - y[left : right + 1].min())
        report["tiers"].append(
            {
                "fft_samples": size,
                "output_points": dsp.points,
                "fft_bin_spacing_hz": FS / size,
                "sdk_output_spacing_hz": float(f[1] - f[0]),
                "enbw_hz": FS * dsp.rbw,
                "observation_ms": size / FS * 1000,
                "calibration_error_db": error,
                "resolved_pair_valley_db": valley,
                "mean_wall_ms": np.mean(times) * 1000,
                "p95_wall_ms": np.percentile(times, 95) * 1000,
                "cpu_ms_per_fft": cpu * 1000,
                "output_fps": len(rows) / 2,
                "rtaf_bytes_per_frame": dsp.points * 12 + 480,
                "full_band_network_mib_s_at_20fps": (dsp.points * 12 + 480) * 20 / 1024**2,
            }
        )
        waves.append((f, power, rows, starts))
        dsp.close()
    assert (
        report["tiers"][0]["resolved_pair_valley_db"] is None
        or report["tiers"][0]["resolved_pair_valley_db"] < 3
    )
    assert report["tiers"][-1]["resolved_pair_valley_db"] > 10
    fig.savefig(args.output / "fft-resolution-time.png", dpi=160)
    plt.close(fig)
    fig, axs = plt.subplots(3, 1, figsize=(11, 8), layout="constrained")
    for ax, size, (f, power, _, _) in zip(axs, SIZES, waves, strict=True):
        sel = (f - CENTER > 19000) & (f - CENTER < 22000)
        ax.plot((f[sel] - CENTER) / 1000, power[sel], marker=".", ms=2, lw=1)
        ax.set(
            xlabel="RF offset (kHz)",
            ylabel="dBm",
            ylim=(-145, -20),
            title=f"{size} samples: strong carrier + weak carrier 41.8 dB below it",
        )
    fig.savefig(args.output / "strong-weak.png", dpi=160)
    plt.close(fig)
    # Same IQ FFT: compare capture q30 with view q10 using native values.
    f, power, rows, starts = waves[1]
    view = (f - CENTER > 8000) & (f - CENTER < 14000)
    full = float(np.percentile(power, 30))
    local = float(np.percentile(power[view], 10))
    fig, axs = plt.subplots(2, 1, figsize=(11, 6), layout="constrained")
    for ax, floor, label in zip(
        axs, [full, local], ["Full capture (q30)", "Current view (q10)"], strict=True
    ):
        ax.imshow(
            rows[:, view],
            aspect="auto",
            interpolation="nearest",
            origin="lower",
            cmap=cmap,
            vmin=floor - 6,
            vmax=floor + 49,
            extent=(
                8,
                14,
                (32768 - int(FS / 20)) / 2 / FS,
                (starts[-1] + (32768 + int(FS / 20)) / 2) / FS,
            ),
        )
        ax.set(
            xlabel="RF offset (kHz)",
            ylabel="Window centre (s)",
            ylim=(0, 2),
            title=f"{label}: floor {floor:.1f} dBm; same raw observations",
        )
    fig.savefig(args.output / "view-floor-comparison.png", dpi=160)
    plt.close(fig)
    report["view_floor"] = {
        "capture_q30_dbm": full,
        "view_q10_dbm": local,
        "native_view_samples": int(view.sum()),
    }
    report["peak_process_rss_mib"] = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024
    (args.output / "synthetic.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
