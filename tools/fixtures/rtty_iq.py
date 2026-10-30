#!/usr/bin/env python3
"""Independent ITA2 UART/continuous-phase FSK fixture (no OnAir code or transmitter).

Writes interleaved little-endian complex float32 IQ plus provenance JSON. Tune the
receiver to the fixture dial, USB; normal mark is the lower audio tone. The optional
8 kHz PCM WAV is for an independent minimodem/fldigi receive oracle.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import wave

import numpy as np

# ITA2 US figures; protocol table, independent of the receive implementation.
LETTERS = "\x00E\nA SIU\rDRJNFCKTZLWHYPQOBG\x00MXV\x00"
FIGURES = ("\x00", "3", "\n", "-", " ", "'", "8", "7", "\r", "$", "4", "\x07",
           ",", "!", ":", "(", "5", '"', ")", "2", "#", "6", "0", "1", "9", "?", "&", "\x00", ".", "/", ";", "\x00")
DEFAULT_TEXT = "CQ CQ DE WEBSA TEST 0123456789 1/2 -.:? THE QUICK BROWN FOX\r\n"


def segments(text: str, baud: float, usos: bool = False) -> list[tuple[int, float]]:
    """Mark=1, start=0, five LSB-first data bits, 1.5 stop bits."""
    result: list[tuple[int, float]] = [(1, baud)]  # one second idle

    def character(code: int) -> None:
        result.append((0, 1.0))
        result.extend(((code >> bit) & 1, 1.0) for bit in range(5))
        result.append((1, 1.5))

    figures = False
    character(31)
    for char in text:
        if char in LETTERS and char != "\x00":
            code, needs_figures = LETTERS.index(char), False
        elif char in FIGURES and char != "\x00":
            code, needs_figures = FIGURES.index(char), True
        else:
            raise ValueError(f"Character cannot be encoded as ITA2: {char!r}")
        # Space/CR/LF have identical codes in both alphabets.
        if code not in (2, 4, 8) and needs_figures != figures:
            character(27 if needs_figures else 31)
            figures = needs_figures
        character(code)
        if usos and code == 4:
            figures = False
    result.append((1, baud))
    return result


def synthesize(parts: list[tuple[int, float]], rate: int, baud: float,
               low: float, shift: float, reverse: bool, amplitude: float) -> np.ndarray:
    total_bits = sum(length for _, length in parts)
    result = np.empty(round(total_bits * rate / baud), dtype=np.complex64)
    boundary = 0.0
    cursor = 0
    phase = 0.0
    for mark, length in parts:
        boundary += length * rate / baud
        end = round(boundary)
        frequency = low + (shift if bool(mark) == reverse else 0.0)
        step = 2 * np.pi * frequency / rate
        positions = np.arange(end - cursor, dtype=np.float64)
        result[cursor:end] = amplitude * np.exp(1j * (phase + positions * step))
        phase = (phase + (end - cursor) * step) % (2 * np.pi)
        cursor = end
    return result[:cursor]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="interleaved f32le IQ file")
    parser.add_argument("--rate", type=int, default=48000)
    parser.add_argument("--baud", type=float, choices=(45.45, 50, 75, 100), default=45.45)
    parser.add_argument("--shift", type=float, choices=(170, 200, 425, 450, 850), default=170)
    parser.add_argument("--low-tone", type=float, default=2125)
    parser.add_argument("--reverse", action="store_true")
    parser.add_argument("--usos", action="store_true")
    parser.add_argument("--text", default=DEFAULT_TEXT)
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--wav", type=Path, help="also write 8 kHz mono PCM WAV for an independent oracle")
    args = parser.parse_args()
    if args.rate < 8000 or not (300 <= args.low_tone <= 3300 - args.shift) or args.repeat < 1:
        parser.error("rate >=8000, pair within 300..3300 Hz, repeat >=1 required")
    text = args.text.upper()
    # Acquisition warm-up exercises both tones before the acceptance payload.
    preamble = "RY" * 20 + "\r\n"
    parts = segments(preamble + text * args.repeat, args.baud, args.usos)
    iq = synthesize(parts, args.rate, args.baud, args.low_tone, args.shift, args.reverse, 0.5)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    iq.astype("<c8", copy=False).tofile(args.output)
    metadata = {"generator": "independent ITA2 UART + continuous-phase FSK", "format": "cf32_le",
                "sampleRateHz": args.rate, "dialOffsetHz": 0, "sideband": "upper", "samples": len(iq),
                "baud": args.baud, "shiftHz": args.shift, "lowToneHz": args.low_tone,
                "markHz": args.low_tone + (args.shift if args.reverse else 0),
                "spaceHz": args.low_tone + (0 if args.reverse else args.shift),
                "reverse": args.reverse, "unshiftOnSpace": args.usos, "preamble": preamble,
                "expectedText": text.replace("\r", ""), "transmittedText": text,
                "repetitions": args.repeat, "hardwareAccess": False}
    args.output.with_suffix(args.output.suffix + ".json").write_text(json.dumps(metadata, indent=2) + "\n")
    if args.wav:
        audio = synthesize(parts, 8000, args.baud, args.low_tone, args.shift, args.reverse, 0.5).real
        with wave.open(str(args.wav), "wb") as wav:
            wav.setparams((1, 2, 8000, 0, "NONE", "not compressed"))
            wav.writeframes(np.rint(audio * 32767).astype("<i2").tobytes())
    print(json.dumps(metadata))


if __name__ == "__main__":
    main()
