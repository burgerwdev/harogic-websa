"""Exercise the actual vendored CW input buffers at supported PCM rates under ASan."""
from __future__ import annotations

import os
import resource
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
DRIVER = r"""
#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <iostream>
#include <vector>

extern "C" {
void* native_ggmorse_new(int, float, float) noexcept;
void native_ggmorse_free(void*) noexcept;
int native_ggmorse_push(void*, const int16_t*, size_t) noexcept;
size_t native_ggmorse_take(void*, char*, size_t) noexcept;
float native_ggmorse_pitch(void*) noexcept;
float native_ggmorse_wpm(void*) noexcept;
}

int main(int argc, char** argv) {
    if (argc != 2) return 2;
    const int rate = std::atoi(argv[1]);
    void* decoder = native_ggmorse_new(rate, 700.0f, 900.0f);
    if (rate < 8000 || rate > 192000) {
        if (decoder) { native_ggmorse_free(decoder); return 7; }
        return 0;
    }
    if (!decoder) return 3;
    // Exercise both full 100-ms pushes and partial frames. All rates are in
    // the safe Rust CwDecoder's advertised 8..192 kHz range; no device is used.
    const size_t total = rate / 2;
    std::vector<int16_t> input(total);
    for (size_t i = 0; i < total; ++i) {
        input[i] = int16_t(std::lround(12000.0 * std::sin(2 * std::acos(-1.0) * 700.0 * i / rate)));
    }
    size_t accepted = 0;
    const size_t blocks[] = {1, 137, size_t(rate / 10), 53, size_t(rate / 10)};
    for (size_t block = 0; accepted < total; ++block) {
        const size_t n = std::min(blocks[block % 5], total - accepted);
        if (native_ggmorse_push(decoder, input.data() + accepted, n) != 0) {
            native_ggmorse_free(decoder); return 4;
        }
        accepted += n;
        char text[4096];
        if (native_ggmorse_take(decoder, text, sizeof(text)) > sizeof(text)) {
            native_ggmorse_free(decoder); return 5;
        }
    }
    const bool finite = std::isfinite(native_ggmorse_pitch(decoder))
        && std::isfinite(native_ggmorse_wpm(decoder));
    native_ggmorse_free(decoder);
    if (!finite) return 6;
    std::cout << "accepted=" << accepted << '\n';
}
"""


@pytest.fixture(scope="module")
def cw_bridge(tmp_path_factory):
    compiler = shutil.which("c++")
    if compiler is None:
        pytest.skip("C++ compiler required for native CW buffer regression")
    directory = tmp_path_factory.mktemp("cw-bridge-bounds")
    source, binary = directory / "driver.cpp", directory / "driver"
    source.write_text(DRIVER)
    vendor = ROOT / "tools/vendor/ggmorse"
    subprocess.run([compiler, "-std=c++17", "-O1", "-g", "-fsanitize=address",
                    "-fno-omit-frame-pointer", "-I", str(vendor / "include"),
                    "-I", str(vendor / "src"), str(source),
                    str(ROOT / "backend/native/ggmorse_bridge.cpp"),
                    str(vendor / "src/ggmorse.cpp"), str(vendor / "src/resampler.cpp"),
                    "-o", str(binary)], check=True, capture_output=True, text=True, timeout=60)
    return binary


def bounded_child():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    resource.setrlimit(resource.RLIMIT_CPU, (10, 10))


@pytest.mark.parametrize("rate", [48000, 132000, 144000, 176400, 192000])
def test_cw_supported_pcm_rate_fits_vendor_input_buffers(cw_bridge, rate):
    # ASan reserves virtual shadow space, so constrain physical RSS through its
    # watchdog rather than an address-space limit that prevents ASan startup.
    result = subprocess.run([str(cw_bridge), str(rate)], capture_output=True, text=True,
        timeout=15, preexec_fn=bounded_child,
        env={**os.environ, "ASAN_OPTIONS": "detect_leaks=1:hard_rss_limit_mb=256:allocator_may_return_null=1"})
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == f"accepted={rate // 2}"


@pytest.mark.parametrize("rate", [0, 7999, 192001])
def test_cw_ffi_rejects_rates_outside_the_rust_contract(cw_bridge, rate):
    result = subprocess.run([str(cw_bridge), str(rate)], capture_output=True, text=True,
        timeout=15, preexec_fn=bounded_child,
        env={**os.environ, "ASAN_OPTIONS": "detect_leaks=1:hard_rss_limit_mb=256:allocator_may_return_null=1"})
    assert result.returncode == 0, result.stderr
    assert result.stdout == ""
