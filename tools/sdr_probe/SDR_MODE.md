# SDR mode (backend + frontend spike)

A first working vertical slice of a web SDR on the SAN-90, built on the verified
IQS / DSP_DDC / ADM capabilities (`FINDINGS.md`).

## Data path

```
IQS Adaptive IQ stream (int16, 62.5 MSPS / 2**n)
  -> Panadapter FFT (numpy, dBm)          -> RTAF frame  (reuses the RTA renderer)
  -> DSP_DDC (offset + decimate, float)   -> AnalogDemod -> AUDF frame (48 kHz PCM)
  -> ADM_* (AM/FM)                        -> STATUS.sdr.adm
```

- `web_sa/demod/filters.py` — streaming FIR (complex band-pass), linear resampler, AGC
- `web_sa/demod/ddc.py`      — `DdcChannel`, the verified `DSP_DDC_*` wrapper
- `web_sa/demod/demod.py`    — `AnalogDemod`: AM / FM / NFM / WFM / USB / LSB / CW
- `web_sa/demod/spectrum.py` — `Panadapter`: FFT + waterfall row (dBm)
- `web_sa/measurements/sdr.py` — `SdrSession` (owns IQS, emits `RTAF` + `AUDF`)

## Commands (WebSocket / REST)

| Command | Params | Effect |
|---|---|---|
| `SET_MODE` | `mode: "sdr"` | enter SDR (exits SWP/RTA/harm/pnm) |
| `SET_SDR` | `center`, `decimate` | wideband center / capture bandwidth (power of two) |
| `SET_SDR_TUNE` | `listen` | demod frequency (clamped to the captured band) |
| `SET_SDR_DEMOD` | `mode`, `ifbw`, `squelch`, `volume`, `agc`, `pitch` | demod chain |

`STATUS.sdr` reports `{center, decimate, listen, demod, if_bw, squelch, volume, agc,
pitch, actual{...}, level_dbfs, squelch_open, adm}`.

## Frames

- `RTAF` — identical layout to the RTA frame, so the existing spectrum/density/
  waterfall renderer is reused unchanged.
- `AUDF` — `magic(4) + seq(u32) + rate(u32) + samples(u32) + int16 PCM`.

- **Stability**: `IQS` `BusTimeout` is 250 ms and the session waits ~0.4 s after
  `IQS_Configuration` before fetching, so entering SDR no longer produces an initial
  `BusDataError` burst and the stream no longer stalls during steady operation
  (verified 0 errors over 6 s and 16/16 mode switches under a live stream).
- **Tuning is DDC-only**: `set_tune` reconfigures just the `DSP_DDC` offset and calls
  `AnalogDemod.retune()` (clears filter/discriminator state, keeps the AGC gain), so
  switching stations is click-free (no "noisy then clear"). A 120 ms fade-in hides any
  reconfiguration transient.
- **Auto-scale**: smoothed noise floor + peak (EMA, 3 dB deadband, 400 ms rate limit)
  sets the display ref so the noise floor sits ~8 dB above the bottom and the peak is
  never clipped; it no longer flashes when a signal fades. The shared Ref group / Auto
  button overrides it.
- **Peak list reuse**: peak-list rows in the swept view are clickable and hand their
  frequency to the SDR demod (no duplicate "station list").
- **Removed** the SDR band-preset row (superseded by the sweep -> SDR handoff).

## Recommended workflow (sweep -> locate -> demod)

The SAN-90's strength is the 9 GHz swept spectrum; IQ streaming is for the narrow demod
window. The two are combined into one flow:

1. **Observe** in the normal swept (SWP) view: set center/span/start/stop freely, use the
   span step arrows to narrow down progressively, watch markers/peaks.
2. **Locate**: click the signal on the swept spectrum (this places/enables the active
   marker at that frequency).
3. **Listen**: press **SDR** — the SDR mode is centred on the active marker (or the SWP
   centre if no marker), auto-picks a demod (WFM for 87.5-108 MHz, AM for 118-137 MHz,
   AM otherwise), and opens a 3.13 MHz IQ window. Enable **Audio** to hear it.
4. Inside SDR, click/drag to tune the listen frequency (smooth, DDC-only), wheel to zoom,
   and press **SDR** again to return to the sweep.
5. The **Ref** group (top control panel) is shared: in SDR it sets the display reference;
   **Auto** toggles the automatic amplitude scaling.

