# Tools

Helpers for the native Rust service, committed fixtures, browser gates, and RF benches.
Start with [Architecture](../docs/en/ARCHITECTURE.md), [API](../docs/en/API.md), and
[Development](../docs/en/DEVELOPMENT.md) for runtime and protocol contracts.
Python dependencies belong to these tools; the service runs through `./run.sh`,
`./stop.sh`, and `./status.sh`.

## Tool groups and dependencies

| Group | Maintained purpose |
| --- | --- |
| `checks/` | Version, documentation, DOM/registration, architecture, and artifact gates. |
| `fixtures/` | Generate/check committed wire, DSP, FT8, SSTV, and DRM fixtures and tables. |
| `reference/`, `analysis/` | Offline SDK/DDC/demodulation references, DSP parity, and DRM oracle checks. |
| `e2e/` | Playwright UI, receiver, state-transition, viewport, and screenshot tools. |
| `bench/` | Native pipeline, numerical, service performance, capture/replay, and hardware diagnostics. |
| `pluto/`, `pluto_tx.py` | RF signal generators behind one transmit entry point. |
| `sdr_probe/` | SDK constraints, device helpers, and native IQ/audio/decode probes. |
| `vendor/` | ggmorse sources, licensing, and artifact rebuild script. |
| `build-soapy.sh` | Build the official SoapyHarogic module with typed IQ and device-lifecycle fixes; `--check-patch` checks pinned patch applicability and `--check-stream` checks the patched stream contract with an HTRA stub. |

Run commands from the repository root. Install Python tools with
`python3 -m pip install -r requirements-dev.txt`; use `requirements-lock.txt` for
the pinned environment. Browser probes also need `python3 -m playwright install chromium`.
Native DSP builds require Rust, `pkg-config`, FDK AAC 2.0.3 or newer, and a C++17
compiler. Frontend builds require Node/npm. Browser reception uses native DSP;
WASM builds remain explicit offline diagnostics.

SAN-90 tools require the vendor SDK and exclusive device access. `make sdk-shim`
builds the retained-session shim against SDK 0.55.100; override `SDK_ROOT` and
`SDK_SHIM` for the local installation. Native hardware helpers take an explicit
shim or `WEBSA_RUST_IQS_SHIM`. Pluto transmission requires a reachable ADALM-PLUTO,
libiio, and pyadi-iio; tinySA helpers require pyserial and the selected serial port.

## Soapy Server driver setup

Server mode needs system SoapySDR 0.8 development files, `SoapySDRUtil`, and SoapyRemote (`SoapySDRServer` on the host and the remote module on clients). Build tools are `curl`, CMake, pkg-config, realpath, patch and a C++ compiler. Install these dependencies and the proprietary HTRA SDK yourself; the helper never runs root or package-install commands.

```sh
make native sdk-shim frontend
make soapy-driver
# Custom SDK and installation prefix:
make soapy-driver SDK_ROOT=/opt/htraapi SOAPY_PREFIX="$HOME/.local/share/harogic-websa/soapy"
# SDK-free pinned patch and helper shell syntax check:
make soapy-check-patch
# Stream contract check with real SDK headers; no device or SDK library used:
make soapy-check-stream SDK_ROOT=/opt/htraapi
```

