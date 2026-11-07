# Tools

Helpers for the native Rust service, committed fixtures, browser gates, and RF benches.
Start with [Architecture](../docs/en/ARCHITECTURE.md), [API](../docs/en/reference/API.md), and
[Development](../docs/en/DEVELOPMENT.md) for runtime and protocol contracts.
Python dependencies belong to these tools; the service runs through `./run.sh`,
`./stop.sh`, and `./status.sh`.

## Choose a task

| Task | Section |
|---|---|
| Install Soapy handoff | [Soapy driver](#soapy-server-driver-setup) |
| Verify fixtures or replay IQ | [Checks and references](#checks-fixtures-and-references) |
| Set up ADS-B, GPS or RTTY | [ADS-B](#ads-b--mode-s-aircraft), [GPS](#gps-l1-ca-from-sdr-iq), [RTTY](#independent-rtty-companion) |
| Exercise the browser | [Browser/service benches](#browser-and-service-benches) |
| Run signal-source experiments | [RF transmit/capture](#rf-transmit-and-capture) |
| Qualify the SDR pipeline | [Pipeline acceptance](#sdr-pipeline-acceptance) |
| Inspect device behavior | [Native hardware diagnostics](#native-hardware-diagnostics) |

These are focused procedures; general setup and the bounded build/test workflow belong in
[Development](../docs/en/DEVELOPMENT.md). Run heavy probes separately, retain small reports,
and clean only temporary data created by that run. Historical results in this guide do not
qualify the current executable.

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
`python3 -m pip install -r requirements-dev.txt -c requirements-lock.txt` in a virtual environment. Browser probes also need `python3 -m playwright install chromium`.
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

For a custom prefix, export `WEBSA_SOAPY_MODULE_PATH=<prefix>/lib/SoapySDR/modules0.8` before starting `websad`; direct Soapy tools use `SOAPY_SDR_PLUGIN_PATH` for that path. The manager prepends its module path to the inherited plugin path and probes `SoapySDRUtil --info` for the registered lowercase `harogic` factory. `WEBSA_SOAPY_SERVER_BIN` and `WEBSA_SOAPY_UTIL_BIN` are trusted process-environment overrides of the two executables, not browser settings. Handoff requires the nonterminal `san90_session_release_v1` ABI; a loaded shim without it refuses handoff. Rebuild custom SDK shims with `make sdk-shim`.

Open WebSA, select **SRV**, configure the stopped server's IP literal/port, then Start. The default bind is `127.0.0.1:55132`; use `0.0.0.0` for IPv4 LAN clients, then connect to the server's reachable LAN address. A wildcard bind (`0.0.0.0` or `::`) is not a client destination. Use `driver=remote,remote=<host>:<port>,remote:driver=harogic` in a SoapyRemote client. SoapySDRServer exposes installed device factories, so retain the `harogic` filter. Listening confirms only the process endpoint; the analyzer opens on client use. Web capture and audio are stopped throughout Server mode. Spectrum stops and reaps the child before restoring the last graph view. See the paired [API](../docs/en/reference/API.md) / [中文](../docs/zh-CN/reference/API.md) for state, logs, dependency failures, browser lifetime and release refusal.

SoapyRemote provides automatic network discovery through mDNS/Avahi and SSDP; WebSA does not need a second publisher. `avahi-browse -rt _soapy._tcp` can show the advertised host and port. An advertisement can include addresses on multiple interfaces, including loopback; discovery does not make a server bound to `127.0.0.1` reachable from another machine. In OnAir 3.2, stop reception, open the source selector, and enable **Search the network for radios** (off by default). The client must have the SoapyRemote module installed; discovered radios then appear through its Soapy source.

[SDRoxide's SoapySDR backend](https://github.com/dividebysandwich/sdroxide#soapysdr-connectivity) accepts Soapy device arguments. Use a build with its `soapy` feature and install the SoapyRemote module **on the client**. Connect with `sdroxide --device 'driver=remote,remote=127.0.0.1:55132,remote:driver=harogic'`, substituting the server's reachable host for LAN use. SDRoxide's `--connect` and Settings → Remote use its own WebSocket protocol on port 4950; that entry point cannot connect to SoapySDRServer. An AppImage advertising native drivers alone does not establish that its build includes SoapySDR. This is an upstream-supported path, without an SDRoxide/SAN-90 reception claim.

Stream geometry is validated before host allocation: the native SDK rate must be finite and within
1–1,000,000,000 S/s, the returned rate must be within the advertised decimation range, and packets
must contain 1–1,048,576 complex samples with matching byte counts. The 50 ms ring plus conversion
scratch has a 64 MiB ceiling; these are adapter resource ceilings, not device capabilities.
Malformed per-fetch metadata returns a stream error before copying, and stop/reactivation can
reuse the retained device handle. The transport MTU remains 16,384 samples.

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
regenerating data. `tests/test_sdr_capture.py` uses an SDK-free ctypes stub: the Python reference
reader accepts only consistent CS16 packets up to 4 MiB before copying. The separate production
shim regression requires installed SDK headers/library but no hardware:

```sh
cargo test --release -j 1 --manifest-path backend/Cargo.toml --bin websad production_shim_preserves_valid_overflow_and_rejects_malformed_outputs -- --ignored --test-threads=1
```

`tools/reference/` and `tools/analysis/` provide independent numerical
comparisons; they do not run the HTTP service. DRM generation, capture, codec oracle, and AAC
reserialization procedures are documented in [DRM development reference](../docs/en/reference/DRM.md)
([中文](../docs/zh-CN/reference/DRM.md)).

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
External modes use the same receiver: for example, `--mode pocsag1200 --input-rate 22050
--bandwidth 12500` consumes normalized FM IQ, while `--mode ism --input-rate 250000
--bandwidth 200000` supplies CF32 to rtl_433. Configure `WEBSA_MULTIMON_PATH`,
`WEBSA_RTL433_PATH`, or `WEBSA_ONAIR_SERVER` (otherwise executables are found on PATH).
The service's `/api/sdr/plugins` reports the installed capabilities and bandwidth contracts.
rtl_433 receives exactly 250 kS/s at every tuned frequency, including bands above 800 MHz;
the adapter applies its rate option after upstream frequency-dependent defaults.
Use `--center-hz` for the tuned RF frequency and `--realtime` to pace recordings by their
sample clock for live or wall-clock decoders. OnAir AIS requires `--center-hz 162000000`.
OnAir FT4/WSPR require UTC-aligned live IQ. CLI recording replay and looping fake-device
fixtures reject these modes: pacing a recording does not restore its original UTC slot phase.
stdin providers close input, drain results and reap at EOF; OnAir has no consumed/flush ACK,
so its bounded drain reports that upstream completion is unknown. Replay never pads IQ.

### ADS-B / Mode S aircraft

The `adsb` mode uses the stock OnAir 3.3 Web aircraft decoder and appears under
aviation. Tune to 1090 MHz; the Web factory accepts centers from 1087 through
1093 MHz. The host requires at least 2.5 MS/s receiver IQ, filters a fixed
1.8 MHz occupied bandwidth (±900 kHz), and resamples to the decoder's exact
2 MS/s input. A wide capture selects the direct IQ route automatically. The
private rtl_tcp source accepts 2 MS/s without a hardware device; OnAir still
must advertise its `adsb` Web capability for the mode to become available.

The rate and frequency contract comes from upstream
[`web_decoders.cpp`](https://github.com/adswill/OnAir/blob/v3.3.0/server/web/web_decoders.cpp)
(`kSpecs`, `AdsbDec`). The Web slice uses `ChannelFilter`/`ExactResampler`
([`web_dsp.cpp`](https://github.com/adswill/OnAir/blob/v3.3.0/server/web/web_dsp.cpp),
[`exact_resampler.cpp`](https://github.com/adswill/OnAir/blob/v3.3.0/core/src/dsp/exact_resampler.cpp));
the host's ±900 kHz pass edge stays below the 2 MS/s Nyquist edge. Upstream
[`adsb_rx.cpp`](https://github.com/adswill/OnAir/blob/v3.3.0/core/src/adsb/adsb_rx.cpp)
documents a sensitivity penalty for its 2 MS/s, 1.75 MHz frontend compared with
the desktop default of 4 MS/s and 3.5 MHz. The Web decoder's rate therefore does
not imply equal sensitivity to that wider desktop path.

Aircraft snapshots retain ICAO address, callsign, altitude in feet, latitude and
longitude, speed in knots, track, vertical rate, squawk and other available
fields. Each snapshot replaces the preceding aircraft table. Source and OnAir
virtual center frequencies accompany results; absent position fields are not
invented. Reception and offline recordings are supported; the Pluto transmit
tool does not provide ADS-B transmission.

For an offline synthetic fixture, an independently installed OnAir CLI can write
CF32 samples without opening a radio. Its ADS-B synthetic default is **4 MS/s**;
`--rate` configures file input and does not override synthetic generation:
OnAir 3.3's CLI does not handle bare `--help` as a help-only invocation and may
open its default radio. Select `--synthetic` explicitly for the recipe below.

```sh
onair-cli --synthetic --standard adsb --freq 1090 --snr 99 --secs 10 \
  --record /tmp/adsb-4Msps.cf32
backend/target/release/sdr_decode --input /tmp/adsb-4Msps.cf32 --mode adsb \
  --input-rate 4000000 --center-hz 1090000000 --bandwidth 1800000 \
  --realtime --report-json /tmp/adsb-decode.json
```

These commands describe an offline decoder check, not RF sensitivity validation.
As with the other OnAir modes, EOF has a bounded result drain without a consumed
sample acknowledgement.

### GPS L1 C/A from SDR IQ

The `gps` mode runs the independent GPL GNSS-SDR 0.0.20 receiver behind a native
MIT `websa-gnss` process bridge. GPS requires at least 2,600,000 complex samples/s
before rate adaptation; the provider FIFO consumes exactly 4,000,000 complex
samples/s. Normal reception is at 1575.420 MHz with a fixed 2.046 MHz channel
bandwidth. A centered 3.90625 MSps capture can therefore be adapted to 4 MSps;
capture admission also includes the filter transitions and tuning offset.
It decodes the selected SDR IQ; the device SDK's built-in GNSS status is a
different receiver.

```sh
bash tools/setup-gnss-decoder.sh
cargo build -j 1 --release --manifest-path backend/Cargo.toml --bin websa-gnss --bin sdr_decode
install -m755 backend/target/release/websa-gnss "${XDG_DATA_HOME:-$HOME/.local/share}/websa/decoders/bin/websa-gnss"
source "${XDG_DATA_HOME:-$HOME/.local/share}/websa/decoders/gnss-env.sh"
backend/target/release/sdr_decode --input gps.cf32 --mode gps \
  --input-rate 4000000 --bandwidth 2046000 --center-hz 1575420000 \
  --realtime --report-json /tmp/gps.json
```

The installer preserves the pinned source archive, GPL notices and serial build
directory. `--archive FILE` reuses its SHA256-checked source archive;
`--dependency-prefix DIR` locates locally extracted development packages.
GNU Radio, Boost headers and the other development prerequisites listed by
`--help` must be present. `WEBSA_GNSS_SDR_PATH` selects GNSS-SDR and
`WEBSA_GNSS_BRIDGE_PATH` selects the native bridge. Restart WebSA after installation
because capability discovery is cached. No hardware receiver is opened by the
GNSS-SDR process; WebSA remains the sole IQ capture owner.

Structured `satellites`, `navigation` and `pvt` tables report actual PRNs, C/N0
in dB-Hz, acquisition/tracking/navigation-word validity, decoded navigation bits,
and position/velocity/time. No-fix is explicit, and a received position expires
when monitor updates become stale. The upstream FIFO source does not acknowledge
EOF: the bridge drains for a finite interval and reports
`upstreamCompletionKnown:false`. Live reconfiguration starts a new GNSS receiver.
An ordinary VHF antenna does not establish GPS reception; use a suitable passive
receive setup or a recording. No GPS L1-band transmission is part of this workflow.

A provenance-bearing real recording is the [CTTC 2013-04-04 GPS capture](https://sourceforge.net/projects/gnss-sdr/files/data/2013_04_04_GNSS_SIGNAL_at_CTTC_SPAIN.tar.gz/download):
100 seconds at 4 MSps, zero IF, signed 16-bit interleaved I/Q, 1,600,000,000 raw
bytes (approximately 1.15 GB compressed). Convert each signed sample to f32le
by dividing by 32768 before using the command above. The upstream
[first-fix tutorial](https://gnss-sdr.org/my-first-fix/) reports latitude 41.2749°,
longitude 1.98754°, height 100.795 m and GPS navigation subframes from multiple
PRNs. A production Receiver replay of all 400 million complex samples completed
in 103.65 seconds, forwarding all 3.2 GB of converted CF32 with zero malformed
monitor packets. It tracked PRNs 1, 11, 17, 20 and 32, decoded 67 navigation
subframes and emitted 232 valid PVT snapshots. The final valid solution was
41.2748489°, 1.98766735°, height 78.13 m, with five satellites and GPS week 1734.
This establishes real-recording decoding through the integration, not passive
sky reception or translated RF performance. For this historical recording set
`WEBSA_GNSS_PRE_2009_FILE=true` to select GNSS-SDR's historical week-rollover
interpretation; leave it unset for live reception.

For an explicitly enabled translated bench signal, set `WEBSA_GPS_BENCH_HZ=435000000`
before starting WebSA and tune the GPS receiver to that exact frequency. The setting
must contain only decimal digits and lie from 400000000 through 450000000 Hz;
other non-L1 centers remain rejected. Normal 1575.420 MHz reception stays available.
The host passes the actual center via `websa-gnss --center-hz HZ`; this changes
receive validation and provenance only, not the GPS baseband signal model.
Results mark `bench:true`, retain actual `center_hz`/`sourceFrequencyHz`, and
identify `nominalCarrierHz:1575420000` separately. A translated generated-signal
result is a bench decoder result, not passive sky reception. This receiver option
does not start or configure a transmitter.

### Independent RTTY companion

`rtty` and `rtty-reverse` use a separately installed GPL-3.0-or-later companion
linked to OnAir's mature `HfdigRtty` decoder. They do not depend on the stock
OnAir Web RTTY factory, which its upstream policy filters out. The MIT host
passes protocol IQ through its raw USB demodulator at 48 kS/s → 8 kHz mono;
listener AGC, gain, squelch and noise reduction do not affect decoding.

```sh
bash tools/setup-rtty-decoder.sh
# Restart websad after installation.
```

The installer needs Git and a C++17 compiler, fetches pinned OnAir revision
`6b97328b1939f569b161c5066fbea8053a76c130`, and compiles only the RTTY source
and standalone companion. No GUI, sound routing, full OnAir build or SDR driver
is required. `--source /path/to/OnAir` uses a clean checkout of that revision;
`--prefix DIR` selects the independent installation directory. Sources and
GPL notices remain separate from the MIT host; retain corresponding sources
when redistributing the companion. Discovery checks `WEBSA_RTTY_PATH`, then `PATH`,
then `bin/websa-rtty` under `WEBSA_DECODERS_PREFIX` or the default
`${XDG_DATA_HOME:-$HOME/.local/share}/websa/decoders`. For a custom `--prefix`
that is not exported as `WEBSA_DECODERS_PREFIX`, source its `rtty-env.sh` before starting.
Restart the service after installation because capability discovery is cached.

Select **RTTY** or **RTTY (reverse)** from the mode catalog. Both use ITA2,
five data bits, one start bit and 1.5 stop bits. Default baud/shift are
45.45/170 Hz; normal uses the lower tone as mark and reverse uses the upper.
Tune to the USB dial below the pair; the decoder searches 300–3300 Hz for both
tones. For example, lower/upper tones at +2125/+2295 Hz fit the fixed 3400 Hz
passband. Decoded text handles letters/figures shifts; tables report actual
pair frequencies, quality, character count and framing errors. Searching or
pair lock alone is not evidence that valid text has been decoded.

Alternate settings are service startup environment variables:
`WEBSA_RTTY_BAUD=45.45|50|75|100`, `WEBSA_RTTY_SHIFT=170|200|425|450|850`,
`WEBSA_RTTY_LOW_TONE=0` (auto, or lower-tone Hz with both tones within
300–3300 Hz), and `WEBSA_RTTY_USOS=0|1` (unshift on space, default off).
Polarity is selected per receiver by mode; the other settings apply to newly
created RTTY decoders. The standalone companion accepts matching `--baud`,
`--shift`, `--low-tone`, `--usos` and `--reverse` arguments, reads 8 kHz f32le
mono from stdin, and writes JSON lines to stdout.

Generate an independent known-text IQ recording and optional oracle WAV:

```sh
python3 tools/fixtures/rtty_iq.py /tmp/rtty.f32 --wav /tmp/rtty.wav
backend/target/release/sdr_decode --input /tmp/rtty.f32 --mode rtty \
  --input-rate 48000 --bandwidth 3400 --report-json /tmp/rtty-report.json
python3 tools/fixtures/rtty_iq.py /tmp/rtty-reverse.f32 --reverse
backend/target/release/sdr_decode --input /tmp/rtty-reverse.f32 --mode rtty-reverse \
  --input-rate 48000 --bandwidth 3400 --report-json /tmp/rtty-reverse-report.json
```

The generator uses independent ITA2 tables and continuous-phase FSK, with a
two-tone acquisition preamble and repeated letters/figures payload. Its JSON
sidecar records exact rate, tones, polarity and expected text (CR is omitted
by OnAir's display). It requires NumPy and never accesses hardware. A separate
decoder such as minimodem can receive the WAV with explicit mark/space
2125/2295 Hz at 45.45 baud; swap those frequencies for reverse. These commands
describe acceptance checks, not a claim that a recording or RF run passed.

Normal and reverse 45.45-baud/170-Hz RTTY decoded the known text, digits and
punctuation through the production Receiver in both offline fixtures and the
435 MHz Pluto-to-SAN-90 bench. The RF runs used the existing `usb --wav` path
and explicit gain −30 dB; each stopped on matching text within 31 seconds,
without receiver errors. These results apply to those tested settings.



## Browser and service benches

```sh
make e2e-fake
python3 tools/e2e/ui_smoke.py --url http://127.0.0.1:8080
python3 tools/e2e/state_regression.py --url http://127.0.0.1:8080
make viewport-check
make viewport-baseline VIEWPORT_JSON=/tmp/websa-viewport.json
make bench
```

`python3 tools/e2e/native_multichannel_legacy_resume.py --artifacts <new-directory>`
creates its own synthetic native service and checks Stop all followed by main
SDR frequency, same-frequency, center and mode edits. It verifies one UID audio
consumer, advancing PCM playback, local mute preservation and unchanged stopped
auxiliary receivers without editing volume. Build the frontend and native
release binary first. It accepts no existing origin and does not test RF or
physical speaker output.

`make e2e-fake` builds and launches an isolated native fixture service. Direct
probes, viewport targets, and `make bench` use a running service. Viewport output
records current geometry; it is not an input for screenshot comparison.
`bench/bench.py` consumes `bench/bench_baseline.json`; that historical baseline
does not certify current native performance. `make bench-record` replaces it
with measurements from the current host. Keep the configuration and report
with any comparison. CPU samples are attributed only to the native process that
owns the requested loopback HTTP listener. A remote endpoint or an ambiguous
listener has no local CPU measurement; another running service cannot supply it.

Specialized `e2e/` probes cover trace reset, mode switches, WFM/RDS, FT8 window
dragging, SSTV reception, and README screenshots. Check each tool's `--help` for
its service and signal requirements. Browser acceptance must observe actual
audio, decoded content, and images. Include independent tune/rate/mode changes,
fractional IQ rates, and transitions between analog and codec modes. Capture
cadence and combined trace/density/waterfall rendering have separate limits;
a native 20-fps gate does not certify browser rendering or seamless playback.

The state-regression, viewport and native-IQ browser probes tear down browsers and contexts even after an exception. The system-Chromium screenshot path uses a unique temporary profile and log, terminates and reaps only its own process group, removes its temporary files, and restores monitor focus.

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
The shared radio layer enables the TX LO when a previous application left its
`powerdown` attribute set, then restores that state after destroying the TX
buffer on exit. A successful DMA push alone does not confirm RF transmission.

The dispatcher also provides `dtmf`, `aprs`, `ais`, `ft4`, `wspr` and `gps`.
Digital modes and GPS share `pluto_radio.Radio` and its persistent streaming
API: `open_stream(kernel_buffers=4)`, `prime_stream(...)`, `push_chunk(...)`
and `drain_stream(...)`. The shared layer owns radio setup, queue priming,
draining and cleanup; each generator owns its waveform, sample clock and
continuity deadlines. FT4/WSPR align to UTC slots; Pluto has no timestamped TX,
so transport startup uncertainty remains part of the bench evidence.

Install the independent GPS simulator with `bash tools/setup-gps-simulator.sh`.
The GPS command generates a stationary synthetic 2022 scenario at
25.2° N, 55.36° E, 10 m, with signal time advanced by sample count. Use
`python3 tools/pluto_tx.py gps --dry-run` for a bounded offline probe or
`python3 tools/pluto_tx.py gps --duration 180 --gain -30` for the selected bench
setting. The GPS hardware-gain default remains −70 dB; −30 dB is an explicit
bench choice, not a changed default. Set `WEBSA_GNSS_PRE_2009_FILE=false` for
this simulator's receiver, and use the translated receive opt-in described above.

The GPS CLI accepts only a requested `--lo 435000000`; it rejects other carriers,
including GPS L1. Before generating the waveform or starting TX, it negotiates
an actual sample-rate readback within 20 Hz of the request and divisible by ten.
The default TX request is 2.6 MSps; waveform generation and signal-time accounting
use the exact accepted rate. This TX rate is independent of the receiver's
2.6 MSps minimum and exact 4 MSps provider input. LO readback may differ from
435 MHz by at most 100 Hz; actual LO, translation and rate are reported. Failure
to meet either rate or LO constraint stops transmission. GPS uses a bounded
producer queue and uninterrupted chunks; a missed host continuity deadline stops
RF. These deadlines do not measure hardware DMA underruns.

The cold-start translated GPS RF run recorded in
`rf-gps-cold-fixed-minus30.json` passed through the Pluto-to-SAN-90 path at
435 MHz, explicit gain −30 dB and 3.90625 MSps capture adapted to the exact
4 MSps provider input. After 63.41 seconds, four satellites supported a valid
PVT solution at 25.20001599° N, 55.35996828° E, height 7.319 m, versus the
synthetic target 25.2° N, 55.36° E, 10 m: horizontal error was 3.653 m.
The receiver reported no error. The transmitter was interrupted with exit
status 130 and restored its crystal trim. This establishes the tested translated
bench path; passive sky reception remains unverified.

With the native service running and the transmitter coupled to the SAN-90,
capture the channelized IQ for native analysis and repeatable replay:

```sh
python3 tools/bench/hil_audio_check.py --mode wfm --frequency 411e6 \
  --seconds 8 --tinysa-port '' --ifbw 180000 --out /tmp/hil_wfm.json
WEBSA_RDS_IQ=/tmp/hil_wfm.json cargo test -j 1 --release --manifest-path dsp/core/Cargo.toml \
  --test live_rds_capture -- --ignored --nocapture
```

`hil_reference_check.py`, `ft8_capture_check.py`, and `ft8_reference_compare.py`
retain native/oracle comparisons. Synthetic fixtures cannot establish RF filter,
noise, or acquisition quality. For DRM, include acquisition after silence,
signal-loss recovery, actual codec PCM, and browser playback; fixture loop seams
and finite bursts can cause underruns.

## SDR pipeline acceptance

`bench/multichannel_rf.py` owns a SAN90/Pluto bench for sustained sixteen-channel
AM/NFM/USB/LSB acceptance. It verifies every IQ/audio subscription, sample clocks,
stream identity and known RF tones, then stops its service, restores the transmitter
and removes temporary tone PCM after saving hashes. Use a fresh external artifact
directory and the matching SDK shim. The [development guide](../docs/en/DEVELOPMENT.md#fixtures-and-evidence)
([中文](../docs/zh-CN/DEVELOPMENT.md#样本与验证证据)) gives the 600-second command and
the limits of its 300 kHz stimulus within a 50 MHz capture. Keep heavy builds and
browser tests out of the measurement window.

IQBF/IQAX v2 uses a 56-byte little-endian header. `channel_id`, `epoch`,
`input_index`, and `group_delay_samples` begin at bytes 32, 36, 40, and 48;
complex f32 IQ begins at byte 56. Stream identity is `(epoch, channel_id)`.
Frequency is metadata. Registry, native channel clocks, and wire metadata must
agree, including fractional rates and source-time positions. See the
[architecture](../docs/en/ARCHITECTURE.md) and [API](../docs/en/reference/API.md) contracts.

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
