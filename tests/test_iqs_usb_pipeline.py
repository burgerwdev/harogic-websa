"""Exercise ordered, bounded USB delivery without a vendor SDK or physical radio."""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]


@pytest.mark.skipif(sys.platform != 'linux', reason='Linux SDK adapter')
def test_ordered_usb_reads_and_cancellation(tmp_path):
    compiler = shutil.which(os.environ.get('CXX', 'c++'))
    if not compiler:
        pytest.skip('C++ compiler required')
    probe = subprocess.run([compiler, '-x', 'c++', '-E', '-'],
        input='#include <libusb-1.0/libusb.h>\n', text=True, capture_output=True)
    if probe.returncode:
        pytest.skip('libusb development headers required')
    executable = tmp_path / 'iqs-usb-test'
    subprocess.run([compiler, '-O2', '-std=c++17', '-Wall', '-Wextra', '-pthread',
        '-I' + str(ROOT / 'backend/sdk'), str(ROOT / 'tests/cpp/iqs_usb_pipeline_test.cpp'),
        '-ldl', '-o', str(executable)], check=True, capture_output=True, text=True)
    result = subprocess.run([str(executable)], check=True, capture_output=True, text=True, timeout=20)
    assert 'PASS: bounded USB ordering' in result.stdout
