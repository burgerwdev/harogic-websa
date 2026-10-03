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
SDR DDC (i, q, ddc.fs_out ~= 48 kHz)
  -> low-pass -> linear resample to 48 kHz -> DreamDecoder.feed()
  -> pacat --raw -> private null sink -> dream -I <sink>.monitor -c 6 --sigsrate 48000
       |- --status-socket -> newline-delimited JSON -> state.sdr_drm
       `- -O <audio sink> -> parec -> mono int16 -> AUDF frames
```

Audio is captured live from a second private null sink with `parec`. Dream's own
`-w/--writewav` was not usable (the file stayed at 0 bytes while the receiver decoded).

## Selecting it

Pick `DRM` in the SDR panel's demodulator group (the mode is a backend/server mode, so it has
no browser kernel). While it is selected, `STATUS.sdr.drm` carries the decoded metadata and the
DRM readout shows station, robustness mode, bandwidth, bitrate, codec and sync. Audio plays
through the normal SDR audio switch.

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

## Known limitations

The synthetic `tests/fixtures/drm/drm_modeB_so3_48k.f32` fixture (from `feature/drm-demod`)
decodes **metadata** correctly, but Dream reports its MSC frames as `CRC_ERROR` and rejects the
audio: a DecDRM-transmitter <-> Dream interop difference, not a build problem. Audio capture is
therefore verified against a real recording. A real DRM broadcast decodes audio without changes.