This avoids the CPU-heavy wide IQ stream for observation (the channelizer is CPU-bound
above ~3.13 MHz) while still allowing real demodulation and listening.

## Frontend

- `index.html`: `SDR` button + `#sdr-settings` panel. Modern layout: Listen field,
  **Demod** quick buttons (AM/FM/NFM/WFM/USB/LSB/CW), **Filter** width buttons,
  Volume/Squelch/AGC, **Band** presets (FM 88-108, Air 118-137, VHF 145, UHF 435),
  and the wideband Center + capture-bandwidth select.
- **Interaction (mouse and keyboard/trackpad)**:
  - spectrum: **left-click = tune** the listen frequency (green marker + passband),
    **drag = tune** continuously (edge-push shifts the capture centre when the cursor
    reaches the band edge, so panning continues beyond one window), **wheel/two-finger
    scroll = zoom** the capture span around the cursor (changes the decimate).
  - **Audio is OFF by default**: the `Audio` button (or `Space`) enables the WebAudio
    playback; a short fade-in/out avoids clicks. The preference is remembered.
  - **Amplitude**: auto reference by default (peak + 20 dB headroom); the shared `Ref`
    group sets a manual value and `Auto` restores auto-scaling.
  - **Sweep -> SDR handoff**: entering SDR demodulates the active marker (set by a click
    on the swept spectrum) or the SWP centre; the demod is auto-selected by band.
  - **Tuning is DDC-only**: `set_tune` reconfigures just the `DSP_DDC` offset and keeps
    the demod filters/AGC, so switching stations is click-free (no "noisy then clear").
  - keyboard (when focus is not in a text field): `←/→` tune ±1 kHz
    (Shift ×100, Alt ×10), `↑/↓` volume, `PgUp/PgDn` IF bandwidth, `M` cycle demod,
    `Space` audio on/off, `Z`/`X` zoom in/out. The canvas is focusable (`tabindex`).
  - the panel shows a shortcut hint line.
- **Audio transient**: after any chain reconfiguration (tune/bandwidth change) the
  backend mutes for ~70 ms, and the AGC settles faster, so a freshly tuned station no
  longer starts as a loud hiss.
- **Capture bandwidth vs CPU**: the channelizer (vendor `DSP_DDC`) cost scales with
  the IQ rate. Measured on this 8-core host: decimate ≥ 16 (≤ 3.13 MHz) sustains audio
  at realtime; decimate 8 ≈ 68 %, decimate 4 ≈ 35 %. The wide options are marked
  `(audio ⚠)` in the UI — use them for viewing, not for listening.
- **Auto-scale**: in SDR mode the amplitude reference is computed from each frame's
  peak (with hysteresis), because the SWP reference level (often 0 dBm) would push a
  −100 dBm noise floor off the bottom of the display. `#spectrum[data-sdr-ref]` carries
  the current value (debug aid).
- `src/audio/sdrAudio.ts`: WebAudio ring-buffer player (48 kHz, ScriptProcessor).
- `src/core/ws.ts`: routes `AUDF` to the player, `sdr` STATUS to the panel, auto-scale,
  and the listen marker state.
- `src/render/spectrum.ts`: draws the SDR listen marker + passband on the RTA canvas.
- `src/ui/controls.ts`: graph-mode handling for `sdr` (reuses the RTA rendering path),
  SDR panel actions, canvas interaction, keyboard shortcuts.

## Notes / limits

- The demod chain is numpy; DDC is the vendor C library (~5 % realtime).
- `-9 (BusDataError)` from `IQS_GetIQStream_PM1` is treated as a transient bad
  packet and skipped; a persistent streak triggers an in-place reconfigure.
- The publisher does not add its 2 ms sleep in SDR mode (the IQS fetch paces the
  loop), otherwise the device buffer overflows and every fetch returns `-9`.
- `WEBSA_SDR_ADM=0` disables the vendor AM/FM metric calls (useful for debugging).
- FT8 / WSPR / other digital modes are not implemented yet.

## Verification

- `sdr_chain_test.py` — live IQS→DDC→demod to WAV (AM/FM tone + panadapter level).
- `sdr_session_test.py` — direct `SdrSession` stability (packets ok/err, audio, ADM).
- `ws_sdr_test.py` — full WebSocket path (frames + audio WAV + STATUS).
- `ui_sdr_test.py` — Playwright: browser UI, live frames, ADM readout, no JS errors.
