# API Documentation

Native backend (`backend/`) external interfaces: **HTTP REST** + **WebSocket** (JSON commands/status + binary trace frames). Root `./run.sh` defaults to hardware on port 8080; Python clients are optional tools.

- Service: `http://127.0.0.1:8080` (WebSocket at `/ws`)
- Frontend: modern UI at `/`, static assets `/static/dist/{file}`
- Loopback is the default. Set `WEBSA_HOST=0.0.0.0` for LAN access; product endpoints
  do not require authentication. WebSockets require same-origin access or an origin
  listed in `WEBSA_ALLOWED_ORIGINS`.

---

## 1. HTTP Endpoints

### `GET /api/state`

Returns the full device state (JSON, same shape as WS `STATUS`).

```bash
curl http://localhost:8080/api/state
```

Example response:

```json
{
  "cmd": "STATUS",
  "connected": true,
  "device": "SAN-90",
  "device_detail": { "uid": "5230500C00380025", "model": 67, "hw": 8, "mfw": 14184, "ffw": 14184, "bus_speed": 12, "bus_ver": 1, "api_ver": "0.55.89", "warnings": "" },
  "center": 1000000000.0, "span": 600000000.0, "ref": 0.0,
  "rbw_mode": "auto", "rbw": 100000.0, "vbw_mode": "bypass", "vbw": 1000000.0,
  "points": 1000, "window": 1, "spur": "standard", "mode": "std", "pnm_supported": true,
  "caps": { "model": 67, "name": "SAN-90", "fmin": 9000, "fmax": 9000000000,
            "ref_min": -50, "ref_max": 30, "rta_span_max": 50781250, "rta_points": 3328 },
  "preset_defaults": { "center": 1000000000, "span": 100000000, "rbw": 100000, "points": 1000, "ref": 0, "atten": -1 },
  "req": { "center": 1000000000, "span": 600000000, "points": 1000, "rbw_mode": "auto", "rbw": 100000, "vbw_mode": "bypass", "vbw": 1000000, "ref": 0, "spur": "standard" },
  "actual": { "center": 1000000000, "span": 600000000, "points": 985, "rbw": 100000, "vbw": 1000000 },
  "amp": { "atten": -1, "preamp": 0, "ifgain": 2, "gain_strategy": 0, "atten_actual": 30, "preamp_actual": 1, "ifgain_actual": 2 },
  "ref_clock": "internal", "has_docxo": false, "refclk_ppm": 0.0, "calibrating": false,
  "refclk_out": false, "last_cal_freq": 0.0, "gnss": { "lock": false, "sats": 0, "docxo": 0, "docxo_mode": -1, "antenna": -1, "latitude": 0.0, "longitude": 0.0, "altitude": 0, "time": "0000-00-00 00:00:00" },
  "last_error": ""
}
```

Field reference:

| Field | Type | Description |
|---|---|---|
| `connected` | bool | device connected |
| `device` / `device_detail` | str / obj | device name + details (uid/model/hw/mfw/ffw/bus/api/warnings) |
| `center` / `span` / `ref` | number | effective values for the active hardware mode |
| `ref_mode` | str | always `manual`: Auto Scale is a one-shot action, not a tracking mode |
| `rbw_mode` / `rbw` | str / number | RBW mode (manual/auto), resolution bandwidth |
| `vbw_mode` / `vbw` | str / number | VBW mode (bypass/equal/tenth/manual), video bandwidth |
| `points` | int | requested SWP trace points; the actual returned native count is in each FREQ/POWR frame |
| `window` | int | FFT window: 0=FlatTop 1=Blackman-Nuttall 2=LowSideLobe 3=Rectangle 4=Kaiser |
| `spur` | str | spur rejection (bypass/standard/enhanced) |
| `detector` | str | trace detector (auto/sample/pos_peak/neg_peak/rms/auto_peak), SWP only |
| `frame_detector` | str | Frame Avg request; native Preset restores `device_default`, leaving the inter-frame detector to the SDK profile default |
| `mode` | str | measurement mode (std/harmonic/pnm/rta/sdr) |
| `caps` | obj | model capabilities: `model`/`name`/`fmin`/`fmax` plus the numeric limits the client must not hard-code - `ref_min`/`ref_max` (dBm, the **auto-placement** row, not a limit on what a user may ask for - see FAQ_NOTES), `rta_span_max` (Hz), `rta_points` |
| `preset_defaults` | obj | device default config (used by Preset) |
| `req` / `actual` | obj | active request/actual values; `req.swp`, `req.rta` and `req.sdr` retain mode-private settings |
| `swp_actual` / `rta_actual` | obj | latest SDK effective settings for each spectrum mode |
| `sweep_ms` / `sweep_ms_meas` | number | SDK-estimated minimum SWP time (ms, in FREQ/POWR headers; 0 if SDK has no estimate) / publisher's measured acquisition/publish step duration EMA (ms, STATUS only). SWT prefers the estimate; when it is 0, SWT displays the EMA marked `measured` rather than leaving the field blank or mixing sources. |
| `sdr` | obj | Requested tuning/demod/audio settings, `actual` capture/display/DDC geometry, `level_dbfs`, squelch, vendor ADM metrics and native stream health. `sdr.level_dbfs` is channel 1's meter; each `sdr.channels[].level_dbfs` measures its own normalized complex IQ before AGC/audio gain: `10*log10(mean(I²+Q²))`, with wire full scale 1. It is null for no finite measurement (including zero power), inactive/disabled channels, changed stream identity/tuning, outside SDR mode, or input older than 1500 ms. Each channel's `channel_dbm` reports its fresh SDK tuned-bin peak; `squelch_open` reports the processed analog audio gate, and is null before processing, for stale/inactive streams or for DRM codec audio that bypasses the gate. Top-level `sdr.channel_dbm` and `sdr.squelch_open` mirror channel 1. Requested display bandwidth may be narrower than device capture bandwidth. |
| `sdr.filter_limits` | obj | Current mode's dynamic `ifbw` geometry bounds: `mode`, `min_hz`, `max_hz`. `min_hz` is the real DSP floor — 50 for AM/DSB/CW/NFM/WFM/PM and 101 for USB/LSB; protocol-locked or unknown modes report `min_hz:0, max_hz:null`. `max_hz` is an upper bound in whole Hz for the current capture center/bandwidth, input rate, PFB taps, listen frequency and CW pitch; it is null outside SDR, with channel 1 disabled, or when no feasible range exists. It is not a mode recommendation and excludes other enabled channels' cost admission, so an in-range value can still be rejected. |
| `sdr.receiver_error` | string or null | Shared receiver worker diagnostic; null when no receiver failure is active. A failure stops all audio/decode receivers while capture, raw IQ and display streams continue. It is separate from global `last_error` and `ddc_error`. |
| `rta_actual.frame_points` | int | RTA device FFT frame width; RTA2 delivers returned bins independently of SWP points |
| `config_version` / `response_to` / `request_id` | int / str / scalar | successful reconfiguration sequence; command name and optional scalar request identity echoed on command STATUS/ERROR; periodic publications omit response identity |
| `auto_ref` | obj | bounded Auto session state: `adjusting`; `pending`, the hardware or display target awaiting fresh confirmation; `hardware_adjusting`, which distinguishes SDR capture recovery/audio transition and remains false for display-only fitting; `seq`, `last_peak`, `last_noise_floor`, `result`, and successful final `target` (otherwise null). See Auto Scale below. |
| `rta_health` | obj | current consecutive RTA errors and in-place recovery attempts |
| `amp` | obj | shared requested `atten`/`preamp`/`ifgain`/`gain_strategy`, plus active-mode SDK-configured `atten_actual`/`preamp_actual`/`ifgain_actual`; unavailable actuals are null, never copied from requests. Preamp 0 means configured Auto and 1 Off, not physical On/off telemetry. |
| `ref_clock` | str | reference clock source: internal/external/premium/external_forced |
| `has_docxo` / `has_premium_ref_clock` | bool | Installed DOCXO hardware / available premium internal source. Derived from hardware options, independent of GNSS lock or work mode |
| `refclk_ppm` / `calibrating` / `last_cal_freq` | - | GNSS calibration state |
| `refclk_out` | bool | reference clock output enable |
| `gnss` | obj | GNSS state: `lock`(0/1) `sats` `docxo`(0/1) `docxo_mode`(0=disciplined,1=hold) `antenna`(0=external,1=internal) `latitude`/`longitude`(deg) `altitude`(m) `time`(UTC, "0000-00-00 00:00:00" when invalid) |
| `last_error` | str | recent global error message; receiver worker failures use `sdr.receiver_error` |

