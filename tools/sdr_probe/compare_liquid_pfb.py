"""Offline 2x PFB comparison against liquid-dsp 1.7 (no instrument access).

Run after: cargo build --release --manifest-path native-ddc/Cargo.toml --lib
"""
import ctypes as C
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
FS = 62_500_000.0
M, HOP = 128, 64
BINS = (0, 1, 5, 15, 20, 31, 40, 45, 63, 64, 65, 80, 90, 100, 120, 127)


def compare(bin_id, offset, impulse_at=None):
    rust = C.CDLL(str(ROOT / "native-ddc/target/release/libwebsa_channelizer.so"))
    rust.pfb_open_kernel.argtypes = (C.c_double, C.c_uint32, C.c_uint32)
    rust.pfb_open_kernel.restype = C.c_void_p
    rust.pfb_process.argtypes = (C.c_void_p, C.c_void_p, C.c_int, C.c_void_p, C.c_int, C.c_void_p, C.c_int)
    rust.pfb_process.restype = C.c_int
    rust.pfb_close.argtypes = (C.c_void_p,)
    liquid = C.CDLL("libliquid.so.1")
    liquid.firpfbchr_crcf_create.argtypes = (C.c_uint, C.c_uint, C.c_uint, C.c_void_p)
    liquid.firpfbchr_crcf_create.restype = C.c_void_p
    liquid.firpfbchr_crcf_push.argtypes = (C.c_void_p, C.c_void_p)
    liquid.firpfbchr_crcf_execute.argtypes = (C.c_void_p, C.c_void_p)
    liquid.firpfbchr_crcf_destroy.argtypes = (C.c_void_p,)
    a = rust.pfb_open_kernel(FS, 2, 1)
    length = 2048  # Rust 62.5 MSps: M=128, 16 taps per phase.
    x = np.arange(length, dtype=np.float64) - (length - 1) / 2
    taps = (np.sinc(x / HOP) / HOP * np.kaiser(length, 8.6)).astype(np.float32)
    taps /= taps.sum()
    b = liquid.firpfbchr_crcf_create(M, HOP, 8, taps.ctypes.data)
    assert a and b
    try:
        n = np.arange(16240 * 3, dtype=np.float64)
        signed = bin_id if bin_id < M / 2 else bin_id - M
        phase = 2 * np.pi * (signed * FS / M + offset) * n / FS
        iq = np.empty(2 * len(n), dtype=np.int16)
        iq[::2] = np.rint(12000 * np.cos(phase)).astype(np.int16)
        iq[1::2] = np.rint(12000 * np.sin(phase)).astype(np.int16)
        if impulse_at is not None:
            iq[:] = 0
            iq[2 * impulse_at] = 12000
        selected = np.array([bin_id], dtype=np.uint32)
        rust_out = []
        at = 0
        for size in (777, 16240, len(n) - 17017):
            packet = np.ascontiguousarray(iq[2 * at:2 * (at + size)])
            out = np.zeros(2 * (size // HOP + 2), dtype=np.float32)
            groups = rust.pfb_process(a, packet.ctypes.data, size, selected.ctypes.data, 1,
                                      out.ctypes.data, out.size // 2)
            assert groups >= 0
            rust_out.extend(out[:2 * groups].view(np.complex64))
            at += size
        liquid_out = []
        block = np.empty(HOP, dtype=np.complex64)
        row = np.empty(M, dtype=np.complex64)
        for at in range(0, len(n) - HOP + 1, HOP):
            block.real = iq[2 * at:2 * (at + HOP):2]
            block.imag = iq[2 * at + 1:2 * (at + HOP):2]
            assert liquid.firpfbchr_crcf_push(b, block.ctypes.data) == 0
            assert liquid.firpfbchr_crcf_execute(b, row.ctypes.data) == 0
            liquid_out.append(row[bin_id])
        # L=2048; startup needs >32 output groups before stop-band measurement.
        return np.asarray(rust_out[40:]), np.asarray(liquid_out[40:])
    finally:
        rust.pfb_close(a)
        liquid.firpfbchr_crcf_destroy(b)


def test_liquid_reference():
    spacing = FS / M
    seen = set()
    for count in (1, 4, 8, 16):
        # Increasing prefixes correspond to the same IQ/bin matrix in the Rust direct-FIR test.
        for bin_id in BINS[:count]:
            if bin_id in seen:
                continue
            seen.add(bin_id)
            ours, reference = compare(bin_id, 1000.0)
            gain = np.vdot(reference, ours) / np.vdot(reference, reference)
            relative_error = np.sqrt(np.mean(abs(ours - gain * reference) ** 2)) / np.sqrt(np.mean(abs(ours) ** 2))
            assert relative_error < 0.001, (bin_id, relative_error)
            phase_a = np.angle(ours[1:] * ours[:-1].conj()).mean()
            phase_b = np.angle(reference[1:] * reference[:-1].conj()).mean()
            assert abs(phase_a - phase_b) < 0.002, (bin_id, phase_a, phase_b)
            print(f"bin={bin_id:3d} relative_RMS={relative_error:.4f} phase_delta={phase_a-phase_b:.5f}")
    for bin_id in (0, 5, 63, 64, 127):
        center, ref_center = compare(bin_id, 0.0)
        edge, ref_edge = compare(bin_id, 0.45 * spacing)
        alias, ref_alias = compare(bin_id, 1.8 * spacing)
        def rms(x):
            return np.sqrt(np.mean(abs(x) ** 2))

        measured = []
        for name, main, at_edge, neighbor in (("Rust", center, edge, alias),
                                               ("liquid", ref_center, ref_edge, ref_alias)):
            edge_db = 20 * np.log10(rms(at_edge) / rms(main))
            alias_db = 20 * np.log10(rms(neighbor) / rms(main))
            assert edge_db > -1.0 and alias_db < -100.0, (name, bin_id, edge_db, alias_db)
            measured.append((edge_db, alias_db))
            print(f"{name:6s} bin={bin_id:3d} edge={edge_db:.1f} dB alias={alias_db:.1f} dB")
        assert max(abs(a - b) for a, b in zip(*measured, strict=True)) < 1.0, (bin_id, measured)
    ours, reference = compare(127, 0.0, impulse_at=4096)
    peak_a, peak_b = np.argmax(abs(ours)), np.argmax(abs(reference))
    assert peak_a == peak_b, (peak_a, peak_b)
    print(f"impulse peak after 40-group warmup: Rust={peak_a}, liquid={peak_b}")


if __name__ == "__main__":
    test_liquid_reference()
