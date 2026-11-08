"""The FFT diagnostic must reject fake SDK failures without opening hardware."""
from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
HEADER = r"""
#pragma once
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
enum { FlatTop, TraceDetector_PosPeak, Complex16bit };
struct DSP_FFT_TypeDef {
    uint32_t FFTSize = 0, SamplePts = 0;
    int WindowType = 0, TraceDetector = 0;
    double Intercept = 0, DetectionRatio = 0;
    bool Calibration = false;
};
struct IQStream_TypeDef {
    void* AlternIQStream;
    double IQS_ScaleToV;
    struct { int DataFormat; double CenterFreq_Hz; uint32_t TriggerLength; } IQS_Profile;
    struct {
        uint32_t PacketSamples, StreamSamples, PacketDataSize;
        double IQSampleRate, Bandwidth;
    } IQS_StreamInfo;
};
inline bool fault(const char* name) {
    const char* value = std::getenv("WEBSA_FFT_PROBE_TEST_FAULT");
    return value && std::strcmp(value, name) == 0;
}
inline int DSP_Open(void** out) { *out = new int(1); return 0; }
inline int DSP_Close(void** handle) {
    delete static_cast<int*>(*handle); *handle = nullptr;
    std::fprintf(stderr, "fake DSP closed\n"); return 0;
}
inline int DSP_FFT_DeInit(DSP_FFT_TypeDef* in) { *in = {}; return 0; }
inline int DSP_FFT_Configuration(void**, DSP_FFT_TypeDef* in,
        DSP_FFT_TypeDef* out, uint32_t* points, double* rbw) {
    *out = *in; *points = in->FFTSize / 2 + 1;
    *rbw = fault("nonfinite_rbw") ? NAN : 1.0;
    return 0;
}
inline int DSP_FFT_IQSToSpectrum(void**, IQStream_TypeDef* stream,
        double* frequency, float* power) {
    if (fault("processing_error")) return -7;
    const uint32_t points = stream->IQS_StreamInfo.PacketSamples / 2 + 1;
    for (uint32_t i = 0; i < points; ++i) {
        frequency[i] = 100e6 + i * 1000.0;
        power[i] = i == 3 ? -20.0f : -100.0f;
    }
    if (fault("nonfinite_output")) frequency[1] = NAN;
    return 0;
}
"""


@pytest.fixture(scope="module")
def fft_probe(tmp_path_factory):
    compiler = shutil.which("c++")
    if compiler is None:
        pytest.skip("C++ compiler required for SDK diagnostic fault injection")
    directory = tmp_path_factory.mktemp("sdk-fft-probe")
    (directory / "htra_api.h").write_text(HEADER)
    binary = directory / "probe"
    subprocess.run([
        compiler, "-std=c++17", "-O1", "-I", str(directory),
        str(ROOT / "tools/bench/sdk_fft_probe.cpp"), "-o", str(binary),
    ], check=True, capture_output=True, text=True, timeout=30)
    return binary


@pytest.mark.parametrize("fault", ["processing_error", "nonfinite_rbw", "nonfinite_output"])
def test_sdk_fft_probe_rejects_failed_measurement_and_closes(fft_probe, fault):
    result = subprocess.run([str(fft_probe)], env={**os.environ,
        "WEBSA_FFT_PROBE_TEST_FAULT": fault}, capture_output=True, text=True, timeout=5)
    assert result.returncode != 0, f"{fault} reported success:\n{result.stdout}"
    assert result.stderr.count("fake DSP closed") == 1
    assert "mean_ms=" not in result.stdout, "failed configuration must not report a measurement"


def test_sdk_fft_probe_reports_every_successful_configuration(fft_probe):
    environment = {key: value for key, value in os.environ.items()
                   if key != "WEBSA_FFT_PROBE_TEST_FAULT"}
    result = subprocess.run([str(fft_probe)], env=environment,
                            capture_output=True, text=True, timeout=5)
    assert result.returncode == 0, result.stderr
    assert result.stdout.count("mean_ms=") == 5
    assert result.stderr.count("fake DSP closed") == 1