Complete parameter ownership, defaults, and mode transitions are documented in
[`MODE_STATE_FLOW.md`](MODE_STATE_FLOW.md).

### `POST /api/config`

Send a command via REST (equivalent to WS commands, see table below). Response is the full STATUS.

```bash
curl -X POST http://localhost:8080/api/config \
  -H 'Content-Type: application/json' \
  -d '{"cmd": "SET_FREQ", "center": 1090000000, "span": 600000000}'
```

### `GET /`

Returns the frontend page (modern UI `dist/index.html`).

### `GET /static/dist/{file}`

Frontend static assets (JS/CSS/fonts).

---

## 2. WebSocket Protocol

### Connect

```
ws://localhost:8080/ws
```

After connecting, the server starts pushing trace frames (FREQ/POWR) and status.

### 2.1 Client → Server (commands)

JSON object: `{"cmd": "<COMMAND>", ...}`

| Command | Params | Description |
|---|---|---|
| `STATUS` | - | request one full status (STATUS reply) |
| `CONNECT` | - | connect device (if not connected) |
| `SET_PRESET` | - | restore device default config (Preset) |
| `CAL_REFCLK` | `count?` | GNSS 1PPS reference clock calibration (background; `calibrating=true` meanwhile) |
| `SET_FREQ` | `center`,`span` or `start`,`stop` | atomically set the SWP frequency window |
| `SET_REF` | `mode` (manual/auto), `ref?`, `range_db?` | active-mode reference; manual requires `ref` in -160..40 dBm. `mode=auto` starts one Auto Scale session |
| `AUTO_SCALE` | `range_db?` (5..200), `current_ref?`, `display_only?` | bounded reference placement/recovery; see below. `current_ref` excludes external Offset; the browser subtracts Offset from its displayed Ref |
| `SET_RBW` | `mode?` (manual/auto/one_thousandth_span/one_percent_span), `rbw?` | SWP resolution bandwidth; RTA uses its supported manual/auto choices |
| `SET_VBW` | `mode?` (manual/equal/tenth/onethousandth/bypass), `vbw?` | set video bandwidth |
| `SET_SWEEP` | `mode?` (0..8), `time?` (schema: 0..1000) | 0=minSWT, 1=x2, 2=x4, 3=x10, 4=x20, 5=x50, 6=xN, 7=Manual, 8=minSMPxN. `time` is seconds for Manual and a multiplier for xN; mode-specific handler validation also applies. RTA updates its own profile |
| `SET_POINTS` | `points` (integer 51..16000) | requested SWP points; RTA points are SDK-owned |
| `SET_SPUR` | `mode` (bypass/standard/enhanced) | spur rejection mode |
| `SET_WINDOW` | `window` (0~4) | FFT window |
| `SET_DETECTOR` | `mode` (auto/sample/pos_peak/neg_peak/rms/auto_peak) | trace detector, SWP only |
| `SET_AMP` | `atten?` (-1~33), `preamp?` (0/1), `ifgain?` (0~3), `gain_strategy?` (0/1), `request_id?` | update supplied gain fields; STATUS/ERROR echoes request_id. Requested fields and SDK actual readbacks remain separate |
| `SET_REFCK` | `mode` (internal/external/premium/external_forced) | requested reference source; reconfigures the active profile. Unavailable premium hardware is rejected before mutation. `actual.refclk_src` reports the SDK-resolved source, including automatic external fallback |
| `SET_REFCKOUT` | `on` (bool) | reference clock output; reconfigures the active profile |
| `SET_MODE` | `mode` (std/harmonic/pnm/rta/sdr) | switch measurement mode (session) |
| `SET_RTA` | `center?`, `span?` | atomically set RTA center and 2^n analysis span |
| `SET_SDR` | `center?`, `decimate?` (power of two, 1~`caps.decimate_max`; rounded to the nearest power of two), `pfb_taps_per_phase?` (native backend: 4/8/16, default 16) | set the SDR capture window; the native backend rebuilds the channelizer for the selected PFB taps per phase. 16 taps is the default rejection profile; 8/4 are measured lower adjacent/alias-rejection profiles |
| `SET_SDR_TUNE` | `listen` (Hz) | tune the primary receiver. On the native backend, tuning within the capture updates the channel plan; an enabled primary tuned beyond the capture moves the shared capture to the requested frequency. All three channel configurations are preserved. Enabled auxiliary channels must fit the replacement capture; auxiliary tuning never moves the capture. Device limits, occupied bandwidth, coarse passband and admission failures are reported explicitly |
| `SET_SDR_DEMOD` | `mode?`, `deemph_us?` (-1=auto 0=off ..1000), `ifbw?` (positive finite Hz), `squelch?` (-200..0 nominal SDK dBm tuned-bin peak, before audio AGC; <=-160 selects Off), `volume?` (0~2), `pitch?` (CW 100..1600 Hz; other modes (0,20000] Hz), `agc?` (bool), `am_sync?` (bool, default false) | primary receiver settings. The native backend processes all 13 modes: am/dsb/usb/lsb/cw/nfm/wfm/pm/ft8/sstv/radiofax/drm/drmplus. It emits AUD2 PCM and SDE2 events. The browser controls playback and displays results. `ifbw` requests a receiver filter change and reconfigures the channelizer when admitted |
| `SET_SDR_CHANNEL` | `channel?` (1–3, default 1), `enabled?`, `freq?`/`listen?`, `mode?`/`demod?`, `ifbw?`/`if_bw?`, `squelch?`, `volume?`, `agc?`, `pitch?`, `deemph_us?`/`deemph?`, `nr?`, `nr_strength?`, `nr_algorithm?`, `nr_atten_db?`, `stereo?`, `am_sync?`, `audio_rate?`, `sstv_mode?`, `radiofax_acquisition?`, `radiofax_ioc?`, `radiofax_lpm?`, `radiofax_modulation?`, `radiofax_polarity?`, `radiofax_clock_recovery?`, `radiofax_phase_pixels?`, `radiofax_clock_ppm?`, `radiofax_new_page?`, `radiofax_reset_correction?` | partially update one receiver; omitted fields retain their values. Conflicting aliases are rejected. Radiofax acquisition is auto/manual, IOC auto/288/576, LPM auto/60/90/120/180/240, modulation fm/am, polarity normal/inverted and clock recovery auto/off. Phase is ±4096 px and clock correction ±5000 ppm. `radiofax_new_page` closes the active page as partial; `radiofax_reset_correction` sets both corrections to zero. Protocol changes rebuild the decoder; phase/ppm changes do not rebuild RF. |
| `SET_HARM` | `f0`, `count`, `span` | harmonic params (fundamental Hz, orders, span per harmonic) |
| `SET_PNM` | `center`, `threshold`, `traceavg`, `start`, `stop` | phase noise params |
| `SET_TRIGGER` | `source` (bus/freerun/level/external/timer), `edge` (rising/falling/double), `level` (dBm, `caps` bounds), `safetime`/`delay`/`pretime` (0~10 s), `acqtime` (0.0005~60 s), `retrigger` (0-65535), `retriggerperiod` (s), `out` (none/per_hop/per_sweep/per_profile), `outpolarity` (positive/negative) | RTA acquisition trigger (written into the RTA profile): reconfigured immediately while in RTA, otherwise applied on the next entry. The swept-mode level trigger runs in the frontend and does not use this command |

