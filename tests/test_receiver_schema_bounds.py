"""Portable clock fields match the native u64 and nonzero-denominator contract."""
from __future__ import annotations

import json
import random
from pathlib import Path

import pytest
from jsonschema import Draft202012Validator

SCHEMA = json.loads((Path(__file__).resolve().parent / 'fixtures/multichannel/draft-v1.schema.json').read_text())
U64_MAX = (1 << 64) - 1


def integer_oracle(value, *, positive=False):
    # Independent arithmetic oracle: do not reimplement the schema's range regex.
    return (isinstance(value, str) and value.isascii() and value.isdecimal()
            and (value == '0' or not value.startswith('0'))
            and (1 if positive else 0) <= int(value) <= U64_MAX)


def candidates():
    edges = {0, 1, 2**53 - 1, 2**53, 2**53 + 1, U64_MAX - 1, U64_MAX, U64_MAX + 1}
    for exponent in range(1, 23):
        edges.update({10**exponent - 1, 10**exponent, 10**exponent + 1})
    for exponent in range(20):
        quantum = 10**exponent
        lower = (U64_MAX // quantum) * quantum
        edges.update({lower - 1, lower, lower + 1, lower + quantum - 1, lower + quantum})
        edges.update({U64_MAX - quantum - 1, U64_MAX - quantum, U64_MAX - quantum + 1})
    rng = random.Random(0x640C10C)
    edges.update(rng.getrandbits(65) for _ in range(160))
    return [*(str(value) for value in sorted(edges)), '', '-1', '-0', '+1', '00', '01',
            ' 1', '1 ', '1\n', '1\r\n', '1.0', '1e2', '１', '١', None, False, 0, U64_MAX]


def validator(rule):
    return Draft202012Validator({'$defs': SCHEMA['$defs'], **rule})


def test_unsigned_clock_strings_match_independent_integer_bounds():
    check = validator({'$ref': '#/$defs/u64'})
    for value in candidates():
        assert check.is_valid(value) == integer_oracle(value), f'u64: {value!r}'


@pytest.mark.parametrize('path', [
    ('$defs', 'clock', 'properties', 'fraction_den'),
    ('$defs', 'segment', 'properties', 'fraction_den'),
    ('$defs', 'processed_event', 'properties', 'input_fraction_den'),
    ('$defs', 'result_record', 'properties', 'envelope', 'properties', 'input_fraction_den'),
    ('$defs', 'audio_record', 'properties', 'audio', 'properties', 'input_fraction_den'),
    ('$defs', 'control_boundary', 'oneOf', 1, 'properties', 'fraction_den'),
])
def test_clock_denominators_are_nonzero_and_fit_u64(path):
    rule = SCHEMA
    for part in path:
        rule = rule[part]
    check = validator(rule)
    for value in candidates():
        assert check.is_valid(value) == integer_oracle(value, positive=True), f'{path}: {value!r}'
