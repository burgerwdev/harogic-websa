"""Bounded PNG/PPM decoding for user-supplied reference-tool pictures."""
import struct
import zlib

import pytest

from tools.fixtures import sstv_encode as image


def png(width=1, height=1, raw=b'\0\x10\x20\x30', interlace=0):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    header = struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, interlace)
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header)
            + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b''))


@pytest.mark.parametrize('data', [png(width=0), png(interlace=1), png(raw=b'\x05\x10\x20\x30'),
                                 png(raw=b'\0\x10\x20\x30extra'), png()[:-1]])
def test_png_rejects_unsupported_or_inconsistent_framing(data):
    with pytest.raises(ValueError):
        image._read_png(data)


def test_png_checks_dimensions_before_expanding_or_allocating(monkeypatch):
    real = bytearray
    def guarded(value=0):
        if isinstance(value, int) and value > 1024:
            raise MemoryError('test blocked unchecked dimension allocation')
        return real(value)
    monkeypatch.setattr(image, 'bytearray', guarded, raising=False)
    with pytest.raises(ValueError, match='image.*budget|dimensions'):
        image._read_png(png(width=2**32 - 1))


@pytest.mark.parametrize('data', [b'P6\n0 1\n255\n', b'P6\n2 1\n255\n\x10\x20\x30'])
def test_ppm_rejects_empty_dimensions_and_short_pixels(data):
    with pytest.raises(ValueError):
        image._read_ppm(data)


def test_supported_images_preserve_pixel_values_and_binary_whitespace():
    expected = b'\x10\x20\x30'
    assert image._read_png(png()) == (1, 1, expected)
    assert image._read_ppm(b'P6\n1 1\n255\n' + expected) == (1, 1, expected)