The squelch slider covers -160..0 in 0.1 dB steps; its minimum selects Off. Raising
the threshold requires a stronger signal to open audio. The gate opens at or above
the threshold and closes below threshold minus 3 dB after a 300 ms hold, with a
smoothed attack/release. Missing or expired RF measurements fail open. The RF
reference is the SDK spectrum's peak within one bin of the tuned frequency, not
integrated channel power or the IQ dBFS meter; absolute RF calibration accuracy
has not been independently established. Protocol decoding precedes this listener gate.

For native FM audio, `agc=false` uses fixed PCM gain. NFM deviation of 5 kHz
and WFM deviation of 75 kHz map to full scale before listener volume.
WFM waveform events use a separate 20 Hz source clock.

AM defaults to envelope detection; `am_sync:true` selects carrier-recovered synchronous detection. The preference is retained outside AM. An `am_sync`-only update crossfades polarity-aligned detectors without rebuilding capture, filters or resampling, changing stream identity, or resetting input/audio clocks. STATUS reports the confirmed value in `sdr.channels[].am_sync`, with primary mirrors `sdr.am_sync` and `req.sdr.am_sync`.

`sdr.channels[].am_sync_generation` is a server-owned read-only u32 (default 0), mirrored for the primary channel as `sdr.am_sync_generation` and `req.sdr.am_sync_generation`. It increments only when `am_sync` changes; duplicate values leave it unchanged and requests cannot set it. Preset resets it with the new stream identity.

Native WFM defaults to stereo interleaved L/R AUD2; explicit `stereo:false` selects mono.
DRM/DRM+ codec audio does not use analog AGC, NR, squelch or deemphasis; the UI
disables these controls and retains volume/playback. Disabled receiver slots accept
partial updates without a valid dormant frequency; enabling validates tuning.
SSTV `sstv_mode:null` selects automatic detection; a supported integer pins a mode.
Saved selection is replayed only after confirmed SDR entry. Display decimation,
capture `sdr.actual.iq_rate` and per-channel `actual_iq_rate` are distinct quantities.