`make soapy-driver` calls `tools/build-soapy.sh`; a direct invocation builds the same patched module (`--build` is an explicit alias for the default). It fetches only `CMakeLists.txt`, `HarogicDevice.cpp`, `HarogicDevice.hpp`, `LICENSE`, and `cmake/FindLibHTRA.cmake` from official [HAROGIC-Technologies/soapy-htra](https://github.com/HAROGIC-Technologies/soapy-htra/tree/762dbfa1512c705e1d8b86e820b06e8a10e08a92) revision `762dbfa1512c705e1d8b86e820b06e8a10e08a92`. The driver is GNU LGPL v2.1 or later; its source, copyright notices, licence and revision attribution remain under `${SOAPY_SRC_CACHE:-$HOME/.cache/harogic-websa/soapy-htra}/<revision>`. SDK headers/libraries stay external. Builds use one compile job and install to `${SOAPY_PREFIX:-$HOME/.local/share/harogic-websa/soapy}/lib/SoapySDR/modules0.8`; the default `SDK_ROOT` is `/opt/htraapi`.

`make soapy-check-patch` requires `curl`, `patch` and standard shell utilities, but no SDK or SoapySDR development files. It runs `bash -n tools/build-soapy.sh` and the helper's `--check-patch` mode to check applicability against the pinned upstream sources. `make soapy-check-stream SDK_ROOT=/opt/htraapi` additionally requires `pkg-config`, `realpath`, a C++ compiler, SoapySDR development files and real vendor SDK headers. It compiles the patched driver with stub HTRA functions and links only SoapySDR; it needs neither a vendor library nor a device.

Every build applies this project's `tools/soapy-htra-stream-format.patch` to a private source copy, leaving the cached upstream revision unchanged. Upstream advertises integer formats while writing CF32; the patched driver actually writes CF32, CS16 or CS8 as requested. Before stream setup, the native-format query defaults to CS16 up to 60 MSps (`fullScale` 32768) and CS8 above 60 MSps (`fullScale` 128). After setup it reports the selected output format and its `fullScale` (CF32: 1, CS16: 32768, CS8: 128). `native_format` selects SDK CS8/CS16/CS32/AUTO at stream setup, and that SDK width stays fixed until the stream is closed. AUTO matches integer output width; for CF32 it selects CS16 up to 60 MSps and CS8 above that rate at setup. Explicit integer output requires a matching SDK width. The ring stores raw SDK samples and bulk-copies matching integer output; CF32 converts each component by its SDK scale (`2^7`, `2^15` or `2^31`). CS32 is an SDK input option, not an offered Soapy output format. CF32, CS16 and CS8 use 8, 4 and 2 bytes per complex sample respectively; client output format and SoapyRemote wire format can differ. `--check-stream` compiles the actual patched driver with an in-process HTRA stub and links SoapySDR only. It checks the format contract and buffer writes without opening hardware; it does not qualify RF streaming or any SDR application.

The stream MTU is 16384 samples before and after activation, independent of SDK packet size. Active reads return available samples up to the requested count; consumers must accept partial reads. Inactive reads honor their timeout, and fatal acquisition errors return `SOAPY_SDR_STREAM_ERROR`.

The host IQ ring holds about 50 ms at the configured sample rate, with a minimum of two SDK packets and 16384 samples. Allocation occurs during activation and reuses existing storage. A full ring reports `SOAPY_SDR_OVERFLOW`.

For a custom prefix, export `WEBSA_SOAPY_MODULE_PATH=<prefix>/lib/SoapySDR/modules0.8` before starting `websad`; direct Soapy tools use `SOAPY_SDR_PLUGIN_PATH` for that path. The manager prepends its module path to the inherited plugin path and probes `SoapySDRUtil --info` for the registered lowercase `harogic` factory. `WEBSA_SOAPY_SERVER_BIN` and `WEBSA_SOAPY_UTIL_BIN` are trusted process-environment overrides of the two executables, not browser settings. Rebuild the SDK shim too: handoff requires the nonterminal `san90_session_release_v1` ABI. The committed runtime adapter has not been updated and cannot provide the handoff.

Open WebSA, select **SRV**, configure the stopped server's IP literal/port, then Start. The default bind is `127.0.0.1:55132`; use `0.0.0.0` for IPv4 LAN clients, then connect to the server's reachable LAN address. A wildcard bind (`0.0.0.0` or `::`) is not a client destination. Use `driver=remote,remote=<host>:<port>,remote:driver=harogic` in a SoapyRemote client. SoapySDRServer exposes installed device factories, so retain the `harogic` filter. Listening confirms only the process endpoint; the analyzer opens on client use. Web capture and audio are stopped throughout Server mode. Spectrum stops and reaps the child before restoring the last graph view. See the paired [API](../docs/en/API.md) / [中文](../docs/zh-CN/API.md) for state, logs, dependency failures, browser lifetime and release refusal.

SoapyRemote provides automatic network discovery through mDNS/Avahi and SSDP; WebSA does not need a second publisher. `avahi-browse -rt _soapy._tcp` can show the advertised host and port. An advertisement can include addresses on multiple interfaces, including loopback; discovery does not make a server bound to `127.0.0.1` reachable from another machine. In OnAir 3.2, stop reception, open the source selector, and enable **Search the network for radios** (off by default). The client must have the SoapyRemote module installed; discovered radios then appear through its Soapy source.

USB discovery uses the SDK's `Device_List` to obtain device indices and serials without opening acquisition handles. Explicit Ethernet addresses bypass broadcast discovery; the constructor validates the selected device and serial. One streaming-capable handle serves capability queries and subsequent streams. Deactivation and `closeStream` stop acquisition but retain that handle; device destruction releases it. Changing live settings stops and joins the worker before SDK configuration, and unchanged settings do not restart acquisition.

OnAir can reconstruct its entire Soapy device when changing mode or IQ bandwidth. Device reconstruction still requires an SDK open; stream recreation within an existing device reuses its handle. WebSA's server process remains running during client-side changes.

Use the default UDP MTU with SoapyRemote 0.5.2. Native CS16/CS8 transport reduces network traffic relative to CF32. `driver=harogic,prot=tcp` selects TCP in OnAir's remote-driver setting. The driver supplies no hardware timestamps.

## Checks, fixtures, and references

```sh
make ci
python3 tools/checks/check_docs_parity.py
python3 tools/checks/check_doc_paths.py
python3 tools/fixtures/gen_frame_fixtures.py --check
python3 tools/fixtures/gen_dsp_fixtures.py --check
python3 tools/fixtures/gen_ft8_fixtures.py --check
python3 tools/fixtures/gen_sstv_fixtures.py --check
```

`make ci` runs the in-process Rust/Python/frontend and fixture/static gates, including
`soapy-check-patch`, without hardware or the vendor SDK. Hosted CI also runs the SDK-free
patch check. `soapy-check-stream` is a separate gate for hosts with the real vendor headers
and SoapySDR development files. See [fixture ownership](../tests/fixtures/README.md) before
regenerating data. `tools/reference/` and `tools/analysis/` provide independent numerical
comparisons; they do not run the HTTP service. DRM generation, capture, codec oracle, and AAC
reserialization procedures are documented in [DRM bench](../docs/en/DRM_BENCH.md)
([中文](../docs/zh-CN/DRM_BENCH.md)) and
[DRM AAC reserialization](../docs/en/DRM_AAC_RESERIALIZATION.md)
([中文](../docs/zh-CN/DRM_AAC_RESERIALIZATION.md)).

Offline IQ replay uses the production receiver:

```sh
cargo build -j 1 --release --manifest-path backend/Cargo.toml --bin sdr_decode
backend/target/release/sdr_decode --input IQ.f32 --mode ft8 \
  --input-rate 48000 --bandwidth 3000 --report-json /tmp/decode.json
```

Input is interleaved f32le IQ. Add `--output-pcm /tmp/audio.f32` for interleaved
f32le PCM; the report supplies its actual rate and channel count. A single PCM
file rejects format changes, while report-only runs retain each format span.
The decoder adds no input padding and flushes the remaining FT8 window at EOF.

## Browser and service benches

```sh
make e2e-fake
python3 tools/e2e/ui_smoke.py --url http://127.0.0.1:8080
python3 tools/e2e/state_regression.py --url http://127.0.0.1:8080
make viewport-check
make viewport-baseline VIEWPORT_JSON=/tmp/websa-viewport.json
make bench
```

`make e2e-fake` builds and launches an isolated native fixture service. Direct
probes, viewport targets, and `make bench` use a running service. Viewport output
records current geometry; it is not an input for screenshot comparison.
`bench/bench.py` consumes `bench/bench_baseline.json`; that historical baseline
does not certify current native performance. `make bench-record` replaces it
with measurements from the current host. Keep the configuration and report
with any comparison.

Specialized `e2e/` probes cover trace reset, mode switches, WFM/RDS, FT8 window
dragging, SSTV reception, and README screenshots. Check each tool's `--help` for
its service and signal requirements. Browser acceptance must observe actual
audio, decoded content, and images. Include independent tune/rate/mode changes,
fractional IQ rates, and transitions between analog and codec modes. Capture
cadence and combined trace/density/waterfall rendering have separate limits;
a native 20-fps gate does not certify browser rendering or seamless playback.

## RF transmit and capture

`pluto_tx.py` lists and dispatches the per-mode generators:

```sh
python3 tools/pluto_tx.py --list
python3 tools/pluto_tx.py --dry-run-all
python3 tools/pluto_tx.py nfm --lo 411e6 --gain -10
python3 tools/pluto_tx.py usb --wav tools/bench/audio/speech.wav --seconds 60
python3 tools/pluto_tx.py wfm --no-audio --rds-rt 'RIG RDS OK 0123456789' \
  --rds-ps 'RIG RDS' --lo 411e6 --gain -10
python3 tools/pluto_tx.py sstv --mode 'Robot 36' --callsign 'CQ WE0BSA'
python3 tools/pluto_tx.py radiofax --ioc 576 --lpm 120 --image chart.png --dry-run
python3 tools/pluto_tx.py drm --iq drm_iq.wav --seconds 600
```

Use `--wav` for analog programme audio, `--voice` for the USB/LSB voice chain,
and `wfm --stereo FILE` for two-channel programme audio with a pilot. For a mono
station omit stereo, pilot, and RDS options. RDS uses the WFM 57-kHz subcarrier;
`--rds-buffer` enables cyclic playback. SSTV accepts `--image FILE` and
`--list-modes`. Radiofax (`wefax` is an alias) accepts PNG/PPM files through
`--image`, keeping their aspect at the IOC width unless `--lines` is given.
Use `--ioc 288|576`, `--lpm 60|90|120|180|240`, `--modulation fm|am`,
`--polarity normal|inverted`, and `--list-modes`. The default is an IOC576/120 LPM
FM pattern with 128 lines and standard APT/phasing. Framing times are configurable;
`--seconds 0` sends a full page, `--repeat 0` repeats until interrupted, and
`--pause` pushes silence between pages. A radiofax dry run synthesizes at most
two seconds by default (`--dry-run-seconds` changes that limit).
FM uses 1500–2300 Hz above the TX LO; tune the receiver in USB at the LO with
Radiofax IFBW 3000. AM uses a 1900 Hz carrier with true zero for black; choose
AM modulation on the receiver. 180 LPM needs manual receive settings. Whole
pages use fixed 0.25 s complex64 chunks at the radio's accepted rate; Gray8 input
is limited to 40 MiB. CPU replay of generated IOC288/576 pages through the native
receiver has verified phasing and APT-stop completion with correct 16-line ramps.
Real Pluto-to-SAN-90 RF at 435 MHz also received complete IOC576/120 LPM 1810×64
and IOC288/240 LPM 905×256 test pages, with phasing clock recovery and no missing rows.
Generator clock trim is device-specific; inspect the selected radio's options
before relying on RF frequency accuracy.

With the native service running and the transmitter coupled to the SAN-90,
capture the channelized IQ for native analysis and repeatable replay:

```sh
python3 tools/bench/hil_audio_check.py --mode wfm --frequency 411e6 \
  --seconds 8 --tinysa-port '' --ifbw 180000 --out /tmp/hil_wfm.json
WEBSA_RDS_IQ=/tmp/hil_wfm.json cargo test -j 1 --release --manifest-path dsp/core/Cargo.toml \
  --test live_rds_capture -- --nocapture
```

`hil_reference_check.py`, `ft8_capture_check.py`, and `ft8_reference_compare.py`
retain native/oracle comparisons. Synthetic fixtures cannot establish RF filter,
noise, or acquisition quality. For DRM, include acquisition after silence,
signal-loss recovery, actual codec PCM, and browser playback; fixture loop seams
and finite bursts can cause underruns.

## SDR pipeline acceptance

IQBF/IQAX v2 uses a 56-byte little-endian header. `channel_id`, `epoch`,
`input_index`, and `group_delay_samples` begin at bytes 32, 36, 40, and 48;
complex f32 IQ begins at byte 56. Stream identity is `(epoch, channel_id)`.
Frequency is metadata. Registry, native channel clocks, and wire metadata must
agree, including fractional rates and source-time positions. See the
[architecture](../docs/en/ARCHITECTURE.md) and [API](../docs/en/API.md) contracts.

Against a running hardware service, use explicit URLs and retain JSON reports:

```sh
python3 tools/bench/sdr_pipeline_controls.py --url http://127.0.0.1:8080 \
  --center 411e6 --output /tmp/sdr-controls.json
python3 tools/bench/sdr_pipeline_check.py --url http://127.0.0.1:8080 \
  --seconds 60 --aux 410980000 --aux 411040000 --output /tmp/sdr-live.json
```

The control probe exercises identity, aux removal/recreation, retune/replan,
rejected requests, and old-tail ordering before a new `seq=0`. Command replies
must have matching registry/clock keys; `SET_MODE std` empties both arrays.
The live probe checks continuity, error counters, and 20-fps spectrum delivery.
Its positive wire/native clock skew allowance is 1.5 seconds for cached status
and framing/scheduling. This sampling allowance does not prove drained sample
conservation or alter recovery thresholds.

Stop the service before the internal hardware fixture. Build first, then keep
the device window free of builds, tests, browsers, and other session work:

```sh
./stop.sh
backend/target/release/websad --sdr-pipeline-bench \
  --product-channels true --seconds 60 --warmup 5 \
  --shim "$WEBSA_RUST_IQS_SHIM" --center 411000000 --output /tmp/sdr-product-60.json
```

Set `WEBSA_RUST_IQS_SHIM` to the matching built shim. This fixture runs three
native receivers: WFM 180 kHz, AM 6 kHz, and CW 500 Hz, with independent rates,
full-band display D=1, and 16 taps. After the 60-second zero-queue-full screen,
run the same command with `--seconds 600` and a separate output path. Require
zero queue-full events, recovery, failed fetches, skips, and stamp gaps. After
shutdown, compare every delivered sample count with its stopped native clock;
sequence checks alone cannot prove conservation. Keep failed reports.

For a channelizer-only workload, replace `--product-channels true` with
`--channels 4 --taps 16 --display-decimate 1 --fine-decimate 3`. Requested load
does not bypass admission. Fine/framer admission and receiver runtime cost have
different scopes; neither certifies unmeasured RF, network, or recording work.
The backend has one primary receiver and two auxiliary slots; the current product UI exposes only the primary receiver and disables auxiliaries on entering SDR.

## Native hardware diagnostics

`make hw-test` runs tinySA/SAN-90 smoke, command sweep, and UI state regression.
The retained `bench/rust_*` tools cover SWP calibration/drift, mode changes,
PNM/harmonics, unplug recovery, and RF measurements. They use the native service
and restore it through the root scripts. `sdr_raw_iqs_check.py` and
`sdr_pfb_hil.py` inspect raw SDK timing and bounded PFB scheduling;
`sdr_pfb_bench.py` and `sdr_channelizer_ab.py` provide numerical/throughput
comparisons. Their scope is narrower than integrated product acceptance.

[SDK findings](sdr_probe/FINDINGS.md) records current device constraints. Keep
one retained SDK session and stable handle slot on its owning thread, configure
before arming capture, and fetch IQ/FFT/warning/timing atomically through
`san90_next_v3`. Valid IQ returned with warning `-12` retains its original
stamps; saturation is separate from failed fetches. Hardware diagnostics must
observe clock continuity and PCM/decode progress during saturation, Ref changes,
Preset, and mode transitions. These commands describe acceptance procedures;
this guide does not assert a fresh hardware validation result.
