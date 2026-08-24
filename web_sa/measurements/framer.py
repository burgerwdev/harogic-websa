"""
measurements/framer.py —— WS 二进制帧编解码 (协议版本化占位)

来源: web_sa/server.py (v0.11.1) send_frame 迁移。
关键: POWR 强制 float32, FREQ 保持 float64 (np.interp 会把 float32 变 float64, 否则前端解析错位)。
"""
from __future__ import annotations

import struct

import numpy as np

MAGIC_FREQ = b'FREQ'
MAGIC_POWR = b'POWR'
HEADER = struct.Struct('<IIf')     # version, points, sweep_ms (共 12 字节, 前缀 4 字节 magic)


def _frame(magic: bytes, version: int, points: int, sweep_ms: float, data, dtype) -> bytes:
    hdr = magic + HEADER.pack(version, points, sweep_ms)
    return hdr + np.ascontiguousarray(data, dtype=dtype).tobytes()


def encode_freq(version: int, freq_hz, sweep_ms: float) -> bytes:
    return _frame(MAGIC_FREQ, version, len(freq_hz), sweep_ms, freq_hz, np.float64)


def encode_powr(version: int, power_dbm, sweep_ms: float) -> bytes:
    return _frame(MAGIC_POWR, version, len(power_dbm), sweep_ms, power_dbm, np.float32)


def parse_header(buf: bytes):
    if len(buf) < 16:
        return None
    magic = buf[:4]
    ver, points = struct.unpack_from('<II', buf, 4)
    sweep_ms, = struct.unpack_from('<f', buf, 12)
    return magic, ver, points, sweep_ms


def decode_powr(buf: bytes, points: int) -> np.ndarray:
    return np.frombuffer(buf, dtype=np.float32, offset=16, count=points)