Commands validate the complete candidate channel before changing state. CW pitch outside 100..1600 Hz returns `bad_parameter` without failing receivers. NR changes preserve decoder history and continuous `audio_index`; source position advances over discarded reducer tails. DRM/DRM+ store NR preferences with effective NR Off. DFN initialization failure or unsupported operation selects Wiener. A runtime DFN error fails the receiver without fallback.

`ifbw` is a mode-dependent request. For AM/DSB/NFM/WFM/PM it is the total bandwidth centered on the dial. For USB/LSB it is the outer high-frequency cutoff; the inner carrier guard is `clamp(0.1 × ifbw, 100, 300)` Hz, mirrored for LSB. CW is centered at `max(100, pitch)` Hz and extends by `±max(50, ifbw) / 2`. The browser editor and backend both enforce a 50 Hz minimum for other editable modes and 101 Hz for USB/LSB, preventing guard collapse. The browser also checks the current `sdr.filter_limits`; backend admission additionally applies the selected mode's envelope, capture and output sample rates, coarse passband and channel-capacity constraints. An invalid or inadmissible request is rejected rather than silently clamped. Presets are recommendations, not upper limits: custom values above common presets, such as AM 100 kHz, SSB 50 kHz and CW 10 kHz, are allowed when admitted. The input supports Hz/kHz/MHz; unit changes preserve the underlying Hz value, and the virtual keypad uses the same validation and commit path as typed input.

The browser fixes FT8 at `ifbw=3000` with a 100–3000 Hz decoder range, SSTV at `ifbw=3000` with a 300–2700 Hz decoder range, selects and locks DRM at `ifbw=12000`, and fixes DRM+ at `ifbw=100000`. These are UI mode semantics, not wire-level restrictions: a direct API request may still use other values, subject to backend geometry and admission. DRM displays `±confirmed_ifbw/2` independently of the FAC-signalled occupancy, so it does not automatically receive every 4.5–20 kHz occupancy. DRM+'s 100 kHz UI value is the channel width, not a claim that its outermost OFDM carriers are exactly ±50 kHz.

Channel admission uses a symmetric occupied-bandwidth envelope around tuning. USB/LSB cover the sideband IF limit; CW includes pitch and IF; FT8 retains at least positive 3.2 kHz and SSTV positive 3 kHz. Integer IQ rates follow that envelope; exact protocol rates are adapted internally. WFM IQ also supports pilot/stereo/RDS, independently of PCM playback rate. A primary recapture validates all enabled channels first; rejection leaves the previous plan active.

> Config replies include `response_to=<command>` on STATUS and ERROR. A scalar
> `request_id` (string/number/bool/null) is echoed unchanged; object/array IDs are omitted.
> Periodic status and publisher errors carry no request identity. Rejected SWP/RTA
> reconfiguration preserves the prior request and applied profile when restoration succeeds.
> Invalid REST commands return HTTP 400; WS commands return `cmd:"ERROR"` with `msg`.
>
> Periodic STATUS `stream` metrics contain `clients`, `dropped_frames`, `dropped_control`,
> `dropped_audio` and `dropped_iq` for observing latest-wins drops and streaming-FIFO overruns
> caused by slow clients.

**Command examples:**

```js
// set frequency and switch to harmonic measurement
ws.send(JSON.stringify({ cmd: 'SET_FREQ', center: 1e9, span: 100e6 }));
ws.send(JSON.stringify({ cmd: 'SET_MODE', mode: 'harmonic' }));
ws.send(JSON.stringify({ cmd: 'SET_HARM', f0: 1e9, count: 5, span: 1e6 }));
```

### 2.2 Server → Client

#### JSON messages

| `cmd` | Trigger | Description |
|---|---|---|
| `STATUS` | on connect / after commands / **periodic (every 1 s)** | full status (fields in §1); periodic push keeps GNSS/time/lock states fresh without page refresh |
| `HARM` | during harmonic measurement | result: `{cmd:'HARM', list:[{n,f,amp,dBc,idx,inSpan}...]}` |
| `PNM` | during phase noise | result: `{cmd:'PNM', offset[], pn[], carrier_freq, carrier_power, progress, done}` |
| `ERROR` | device/command error | `{cmd:'ERROR', code, params?, msg, response_to?, request_id?}`; stable `code` supports localization, unknown codes fall back to `msg` |
| `RECEIVER_ERROR` | shared receiver worker failure | `{cmd:'RECEIVER_ERROR', error:string, failed:true, scope:'all_receivers'}`; retains the receiver diagnostic without a global spectrum error |

**PNM message fields:**

```json
{
  "cmd": "PNM",
  "carrier_freq": 999999900.0,   // carrier frequency (Hz)
  "carrier_power": -20.1,        // carrier power (dBm)
  "offset": [100, 1000, 10000, 100000, 1000000, 10000000],   // offset points (Hz)
  "pn": [-82.8, -95.5, -104.3, -117.1, -126.4, -132.0],      // phase noise (dBc/Hz)
  "progress": 25,                // incremental acquisition progress 0~100 (refreshes after first round)
  "done": false                  // first-round complete flag
}
```

> Unfilled segments of `pn` are -500 (invalid); the plot is complete when `offset` covers the full range and `pn` has no invalid points.

#### Binary trace frames

**FREQ/POWR — 16-byte header + data:**

```
offset  size  type    content
0       4     bytes   magic: 'FREQ' | 'POWR'
4       4     u32     version (FREQ/POWR pairing sequence)
8       4     u32     points
12      4     f32     sweep_ms (SDK-estimated minimum SWP time, ms; 0 if unavailable)
16      ...           data
```

