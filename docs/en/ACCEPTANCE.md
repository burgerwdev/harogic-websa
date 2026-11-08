# 1.12.15 acceptance — 2026-11-08

## Changes and regression

- External decoder admission, recovery and retirement share PreparedDecoders across multimon, rtl_433, OnAir, RTTY and GNSS. Failed preparation stays local; unaffected receivers continue.
- Pipe input retains at most 32 MiB and 1,024 blocks, including execution. Fine and processor byte budgets use two seconds at the actual rate/format, clamped to 4–64 MiB. Processor input also has a two-second media limit. Both queues allow 1,024 entries; fine input includes one executing block.
- Digital reception is independent of companion audio squelch. Modes without an analog listener report `squelch:null`; fault, recovery and backlog take priority over waiting status.
- Decode and overview subscribe to existing receivers. Enabled receivers drive reception without an open view. Current and a saved receiver at the same frequency own separate processing and external decoder processes.
- Wide-mode default transition is max(1 kHz, occupied bandwidth / 8): 225 kHz for ADS-B. Existing explicit configurations remain explicit.
- Diagnostic/raw JSON, stream/IQ delivery and recording source/file details have Copy buttons, including plain-HTTP clipboard fallback.
- Offline decoding accepts pipe stdin. Generated FT4/WSPR fixtures can explicitly schedule complete host-UTC slots; ordinary recording replay remains rejected.

## Measured acceptance

| Input/check | Result |
|---|---|
| RTTY / reverse | Exact ITA2 text, numbers, punctuation and final newline; 935,604 complex samples per polarity at 48 kS/s. |
| APRS | Three generated AX.25 frames: N0CALL-1 to APRS, WEBSA APRS TEST, 49.05833°, −72.02917°. |
| FT4 / WSPR | Generated complete UTC slots decoded CQ K1ABC FN42 / K1ABC FN42 30. |
| GPS | Synthetic 90-second scenario, 234,000,000 complex input samples at 2.6 MS/s; 196 valid PVT snapshots and 132 navigation rows. |
| Browser/audio | Clipboard copies verified; −6 dB EQ amplitude ratio approximately 0.50; full right pan; zero PCM under master mute; bypass/unmute restoration; no local-control configuration writes. |
| Channel lifecycle | Three concurrent native channels, 16 mode changes, four delete/recreate cycles; healthy neighbour configuration/stream preserved. |
| Reception without views | HTTP-only fixture regression creates a second receiver; both process and advance without browser or output subscriptions. |
| Dual hardware ADS-B | SAN-90, original Current and saved 1090 MHz settings, 7.8125 MS/s; 304 seconds during CI, backlog peaked at 0.87 s and drained, both active at completion, zero interruptions/faults, unchanged stream identities and advancing clocks. |

Known-payload tests qualify integration. Sustained sixteen-channel RF capacity, sensitivity and passive GPS sky reception remain unqualified. No RF transmission was performed.

## Reproduction and artifacts

[Tools](../../tools/README.md) lists installed-provider and browser commands. The
[digital manifest](../../tests/fixtures/digital/manifest.json) records recipes, hashes and decoded evidence.
Large IQ files and local reports remain outside Git. Hardware-free CI skips tests explicitly marked as requiring installed providers or SDK hardware.

## Candidate and remaining distribution evidence

`tools/package-candidate.py OUTPUT --verify` stages a candidate outside the checkout and checks isolated startup, version, license page, matching source download and running binary identity. Generated source archives are excluded from Git. A public source offer may use an exact matching release commit/tag with the complete build inputs.

[Licensing](LICENSING.md#follow-up-evidence-on-2026-11-08) records unresolved SDK agreement, original codec archive source/toolchain, CTY permission and model-specific grant. Local codec rebuild hashes differ from committed archives. Candidate validation does not resolve those provenance gaps.
