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
    python3 tools/pluto_tx.py wfm --rds-rt 'CQ DE N0CALL, bench RDS test' --rds-pi 0x1234
    python3 tools/pluto_tx.py usb --wav tools/bench/audio/speech.wav --seconds 60
    python3 tools/pluto_tx.py drm --iq drm_iq.wav --seconds 600
    python3 tools/pluto_tx.py sstv --mode 'Robot 36' --callsign 'CQ WE0BSA'
    python3 tools/pluto_tx.py sstv --image poster.png --repeat 3

SSTV sends a picture as single-sideband tones: `--mode` picks one of the 18 modes the receiver can
decode (`--list-modes` prints them with their lengths), `--image FILE` sends a PNG or PPM file
(resized to the mode's resolution) instead of the generated test pattern, and `--callsign` is drawn
into that pattern. A transmission is 36 s (Robot 36) to 406 s (Pasokon P7), so the baseband is
synthesized in chunks and pushed as it is built rather than being handed to the driver whole.

FM is `nfm` (3 kHz deviation) and `wfm` (75 kHz). `--wav FILE` sends a real audio
file instead of a tone, which is the easy way to judge the decoded audio. It works
for am/dsb/usb/lsb/nfm/wfm/pm, reads 8/16/24/32-bit PCM, defaults to the whole file,
and is peak-normalised. `--voice` adds the real-transmitter SSB chain (300-2700 Hz
band-pass + peak compression) for USB/LSB voice tests.
`tools/bench/audio/speech.wav` is the committed voice sample (22050 Hz mono, 11.8 s):
it is the default audio for the USB/LSB voice tests.

`wfm --rds-rt TEXT` makes the transmission a broadcast station: RDS Group 2A
RadioText, a Group 0A PS name and an optional Group 4A clock group, in biphase BPSK
on the 57 kHz subcarrier of the composite. `--rds-ps`, `--rds-pi`, `--rds-pty` and
`--rds-deviation-hz` set the name, the code, the type and the injection (2 kHz by
default). Repeat `--rds-rt`, or separate the texts with commas, to rotate messages;
each stays up for whole passes, at least two, because a receiver wants the repetition
of a segment. Repeat `--rds-ps` to rotate the station name as well, which a real
station does not do. `tools/pluto/rds_encode.py` is the encoder; Open Data, EON and
the alternative frequencies are not implemented. RDS is wfm-only, because 57 kHz does
not fit a narrowband FM channel.

A pass is the 16 RadioText segments as one burst of 1.4 s, then four PS groups, so a
pass takes 1.75 s. The burst lets a receiver collect the whole text before a marginal
signal drops a segment. The 19 kHz pilot belongs to the transmission
(`--pilot-deviation-hz`, 0 to leave it out): EN 50067 locks the subcarrier to its
third harmonic, so a radio that recovers its RDS clock from the pilot needs one. The
pilot, the data and the audio share the deviation budget, so the composite is scaled
back to `--deviation-hz` peak. `--no-audio` sends the pilot and RDS alone, the best
RDS test signal, because a tone at full deviation spends the modulation that the
radio has to carry on an empty 1 kHz. The tool encodes the transmission as it goes;
`--rds-buffer` builds one rotation and lets the device repeat it instead, which cannot
gap, for a host that is slow to feed the link.

The per-mode generators live in `tools/pluto/`. Run one directly only when the
wrapper cannot do what you need.

## Common gates

    python3 tools/checks/check_docs_parity.py
    python3 tools/fixtures/gen_dsp_fixtures.py --check
    make ci