| magic | data type | description |
|---|---|---|
| `FREQ` | float64 × points | frequency axis (Hz), sent only on version change |
| `POWR` | float32 × points | power trace (dBm), paired with same-version FREQ |

**RTA2 / RTAF — 24-byte base header:**

```
magic(4) + ver(u32) + points(u32) + wfLen(u16) + maxDensity(u16) + startHz(f64)
+ float64[points] + float32[points] + uint16[wfLen] + stopHz(f64)
```

RTA emits `RTA2`: after the base payload, append ASCII `TRIG`, a little-endian
u32 UTF-8 JSON byte length, then the trigger metadata JSON described below.
SDR panadapter emits legacy `RTAF` without the trailer. The browser can read old
RTAF recordings, but those frames cannot establish hardware capture completion.
Refresh older clients for RTA2 support. `points` is the display trace length.

#### 2.3 Native receiver subscriptions

| Endpoint | Binary output |
|---|---|
| `/ws` | display FREQ/POWR/RTA2/RTAF plus JSON commands/status |
| `/ws?iq=1` | primary IQBF |
| `/ws?iqaux=1` | auxiliary IQAX |
| `/ws/audio?channel=1` | AUD2 receiver PCM |
| `/ws/decode?channel=1` | SDE2 receiver events |

Audio/decode `channel` is optional: omit it for all channels, or select slot 1–3; invalid values return HTTP 400. These subscriptions do not carry display frames. The backend demodulates; the browser resamples PCM for playback and displays decoded results. Current product controls expose the primary channel and disable auxiliaries on entry to SDR. Backend subscriptions support three slots.

Stream identity is `(epoch, channel_id)`. Retuning assigns a new channel ID; replanning, recapture and new sessions assign a new epoch. Rejected requests preserve the previous configuration and stream. Old output tails precede the new start boundary. Each audio/decode subscriber receives its own sequence starting at zero, without resetting the receiver or other subscribers. Source sequence gaps and format changes without a start marker fail explicitly.

Streaming uses a bounded 64-message per-client FIFO. Overflow closes the slow subscription with code 1013; samples/events are not dropped to continue playback. Display traces instead use latest-wins delivery. See [ARCHITECTURE.md](ARCHITECTURE.md) for processing ownership and [KNOWN_ISSUES.md](KNOWN_ISSUES.md) for receiver limitations.

A shared native receiver worker failure stops all audio/decode receivers. The display/control stream receives `RECEIVER_ERROR` with `{error, failed:true, scope:'all_receivers'}`. STATUS retains the diagnostic in `sdr.receiver_error:string|null`. Audio/decode subscriptions close with code 1011 and reason `receiver failed`. Capture, PFB, fine DDC, raw IQ, spectra and waterfall continue. Receiver failure does not set `last_error` or `ddc_error`, add a spectrum error overlay, or change hardware `connected` state. Capture and device faults use global error handling.

Raw IQ and spectrum processing use bounded queue backpressure. Queue backlog waits until explicit failure without reopening the device. A sustained SDK stall ends capture with its original cause; recovery requires a new capture epoch.

Tuning, filter edits and zoom remain available. Recovery requires a demodulator or channel command with explicit `mode` or `demod`; selecting the current mode also retries. Frequency-only and filter-only commands, such as `SET_SDR_DEMOD {ifbw}`, retain the failure. Until stopped workers exit and the publisher closes failed subscriptions, recovery requests return `code:'receiver_retiring'`. Retry later. The service does not schedule automatic restarts or allow unbounded replacement threads.

Successful recovery clears `sdr.receiver_error` and gives each active channel a fresh stream identity and start boundary. Clients must discard old PCM and decoder state before consuming the new stream. A failed receiver cannot guarantee a complete ordered tail. Samples across a missing IQ interval are not joined.

Audio and decode controls show the error inline. The Decode row retains it when the floating decoder is closed. Retry uses the existing demodulator/channel controls. The browser clears the failure only when STATUS reports `sdr.receiver_error:null` with fresh active channel identities.

AudioContext setup/resume errors affect playback only. Stale worker callbacks are ignored. Local decoder/setup failures remain latched until explicit retry or a healthy fresh identity.

#### 2.4 IQBF/IQAX v2 header

All multibyte binary fields are little-endian. IQ frames contain normalized ADC full-scale samples (`1/32768`), with no framing AGC or clipping; filter overshoot may exceed `[-1,1]`.

| offset | size | field |
|---:|---:|---|
| 0 | 4 | magic `IQBF` or `IQAX` |
| 4 | 4 | version, u32 = 2 |
| 8 | 4 | seq, u32; 0 is the empty start marker |
| 12 | 4 | samples, u32 complex samples |
| 16 | 8 | rate, f64, output Sps |
| 24 | 8 | center, f64, Hz |
| 32 | 4 | channel_id, u32 |
| 36 | 4 | epoch, u32 |
| 40 | 8 | input_index, u64, capture complex samples |
| 48 | 8 | group_delay_samples, f64, output complex samples |
| 56 | 8×samples | interleaved I/Q, f32 |

`input_index` is the first output sample's causal position in the plan input, not the packet start; delay is separate. Alignment is `input_index / input_rate - group_delay_samples / rate`. The native path provides an exact index; synthetic sources use their own sample clock and zero delay. Offline reference fixtures may use `u64::MAX` for unavailable timing; never align that sentinel. Preserve u64 values as integers (JavaScript `bigint`).

`STATUS.channel_registry`, `actual.channel_registry` and `sdr.actual.channel_registry` share a snapshot. Entries contain `channel`, `channel_id`, `epoch`, `frequency_hz`, `output_rate`, `input_rate`, `group_delay_samples`, `state="active"`. Disabled channels are absent; outside SDR the registry and channel clocks are empty. The service does not negotiate v1.

#### 2.5 AUD2 audio header and clock

AUD2 has a 96-byte header followed by interleaved finite f32 PCM: total bytes = `96 + frames * channels * 4`.

