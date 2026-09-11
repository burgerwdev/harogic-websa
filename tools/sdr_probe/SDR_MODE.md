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

## Frontend

- `index.html`: `SDR` button + `#sdr-settings` panel (center, bandwidth, listen,
  demod, IF BW, volume, squelch, AGC, ADM readout).
- `src/audio/sdrAudio.ts`: WebAudio ring-buffer player (48 kHz, ScriptProcessor).
- `src/core/ws.ts`: routes `AUDF` to the player and `sdr` STATUS to the panel.
- `src/ui/controls.ts`: graph-mode handling extended to `sdr` (reuses the RTA
  rendering path); SDR panel actions.

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
