# External digital protocol fixtures

The manifest records deterministic generator recipes, IQ hashes where retained,
input sample counts and observed protocol payloads from production Receiver
acceptance on 2026-11-08. It covers both RTTY polarities, APRS, FT4, WSPR and GPS.

Run tools/checks/digital_decode_acceptance.py from the repository root after
building sdr_decode and installing the independent providers/encoders. Use its
--encoder and --gps-source options for existing installations. The runner saves
generated IQ and complete receiver JSON under --artifacts; GPS streams its
90-second scenario to avoid storing gigabytes of IQ.

RTTY uses the independent ITA2 generator in tools/fixtures/rtty_iq.py.
FT4/APRS/WSPR use the pinned standalone encoder installed by
tools/pluto/build-digital-source.sh; its binary hash is in the manifest.
FT4/WSPR are generated complete slots rescheduled to host UTC, with explicit
preroll metadata. This does not restore historical recording UTC.
GPS uses the pinned MIT gps-sdr-sim/RINEX fixture, whose installer manifest and
source hashes are retained. No RF hardware is opened by this gate.

The manifest's observed fields are acceptance evidence, never decoder input.
Tests require actual protocol payloads or a known-position navigation solution;
status messages and empty tables do not count as decoded content.
