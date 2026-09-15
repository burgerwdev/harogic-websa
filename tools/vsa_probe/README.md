# tools/vsa_probe — VSA feasibility probes

Feasibility evidence for a vector-signal-analysis (VSA) mode on the SAN-90.
These scripts **measure and report**; they are not production code and nothing
here is imported by `web_sa/`.

## Device exclusivity (mandatory)

`libhtraapi` is not thread-safe and only one process may hold the USB handle.
`harness.exclusive_websa()` stops the WebSA supervisor + worker before any
hardware probe and restores them (`./run.sh`) afterwards — also when the probe
raises. Never run two hardware probes at once; never leave one running.

```bash
./stop.sh          # what the harness does for you
./run.sh           # ... and what it does afterwards
```

Offline probes (`probe_siggen_selfcheck.py`, `probe_dsp_loopback.py`) need no
device and no service state.

## Files

| File | Role |
|---|---|
| `harness.py` | exclusive-device harness, `Report` (prints + JSON), tinySA driver import |
| `siggen.py` | synthetic IQ source: constellations, RRC, AWGN, CFO, timing offset |
| `capture.py` | IQS capture primitives (FixedPoints frame, Adaptive stream, volts conversion) |
| `analysis.py` | spectral helpers (coherent tone level, band power, noise floor) |
| `measure.py` | Tier1 vector measurements (power-time, CCDF, spectrum, constellation) |
| `demod.py` | Tier2 in-house PSK/QAM demodulation chain + EVM |
| `probe_*.py` | the probes themselves; each prints a summary and writes JSON |

## Probes

```bash
# offline: is the synthetic reference trustworthy?
python3 tools/vsa_probe/probe_siggen_selfcheck.py

# offline: do the Tier1/Tier2 DSP chains meet their theoretical EVM floor?
python3 tools/vsa_probe/probe_dsp_loopback.py

# hardware: can we capture a frame / stream continuously? (tinySA CW source)
python3 tools/vsa_probe/probe_iq_capture.py framed --decimate 8 --trigger-length 65536 --repeat 5
python3 tools/vsa_probe/probe_iq_capture.py depth  --decimates 4,16,64
python3 tools/vsa_probe/probe_iq_capture.py stream --decimates 4,16,64 --seconds 5
python3 tools/vsa_probe/probe_iq_capture.py no-trigger
python3 tools/vsa_probe/probe_iq_capture.py all

# hardware: level calibration against the vendor swept path, DC/image behaviour
python3 tools/vsa_probe/probe_iq_level.py level --levels -30,-25,-20,-15
python3 tools/vsa_probe/probe_iq_level.py dcimage
```

Every probe takes `--json PATH` (default: next to the script) and writes the
numbers it printed plus the bench conditions. Those `*.json` files are local run
artifacts — like `tools/sdr_probe/`, only the scripts and the `.md` files are
tracked, so re-run a probe to regenerate its JSON. `--tinysa-port` overrides
`/dev/ttyACM0`.

## Known-signal sources

* **tinySA Ultra ZS407** — CW / AM / FM only. Used for real-path level, DC,
  image and capture-timing measurements.
* **Synthetic IQ** (`siggen.py`) — the only way to produce PSK/QAM. Tier2
  demodulation claims are validated on synthetic waveforms with injected timing,
  carrier and AWGN impairments, never on impossible hardware signals.

Results and their bench conditions are collected in `FINDINGS.md`.
