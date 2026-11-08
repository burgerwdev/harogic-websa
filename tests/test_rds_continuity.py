"""Independent sums and message epochs for the RDS transmitter reference."""
import numpy as np

from tools.pluto import rds_encode as rds


def test_odd_message_rotation_flips_the_text_flag_on_wrap():
    flags = []
    for epoch in range(7):
        bits = rds.station_group(0x1234, 0, '', ['A', 'B', 'C'], 2,
                                 epoch * 2 * 16, ab_toggle=True)
        # Group B's 16 information bits precede its ten check bits.
        word = int(''.join(str(int(v)) for v in bits[26:42]), 2)
        flags.append((word >> 4) & 1)
    assert flags == [0, 1, 0, 1, 0, 1, 0]


def test_baseband_sums_every_symbol_within_the_declared_pulse_support():
    bits = np.array([1, 0, 1, 1, 0, 1, 0, 0, 1, 0], dtype=np.uint8)
    positions = np.array([3.1, 3.5, 3.8, 3.99, 4.1])
    # Sum complete independent symbol shifts rather than choosing neighbours by
    # the current sample's floor. The shared pulse kernel is tested separately.
    expected = sum((1 if bit else -1) * rds.symbol(positions - index)
                   for index, bit in enumerate(bits))
    actual = rds.baseband(positions / rds.SYMBOL_RATE, bits)
    np.testing.assert_allclose(actual, expected, atol=1e-12, rtol=0)
