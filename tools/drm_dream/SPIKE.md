# DRM-via-Dream: feed & decoder spike (task-2)

Branch `feature/drm-dream-decoder`, worktree `/home/hui/git/harogic-websa-drm-dream`,
based on `master` (983f6a2). Isolated from the other pi worktree
(`/home/hui/git/harogic-websa`, branch `feature/drm-demod`): we use
`WEBSA_PORT=8180`, `WEBSA_FAKE_PORT=8199` and `/tmp/drm-dream-*` IPC paths
(see `env.sh`).

## Which `dream` binary

| Binary | Build | `--status-socket` | Result |
| --- | --- | --- | --- |
| `/usr/bin/dream` (was `dream-nox` 2.2.4, wwek fork) | `qmake CONFIG+=qtconsole` | **absent** (not compiled) | no metadata |
| `/usr/bin/dream` (now `dream` 2.3_qt6-3, upstream GUI) | CMake `USE_QT=ON` | **absent** | metadata only via `DreamLog*.csv/.txt` |
| `tools/drm_dream/build/dream` (vendored) | wwek fork `v2.2.4`, `qmake CONFIG+=console CONFIG+=fdk-aac` | **present** | full JSON metadata + audio |

`CONFIG+=console` selects `src/main.cpp`, which starts `CStatusBroadcast`; the
`qtconsole` and GUI builds use `src/main-Qt/main.cpp`, which never does. Build
the vendored binary with `tools/drm_dream/build_dream.sh`.

## Status protocol

Dream listens on the Unix socket (it is the **server**); a client connects and
receives one JSON object per line at ~2 Hz. Verified fields include
`status.{io,time,frame,fac,sdc,msc}` (0 = OK, see `ETypeRxStatus`),
`signal.{snr_db,mer_db,doppler_hz,...}`, `mode.{robustness,bandwidth_khz,interleaver}`,
and `service_list[]` with `label`, `bitrate_kbps`, `audio_mode`,
`protection_mode`, `language`, `country`.

## Feed paths

1. **File input (`-f file.wav`) — used for deterministic tests.**
   `libsndfile` path (WAV, int16 stereo, I=L/Q=R). Dream only accepts raw
   `.iqNN`/`.ifNN` as PCM16 too, but that path did **not** lock on our fixture;
   the WAV path did. At EOF Dream **rewinds to offset 0** (`sf_seek(...,0)` in
   `CAudioFileIn::Read`), so a growing file cannot be streamed — it would replay
   from the start. Channel select `-c 6` (I/Q positive, 0 Hz IF) locks our
   DC-centred baseband. Verified: SNR 37–41 dB, label `SAN90 DRM TEST`,
   mode B, 10 kHz, 20.96 kbps.
2. **Soundcard loopback — used for the live pipeline.**
   `module-null-sink` + `pacat --raw --format=s16le --rate=48000 --channels=2`
   writing interleaved I/Q, with `dream -I <sink>.monitor -c 6 --sigsrate 48000`.
   Continuous, real-time, no EOF rewind. Verified: SNR 40.8 dB, label
   `SAN90 DRM TEST`, mode B, 10 kHz. This is what the SDR DDC baseband will feed.
3. **`--rsiin` RSCI/MDI socket — not used.**
   Dream can ingest an RSCI/MDI stream, but the SDR side would have to implement
   the RSCI packetiser; the loopback delivers the same IQ with far less code.

## Ground truth

`tests/fixtures/drm/drm_modeB_so3_48k.f32` + `manifest.json`, copied read-only
from `feature/drm-demod` (byte-identical, sha256
`6c1a6e5d…1a1cd5`). Decoded metadata matched: label `SAN90 DRM TEST`,
service id `123456`, mode B, SO3/10 kHz, 64-QAM SM, 20.96 kbps, EEP, Mono,
`eng`/`gb`.

Run: `python3 tools/drm_dream/probe_fixture.py` (exit 0 = match).

## Decoded audio capture (task-3)

`-w/--writewav` is unusable here: the file stays at 0 bytes even while Dream decodes
audio. Instead Dream plays to a second private null sink (`-O <audio_sink>`) and
`parec --device=<audio_sink>.monitor --rate=48000 --channels=2 --format=s16le`
captures the decoded PCM, which is downmixed to mono int16 for AUDF frames.

**Dream must not be muted for this path**: with `-m 1` the receiver mutes its
audio output and the sink captures pure silence. `DreamDecoder` therefore passes
`-m 0` when `capture_audio=True` and `-m 1` otherwise.

### Fixture audio limitation (important)

The synthetic `drm_modeB_so3_48k.f32` fixture decodes **metadata** correctly, but
Dream reports `status.msc = 1` (CRC_ERROR) for it and its AAC frames are rejected
(`zero output channels: 0`), so no audio comes out. This reproduced on the
vendored wwek console build *and* upstream Dream 2.3, so it is a DecDRM-transmitter
<-> Dream interop limitation, not a build/config problem. DecDRM's own receiver was
used as the fixture's self-check oracle; Dream needs a real broadcast recording.
Audio capture is therefore verified against Dream's bundled real recording
(`test_data/drm-XHE-AAC-iq.rec`, non-silent RMS ~2.27k):

    DRM_DREAM_AUDIO_FIXTURE=/path/to/drm-XHE-AAC-iq.rec \
        python3 -m pytest tests/test_drm_dream_audio.py -q
