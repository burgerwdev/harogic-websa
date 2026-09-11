# SAN-90 SDR feasibility — bench findings

All results below were measured on the live hardware (SAN-90 Model 67, API 0.55.89,
tinySA Ultra+ ZS407 as the known signal source). WebSA was stopped for every
exclusive run. Probe scripts: this directory.

## 1. Device capabilities (`probe_iqs_caps.py`, `Device_GetHardwareState`)

| Field | Value |
|---|---|
| Model | 67 (SAN-90), 9 kHz – 9 GHz |
| GNSS | present (`GNSSType=1`) |
| `SignalSourceEn` | **0** (no internal RF source) |
| `ADC_VariableRateEn` | **0** (fixed ADC rate) |
| Native IQ | **62.5 MSPS complex, 50 MHz bandwidth** (DecimateFactor = 1) |

## 2. IQS sample rates (`probe_iqs_caps.py`)

`DecimateFactor` is **power-of-two only**; > 2048 clamps to 2048.

| Decimate | IQSampleRate | Bandwidth |
|---|---|---|
| 1 | 62.5 MSPS | 50 MHz |
| 2 | 31.25 MSPS | 25 MHz |
| 4 | 15.625 MSPS | 12.5 MHz |
| 8 | 7.8125 MSPS | 6.25 MHz |
| 16 | 3.90625 MSPS | 3.125 MHz |
| 32 | 1.953125 MSPS | 1.5625 MHz |
| 64 | 976.5625 kSPS | 781.25 kHz |
| 256 | 244.14 kSPS | 195.31 kHz |
| 1024 | 61.04 kSPS | 48.83 kHz |
| 2048 | 30.52 kSPS | 24.41 kHz |

`DataFormat` (8/16/32-bit) changes the per-packet sample count; the packet byte
size stays ~64960.

## 3. Continuous streaming (`probe_stream.py`)

**Adaptive** trigger mode gives true gapless streaming at the full IQ rate:

| Decimate | Rate | USB | gaps |
|---|---|---|---|
| 2 | 31.240 MSPS | 125 MB/s | none |
| 4 | 15.620 MSPS | 62 MB/s | none |
| 8 | 7.810 MSPS | 31 MB/s | none |
| 16 | 3.905 MSPS | 16 MB/s | none |
| 64 | 0.976 MSPS | 3.9 MB/s | none |

`FixedPoints` (trigger per frame) is **not** continuous.

## 4. DSP_DDC (`probe_ddc.py`, `probe_ddc_sweep.py`, `probe_ddc_diag.py`)

Verified with a +200 kHz CW tone (SAN center 100 MHz, input 3.90625 MSPS):

- **Output format is interleaved complex float32** (`DataFormat == 6`),
  `PacketDataSize = N*8`. Vendor `DSP_FFT` does **not** work on it (all-zero);
  NumPy FFT is correct.
- **Offset sign:** `f_out = f_in_rel + DDCOffsetFrequency`. To bring absolute
  frequency `ft` to DC set `DDCOffsetFrequency = center - ft` (tone at +200 kHz
  → offset `-200e3`).
- Proper low-pass decimator: passband `±(SampleRate/DecimateFactor)/2`. Verified
  offsets `-220k … -180k` pass, others rejected at dec=100.
- `SampleRate` input field = **input** IQ rate; `ProfileOut.SampleRate` = output
  rate. `ProfileOut.SamplePoints = ceil(input/decimate)`.
- `DSP_DDC_GetDelay`: 0 (dec 1), 60 (dec 8), 73 (dec 100/1000) output samples.
- Cost ≈ 1.6–2.2 ms per 131072 input samples (~5–7 % realtime), independent of
  decimation.

## 5. Vendor DSP_FFT (`probe_vendorfft.py`)

- Works on int16 IQS data when the stream metadata is intact (official pattern).
- A −25 dBm CW at 100.200 MHz → peak at 100200019.8 Hz, −25.16 dB, axis
  `center ± IQSampleRate/2`, points = FFT size.

## 6. ADM analog demod (`probe_adm.py`)

tinySA AM 1 kHz/50 %, FM 1 kHz/25 kHz dev, SAN center 100 MHz, 1.953 MSPS:

| Mode | Result |
|---|---|
| AM `ADM_AMDemod_PM1` | ModRate 994.6 Hz, waveform tone 998.4 Hz, Carrier −19.7 dBm, SINAD 22 dB, SNR 23 dB |
| FM `ADM_FMDemod_PM1` | ModRate 998.3 Hz, Deviation 22331 Hz, CarrierErr 21 Hz, SINAD 7 dB |
| DDC float → FM ADM | works (`DataFormat=Complexfloat`), ModRate 998 Hz |

Note: `ModDepth` is returned as a **fraction** (0.49 for 50 %), not percent.
`Deviation` is ~11 % below the tinySA setting.

## 7. DCC / QDC receive-path comparison

Using the connected antenna and the 101.7 MHz broadcast signal at 1.953125 MSPS:

- `DCCOff`, `DCCHighPassFilterMode`, and `DCCAutoOffsetMode` differed by less than
  0.1 dB in integrated power over the centre +/-100 kHz.
- Moving the station to +200 kHz showed no measurable image-rejection improvement from
  `QDCAutoMode` on this weak/noisy signal (about 4.2 dB apparent desired/image ratio in
  every mode), so that capture cannot justify changing the production default.
- IQS configuration time remained about 5 ms for every DCC/QDC combination.

Keep the bench-proven `DCCHighPassFilterMode + QDCOff` default until a strong off-centre
CW source is physically connected for a meaningful image-rejection measurement.

## 8. Not available

- Digital `Demod_*` API (PSK/QAM/FSK/ASK/GMSK): `Demod_Check() == -1` — missing
  `libDigitalSigDemod.so` + license. Must be implemented in-house if required.
- Internal signal source: none.

## 9. Implication for the SDR mode

The whole chain is feasible and cheap:
`IQS Adaptive stream → DSP_DDC channelizer (float) → [ADM AM/FM | own CW/SSB/FT8/WSPR
in NumPy] → audio + panadapter/waterfall from a NumPy FFT of the wideband IQ`.
A decimate of 16–64 (3.9–1 MSPS) is plenty for a live panadapter plus narrowband
demod, and DDC+RTTY/FT8 DSP cost is a few percent of realtime.