| offset | type | field |
|---:|---|---|
| 0 | 4 bytes | `AUD2` |
| 4 | u32 | version = 1 |
| 8 | u32 | seq; 0 is an empty start marker |
| 12 | u32 | PCM frames (one sample per channel per frame) |
| 16 | f64 | actual PCM rate, Hz |
| 24 | f64 | tuned frequency, Hz |
| 32 | u32 | channel_id |
| 36 | u32 | epoch |
| 40 | u64 | input_index, capture complex samples |
| 48 | f64 | delay_seconds, finite and nonnegative; known portion if total delay is unknown |
| 56 | u32 | channels, 1 or 2 |
| 60 | u32 | low 16 bits: format = 1 (f32); bit 16: total delay unknown; bit 17: causal presentation clock |
| 64 | u64 | audio_index, PCM frame position |
| 72 | f64 | input_rate, capture Sps |
| 80 | u64 | input_fraction_num |
| 88 | u64 | input_fraction_den |
| 96 | f32 array | interleaved PCM |

Rates are finite and positive, IDs/epoch nonzero, and `0 <= num < den` with `den >= 1`. When total delay is known and the clock represents source samples, alignment is `(input_index + num/den) / input_rate - delay_seconds`. Unknown delay or causal presentation does not establish exact source alignment. Protocol blocks can share an input position or jump to the next protocol frame; validate PCM continuity with `audio_index`, rather than inventing input positions from PCM duration. Delay may vary per frame.

An NR tail reset can advance the AUD2 source position over discarded reducer samples while `audio_index` stays continuous. This forward source gap does not authorize overlapping or backward sample spans, which are rejected.

A new subscriber gets an empty `seq=0` retaining the first received block's global `audio_index` (possibly nonzero), then `seq=1`. Codec rate/channel changes may send a new start within the same IQ identity: `audio_index` resets to zero, the next sequence is 1, and the player clears queued PCM and rebuilds its rate adapter. Codec state remains valid. Mode changes mute old PCM until the new boundary.

Playback feedback is internal browser messaging, not a WebSocket acknowledgement API. The ingress sends `{type:'samples', samples, stereo, delivery, generation}`; the sink grants cumulative `{type:'credit', generation, limit}` capacity and confirms `{type:'accepted', generation, delivery}`. Only matching generations apply. Unsent and unacknowledged frames share a three-second delivery budget; overflow fails playback. Receiver mode selects buffering: analog startup 450 ms; DRM/DRM+ startup 1.6 s and queue limit 2.4 s.

#### 2.6 SDE2 decoded events

| offset | type | field |
|---:|---|---|
| 0 | 4 bytes | `SDE2` |
| 4 | u32 | UTF-8 JSON byte length, 1 byte through 2 MiB inclusive |
| 8 | UTF-8 | JSON envelope, exactly the declared length |

Envelope fields: `version=1`, `channel` (slot 1–3), `channel_id`, `epoch`, `seq`, `input_index`, `input_rate`, `input_fraction_num`, `input_fraction_den`, `delay_seconds`, `delay_known`, `causal_presentation`, `timing_kind`, `mode`, `event`. The three u64 position/fraction fields are decimal strings. Clock units and flags match AUD2. `timing_kind` is `sample`, `protocol_provenance`, or `causal_presentation`; protocol provenance does not imply a recovered sample timestamp. FT8 results retain their search-window time rather than asynchronous completion time. `event` contains mode-specific results; sequence zero carries `{type:'start'}`.

AM emits `event.type:'am-info'` on a 20 Hz source-sample grid, independent of browser rendering and audio callback sizes. Its SDE2 envelope uses the source clock and delay flags above. Payload fields are:

| Field | Type / meaning |
|---|---|
| `amSyncGeneration` | u32; actual receiver configuration generation corresponding to STATUS `am_sync_generation` |
| `syncEnabled` | boolean; actual synchronous detector state, not a pending command |
| `syncLocked` | boolean; enabled and carrier evidence passes power, coherence and phase/frequency consistency checks after a dwell |
| `carrierHz` | number or null; absolute recovered RF carrier in Hz, computed from the actual receiver frequency plus `carrierOffsetHz`; null without carrier evidence |
| `carrierOffsetHz` | number or null; recovered carrier offset in Hz, available with valid carrier evidence in either detector mode |
| `modulationPct` | number or null; `100 * sqrt(2 * variance(envelope)) / mean(envelope)`, measured before AGC over a source-clock window; single-tone-equivalent RMS depth, not programme peak depth |
| `inputDbFs` | number or null; raw IQ level `10*log10(mean(I²+Q²))` in dBFS; zero power is null |
| `rmsDbFs`, `peakDbFs` | numbers; final PCM RMS and peak in dBFS, with a -160 dB floor at silence |
| `scope` | array of at most 256 samples from actual final PCM after listener processing, NR and gain |

Unavailable carrier frequency/offset/depth are null, never fabricated zeros; deep carrier fading invalidates them. AM does not report SNR. `syncLocked:false` is expected with sync disabled even when carrier metrics are valid. Recovery uses coarse frequency acquisition followed by integral phase tracking with separate 150 ms acquisition/loss dwell; deep fades invalidate immediately. Input power uses 100 ms windows with 500 ms exponential smoothing. Envelope variance is normalized within each 100 ms window before its modulation power is smoothed over 1 s, avoiding false modulation from slower RF fading. Carrier offset uses 250 ms smoothing after acquisition. Audio waveform samples remain actual PCM.

The browser accepts AM telemetry and lock only when `amSyncGeneration` matches the authoritative STATUS generation; an unknown generation is not accepted. This rejects queued telemetry from an earlier switch even after off/on returns to the same boolean value. Device/control disconnect invalidates telemetry and generation; decode resumes only after STATUS confirms connection.

In WFM `wfm-info` events, `carrierHz` is the receive stream's channel-centre frequency (from the channelizer stream identity), not an independently measured carrier estimate; the remaining fields (deviation, pilot, RDS, scopes) are demodulated measurements.

