# Harogic WebSA

**Browser spectrum analysis and SDR reception for HAROGIC SAN-45 / SAN-60 / SAN-90.**

[简体中文](README.zh-CN.md) · [Documentation](docs/en/INDEX.md) · [Build and contribute](docs/en/DEVELOPMENT.md)

![WebSA spectrum analyzer](screenshots/main_dark.png)

WebSA combines a native Rust acquisition/DSP service, a C++ adapter for the HAROGIC SDK,
and a TypeScript browser interface. Python is used for offline tools and tests.

## What it does

- Swept and real-time spectrum, four traces, density/waterfall, markers, channel power,
  occupied bandwidth, ACPR, harmonic and phase-noise measurements.
- Up to sixteen independently tuned receivers in one capture, local audio mixing,
  floating channel/decoder windows and sixteen shared recording task slots.
- AM/DSB/USB/LSB/CW/NFM/WFM/PM, FM stereo/RDS, noise reduction, FT8, DRM/DRM+, SSTV
  and radiofax; optional external decoder providers extend the mode catalog.
- IQ, audio and decoded-result recording, saved-file playback/export, receiver presets,
  English/Chinese UI and light/dark themes.
- Exclusive SoapySDRServer handoff for external SDR clients.

Mode availability depends on installed providers and capture geometry. Sustained sixteen-channel
RF capacity remains unqualified; synthetic validation is not a hardware performance guarantee.
See [Known limitations](docs/en/OPERATIONS.md).

## Try it

The checkout includes a **Linux x86_64/glibc** runtime. It currently requires glibc 2.44,
GLIBCXX 3.4.30, CXXABI 1.3.15 and `libfdk-aac.so.2`; rebuild if your system cannot run it.

```bash
./run.sh --fake                 # synthetic input, no analyzer required
./status.sh
./stop.sh
```

Open **http://127.0.0.1:8080**. For real hardware, install the separately supplied HAROGIC
SDK under `/opt/htraapi` and run `./run.sh`. The proprietary SDK is not bundled.
[Getting started](docs/en/GETTING_STARTED.md) covers prerequisites, instances and updates.

## Build and test

```bash
make native                    # Rust service, one build job
make frontend                  # browser assets
make sdk-shim                  # hardware adapter; vendor SDK required
./test.sh                      # native, Python and frontend tests
```

Run heavyweight commands separately to limit memory use. Restart the intended service after a
native rebuild; an existing process keeps its old executable. Full prerequisites, fixture/browser
checks and packaging are in [Development](docs/en/DEVELOPMENT.md).

## Find your way

| Need | Start here |
|---|---|
| Tune, listen, decode and record | [User guide](docs/en/USER_GUIDE.md) |
| Troubleshoot a device or service | [Operations](docs/en/OPERATIONS.md) |
| Understand the implementation | [Architecture](docs/en/ARCHITECTURE.md) |
| Integrate another client | [API](docs/en/reference/API.md), [receivers](docs/en/reference/RECEIVERS.md), [recordings](docs/en/reference/RECORDINGS.md) |
| Install optional providers | [Tools](tools/README.md) |

`backend/` owns the service and hardware boundary; `dsp/` owns shared signal-processing
kernels; `frontend/` owns browser interaction; `contracts/` holds shared limits;
`tests/` and `tools/` hold regression fixtures and offline tooling.

## License and attribution

Project host code is [MIT](LICENSE). Vendor code, codecs, model weights and independently
installed providers have their own terms; see [Dependencies and notices](docs/en/DEVELOPMENT.md#dependencies-and-notices).
The HAROGIC SDK is proprietary and must be obtained separately.

Author: [好奇牛马 (Bilibili)](https://space.bilibili.com/28447213).
