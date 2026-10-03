# DRM via Dream (background decoder)

This page documents the DRM receive mode that runs the **external Dream receiver** as a
background process and pipes the SDR pipeline's channelized baseband into it. It is the
"use the existing decoder" path; the Rust/WASM reimplementation on `feature/drm-demod` is a
separate, independent effort and is not touched by this work.

## Isolation

The work lives in its own worktree/branch and never shares ports or IPC with the other pi
worktree (`/home/hui/git/harogic-websa`, branch `feature/drm-demod`, which owns 8080/8099).

- Branch: `feature/drm-dream-decoder` (cut from `master`).
- Worktree: `/home/hui/git/harogic-websa-drm-dream`.
- Web service port: `WEBSA_PORT=8180`; e2e/fake port: `WEBSA_FAKE_PORT=8199`.
- IPC: `/tmp/drm-dream-*` (status socket, sinks), namespaced by PID at runtime.
- `tools/drm_dream/env.sh` is the single place these are declared; source it before running.

## Decoder binary

Dream's `--status-socket` (the newline-delimited JSON status stream this integration reads) is
**not** in the packaged binaries:

| Binary | Build | `--status-socket` |
| --- | --- | --- |
| `dream-nox` 2.2.4 (AUR) | wwek fork, `CONFIG+=qtconsole` | absent (not compiled) |
| `dream` 2.3 (AUR) | upstream `Drm-tools/dream`, CMake `USE_QT=ON` | absent |

The vendored console build of the wwek fork (`qmake CONFIG+=console`) is the only one that
starts `CStatusBroadcast`. Build it into the worktree (git-ignored):

```
tools/drm_dream/build_dream.sh          # -> tools/drm_dream/build/dream
```

`DRM_DREAM_BIN` overrides the path. Otherwise `web_sa/measurements/sdr.py::_default_dream_bin()`
resolves the decoder by capability: it tries `DRM_DREAM_BIN`, `/usr/local/bin/dream`,
`/usr/bin/dream` and finally the vendored build, and uses the first whose `--help` advertises
`--status-socket`. An installed binary that gains the feature is therefore used automatically. The
full rationale (and the user's decision to vendor the console build) is in
`tools/drm_dream/DECISION.md`.

## Data path

```
SDR DDC (i, q, f_out ~= 48 kHz, f_out is *measured*, not nominal)
  -> low-pass -> linear resample to 48 kHz -> DreamDecoder.feed()
  -> pacat --raw -> private null sink -> dream -I <sink>.monitor -c 6 --sigsrate 48000
       |- --status-socket -> newline-delimited JSON -> state.sdr_drm
       `- -O <audio sink> -> parec -> mono int16 -> AUDF frames
```

The resample uses the channelizer's **measured** output rate (`_measure_baseband_rate`).
Resampling from the nominal figure leaves a slow drift against the 48 kHz sound card, and the
receiver then re-locks every second or so; the measured rate holds `msc=0` about 90% of the time
where the nominal held ~50%.

Audio is captured live from a second private null sink with `parec`. Dream's own
`-w/--writewav` was not usable (the file stayed at 0 bytes while the receiver decoded).

## Selecting it

Pick `DRM` in the SDR panel's demodulator group (the mode is a backend/server mode, so it has
no browser kernel). While it is selected, `STATUS.sdr.drm` carries the decoded metadata and the
DRM readout shows station, robustness mode, bandwidth, bitrate, codec and sync. Audio plays
through the normal SDR audio switch.

## Bench transmitter (PlutoSDR)

`tools/pluto_drm_tx.py` streams a DRM signal from the PlutoSDR so the receiver can be tested
without waiting for shortwave propagation.

- The AD9363 TX cannot tune HF (>= ~325 MHz), but a DRM signal is just an OFDM waveform: the
  carrier frequency is irrelevant to Dream. Transmit at UHF (400 MHz default) and tune the
  analyzer there. A **coax cable + attenuator** from the Pluto TX to the SAN-90 RF input is
  preferable to radiating (controlled level, no propagation, no interference).
- The IQ input is an int16 WAV from the DecDRM transmitter (`decdrm tx station.toml --output
  drm_iq.wav --duration 60`, `format = "iq"`, default `iq_swap` = I on the left). Dream only
  decoded the int16 WAV, not the float32 one.
- The DRM baseband sits at `--base-hz` (default +100 kHz), clear of the LO leakage; tune the app
  to `LO + base_hz`. Do not conjugate the baseband (leave `--conj` off): the default orientation
  is what Dream's `-c 6` expects.
- The default is a cyclic DMA buffer (`--stream` uses chunked one-shot buffers). The cyclic wrap
  and the Pluto/SAN-90 clock offset are the main sources of intermittent MSC errors on a bench
  loop; a real off-air signal has neither.

## Verification

```
source tools/drm_dream/env.sh
python3 tools/drm_dream/probe_fixture.py          # fixture -> Dream metadata vs manifest
python3 tools/drm_dream/loopback_probe.py         # same over the null-sink loopback
python3 -m pytest tests/test_drm_dream*.py tests/test_drm_state.py -q
DRM_DREAM_AUDIO_FIXTURE=/path/to/real.rec python3 -m pytest tests/test_drm_dream_audio.py -q
WEBSA_FAKE=1 WEBSA_PORT=8180 python3 -m web_sa.supervisor &   # then:
python3 tools/e2e/drm_mode.py --url http://127.0.0.1:8180
```

`WEBSA_DRM_DUMP=<path>` (env) appends the exact baseband handed to Dream to a raw int16 file, for
comparing the live feed against a captured sink monitor when localising a decode problem.

## Known limitations

The synthetic `tests/fixtures/drm/drm_modeB_so3_48k.f32` fixture (from `feature/drm-demod`)
decodes **metadata** correctly, but Dream reports its MSC frames as `CRC_ERROR` and rejects the
audio: a DecDRM-transmitter <-> Dream interop difference, not a build problem. Audio capture is
therefore verified against a real recording.

On a real antenna, real DRM stations decode (metadata and audio), but a weak signal only holds
the MSC for short stretches: DRM needs roughly `MER >= 15 dB` for xHE-AAC audio. On the Pluto
bench loop the receiver holds `msc=0` about 90% of the time; the remaining drops come from the
cyclic buffer wrap and the Pluto/SAN-90 clock offset, not from the app.