SSTV subscribers receive a start and a bounded snapshot of the current image before live row increments. Image events contain `type:'sstv-image'`, `width`, `height`, `row`, `rows`, packed RGB byte array `rgb`, and `meta`; snapshots are split into at most 64 rows each. The service retains only the current image; browser history is local.
Picture boundaries remain ordered. Pinning an SSTV mode again starts a fresh canvas; long-silence timing stays bounded.

The browser delivers event batches FIFO using `{type:'frames', envelopes}` and acknowledges application with `{type:'ack'}` on its internal worker port. Queued plus in-flight data is limited to 32 events / 4 MiB; overflow explicitly fails. This acknowledgement is not sent over `/ws/decode`.

> **Spectrum detail**: POWR uses float32 levels and the paired native FREQ Hz axis. RTA/SDR retain native bins for traces, markers and export. Local zoom sends no acquisition commands and cannot improve physical RBW. Density keeps either the processed active trace or the received frames before Average/Max Hold; switching the source clears history. Afterglow uses a real-time half-life (Off, 0.2, 0.5, 2, 5, 10 s). Hit weights use elapsed time with bounded gaps. Off stops accumulation and drawing. Both, Density and Trace change visibility only; trace modes, markers, measurements and export retain their data sources. The wide trace glow is suppressed while density is drawn. Contrast changes colour independently of opacity. Waterfall history retains native frequency positions; missing bins stay blank unless SWP GapFill is enabled.

On an axis-version transition, the first frame for the new axis takes priority and
older-version frames are rejected. Density projects native frequency centers continuously,
splits each amplitude contribution between two rows, and draws measured cell centres with a
centred vertical footprint of 1/3/5/9 CSS pixels (default 5). Display width changes only
the pixel footprint; it does not change occupancy, amplitude, frequency or measurements.
Contrast controls colour separately from opacity. Light mode uses a saturated palette;
measured noise remains visible at reduced strength. Grain defaults to 128; NaN gaps stay
blank. Off clears accumulation. VIEW freezes trace-source history; Live continues while
the trace is frozen. Width, theme and contrast changes preserve history.

SWP/RTA/SDR raster paths use each device-pixel column's first, minimum, maximum and last original vertices, in source order, and split at every missing bin. Zoom reveals the retained native vertices as they occupy separate pixels; markers and export always use the complete source arrays. Optional SWP adaptive smoothing still uses the full frame, consistently with marker readouts.

The global navigator connects native vertices, retaining each bounded column's first/minimum/maximum/last samples in source order and breaking at missing bins. Sparse captures remain a continuous outline; this does not add measurement resolution.

CSV exports the selected current-mode processed trace and matching native axis;
OFF/missing/mismatched data produces a no-data notice, never an old SWP fallback.
Limits/channel measurements require the selected processed absolute trace;
normalized traces are N/A and emit no absolute measurement CSV rows. RTA/SDR
disable these measurement controls. dBc receives no external level offset.
SWP waterfall rows come from accepted raw POWR at most 10 Hz, not repaint calls.



---

## 3. Measurement Modes

### Harmonic

1. `SET_MODE {mode:'harmonic'}` → `SET_HARM {f0, count, span}`
2. Server auto-tunes per harmonic (H1~H5…), pushes `HARM` per result

### Phase Noise

1. `SET_MODE {mode:'pnm'}` → `SET_PNM {center, threshold, traceavg, start, stop}`
2. Server does incremental acquisition (`PNM_GetPartialUpdatedFullTrace`), pushes `PNM` per frame (with progress)

Each Harmonic/PNM Set starts a new `session_id`; successful command STATUS echoes it,
and HARM/PNM results carry the applied session ID. Clients reject older session results,
including an old hardware cycle finishing during reconfiguration. The browser clears
previous results on Set. PNM averaging has its own visible control.

### SDR (software-defined receive)

1. `SET_MODE {mode:'sdr'}` → optionally `SET_SDR {center, decimate}` (capture window)
2. `SET_SDR_TUNE {listen}` tunes the primary receiver (native: recaptures when needed, preserving auxiliary frequencies); `SET_SDR_CHANNEL {channel:1, freq}` uses the same native tuning behavior. `SET_SDR_DEMOD` selects the demodulator and its audio settings
3. The server pushes `RTAF` (panadapter spectrum + waterfall) and subscribed `IQBF` baseband. Native receiver audio uses `/ws/audio`; decoder events use `/ws/decode`. Level/squelch/ADM metrics are in `STATUS.sdr`.

On the native backend, SDR capture centers use the model's IQ tuning range
intersected with the device RF range (SAN-90: 9 kHz–9 GHz). The reported
`caps.fmin`–`caps.fmax` receiver range may be wider after sweep readback
(SAN-90: 8 kHz–9.02 GHz). For example, an 8 kHz receiver can use a 9 kHz IQ center.
The IQ capture and displayed spectrum may extend past either RF limit; their
half-width is not added to the minimum center or subtracted from the maximum.
Changing display bandwidth therefore does not force an otherwise valid center
away from a device limit. Frequency-axis drag and wheel gestures request a center
within the reported RF range; the backend confirms the IQ-compatible center.
Dragging within the plot only tunes within the displayed window; reaching an edge does not pan it. Use the frequency axis to move the window. Listen dragging and arrow tuning stop at the RF limits; clicks outside them do not tune a receiver. Enabled receiver frequencies must remain
inside the RF range, and their occupied bandwidth and coarse-channel transition
must still fit the capture passbands. An incompatible auxiliary receiver rejects
the change rather than moving that receiver. RTA retains its whole-window limits.

### Exit measurement

`SET_MODE {mode:'std'}` → restore standard sweep.

---

## 4. Python Example

