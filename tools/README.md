# tools

Bench, fixture and gate scripts. Each directory has one purpose.

| Directory | What it holds |
| --- | --- |
| `checks/` | Static gates that CI runs. No hardware. |
| `fixtures/` | Generators for the committed fixtures and tables. CI runs `--check`. |
| `pluto/` | Per-mode transmit generators for the PlutoSDR. Use `tools/pluto_tx.py`, not these files. |
| `bench/` | Tools that need the SAN-90, the PlutoSDR or a running service. |
| `analysis/` | Offline numeric and reference checks. No hardware. |
| `e2e/` | Browser end-to-end probes (Playwright). |
| `sdr_probe/` | Low-level SAN-90 probing notes and scripts. |
| `vendor/` | Third-party sources (ggmorse) and their build script. |

## Transmit a test signal

`tools/pluto_tx.py` is the one entry point. It lists and dispatches every mode.

    python3 tools/pluto_tx.py --list          # the modes and their tool
    python3 tools/pluto_tx.py --dry-run-all   # build every mode, no radio
    python3 tools/pluto_tx.py nfm --lo 411e6 --gain -10
    python3 tools/pluto_tx.py usb --wav tools/bench/audio/speech.wav --seconds 60
    python3 tools/pluto_tx.py drm --iq drm_iq.wav --seconds 600

FM is `nfm` (3 kHz deviation) and `wfm` (75 kHz). `--wav FILE` sends a real audio
file instead of a tone, which is the easy way to judge the decoded audio. It works
for am/dsb/usb/lsb/nfm/wfm/pm, reads 8/16/24/32-bit PCM, defaults to the whole file,
and is peak-normalised. `--voice` adds the real-transmitter SSB chain (300-2700 Hz
band-pass + peak compression) for USB/LSB voice tests.
`tools/bench/audio/speech.wav` is the committed voice sample (22050 Hz mono, 11.8 s):
it is the default audio for the USB/LSB voice tests.

The per-mode generators live in `tools/pluto/`. Run one directly only when the
wrapper cannot do what you need.

## Common gates

    python3 tools/checks/check_docs_parity.py
    python3 tools/fixtures/gen_dsp_fixtures.py --check
    make ci