```python
import asyncio, json, struct
import aiohttp

async def main():
    async with aiohttp.ClientSession() as s:
        # 1) REST state
        async with s.get('http://localhost:8080/api/state') as r:
            st = await r.json()
            print('connected:', st['connected'], '| mode:', st['mode'])

        # 2) WebSocket trace frames
        async with s.ws_connect('ws://localhost:8080/ws') as ws:
            await ws.send_str(json.dumps({'cmd': 'SET_FREQ', 'center': 1e9, 'span': 100e6}))
            async for msg in ws:
                if msg.type == aiohttp.WSMsgType.BINARY:
                    head = struct.unpack('<4sIIf', msg.data[:16])
                    magic, ver, pts, sweep_ms = head[0], head[1], head[2], head[3]
                    if magic == b'FREQ':
                        freq = struct.unpack_from('<%dd' % pts, msg.data, 16)
                    elif magic == b'POWR':
                        import array
                        pwr = array.array('f')
                        pwr.frombytes(msg.data[16:])
                        print('POWR', ver, 'points', pts, 'peak', max(pwr))
                elif msg.type == aiohttp.WSMsgType.TEXT:
                    obj = json.loads(msg.data)
                    print('JSON:', obj.get('cmd'))

asyncio.run(main())
```

---

## 5. Notes

- **Single handle**: only one process may open the device at a time (SAStudio4 and the Web service are mutually exclusive)
- **Serialized device calls**: the native SDK shim owns a retained session thread and serializes access.
- **Binary SDK**: `libhtraapi.so` is proprietary, obtain it from HAROGIC (the `htra_api.py` wrapper is bundled)
- **SystemClockSource=External**: dangerous, do not use (hangs the device)


## Trigger reference

`SET_TRIGGER` has eleven parameters. Requests remain in `req.rta.trigger_<field>`;
`req.rta.trigger_actual` reports SDK-applied source, edge, level, safetime, delay,
pretime, acqtime, retrigger, retriggerperiod, out and outpolarity, plus runtime metadata.
Applied settings come from ProfileOut, and `poi` from SDK FrameInfo. Missing proof is
unknown/null; requests are never substituted for applied settings.

RTA2 TRIG metadata includes `valid`, `supported`, `simulated`, `generation`, `frames`,
`capture_id`, `packet_index`, `packet_count`, `waiting`, `complete`,
`trigger_missed` and `last_sdk_status`. Generation advances on successful configuration;
frames counts successful SDK packet acquisitions, not STATUS polls. Capture identity is
null when the SDK has none. Packet index is one-based since BusStart, becoming null
after continuity loss; packet count is known only for FixedPoints. Waiting is null
without SDK proof: -10 can mean trigger waiting or a communication error.

For finite Level/FixedPoints captures without retrigger, completion requires uninterrupted
packet continuity and the final configured packet or SDK status -301/-303. Even -301/-303
cannot establish a complete assembled capture after data loss. Timer, adaptive and
retrigger boundaries remain unknown without proof. -302/-303 sets `trigger_missed`;
-303 retains the completed capture and holds its final data while reporting the missed
trigger. A valid complete packet is processed before freezing; older generations are
ignored. No delay or first frame establishes completion. Finite Level capture retains
the maximum across its traces and packets. Simulated RTA reports unsupported, invalid
metadata and cannot produce a hardware HIT. Clock-relocked statuses -36..-39 retain
valid RTA data and last_sdk_status; Auto treats them as warning/pending.

After proven completion, the server stops SDK reads and retains the exact completed
RTA2 for that generation, republishing it at 5 Hz for late subscribers. Replays do not
advance frames, packet_index or Auto observations. Successful reconfiguration,
rearm/Free Run, rollback restart, invalidation and mode exit clear the retained frame.
This is a server-held capture; the finite SDK acquisition need not stop producing packets.

In standard SWP, Capture uses a browser threshold crossing between consecutive
display traces; only level, edge and capture helpers apply. RTA supports device timing;
SDR/harmonic/PNM disable these controls. External/timer API states are displayed
read-only. Free Run/Escape releases a held capture; successful mode/Preset transitions
reset local hold. RTA entry selects bus. Auto Scale does not release an armed trigger.

## Auto Scale and local level controls

Explicit Auto runs for at most 20 seconds / 24 steps. Each step requires fresh data
from the same mode/configuration. SDK IF overflow (-12) raises configured actual Ref
by 10 dB up to `caps.ref_max`, without lowering an already higher Ref or changing gain
selectors. Waiting/no data alone never raises Ref. A clean spectrum drives placement,
then fresh clean data must confirm it. Recovery does not lower hardware Ref back into
the proven overload region. Ordinary SDR fitting changes only display Ref; explicit
overflow recovery may reconfigure IQS through normal admission. Entry fitting uses
`display_only:true` and never changes hardware.

`auto_ref.result` is `idle`, `adjusting`, `applied`, `ok`, `no_data`, `overflow`,
`limit`, `stalled`, `failed`, `cancelled` or `timeout`. `target` is non-null only on
successful completion. No usable acquisition ends as no_data; an adjusted but
unconfirmed session ends as timeout. Manual Ref/AMP, frequency, mode and Preset edits
cancel the session and clear its old target. There is no background overflow recovery.

Manual Ref edits use an independent local viewport (-200..+100 dBm plus external Offset)
in SWP/RTA/SDR. SET_REF and AUTO_SCALE.current_ref subtract Offset and clamp to the existing
-160..+40 dBm wire bounds. A matching raw request report accepts the viewport without replacing
it: display +100 with Offset zero requests hardware +40. Further viewport moves with the same
clamped request do not reconfigure hardware or restart its pending timeout. Rejection or timeout
restores the accepted display. SDR stored `sdr.ref` confirms the request separately from effective
`ref`. Auto placement adds Offset once. Offset edits shift the absolute axis and trace together
without hardware writes; relative dB is unchanged. Waterfall Fixed
range requires finite low < high; rejected edits revert with a notice. Mapping/range
changes clear quantized history even while paused. Local Scale/Unit/Offset/Waterfall
remain usable in std/rta/sdr; exclusive harmonic/PNM disables Level controls.
