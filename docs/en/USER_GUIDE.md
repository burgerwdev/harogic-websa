# Receiver workflow

[Documentation](INDEX.md)

## Spectrum and capture

Use SWP for swept measurements and RTA for real-time spectrum/density. SDR adds native
receivers, audio and decoders. Center and span configure acquisition; display zoom and the
Top/Bottom controls change the view. Display zoom does not improve the captured resolution.
Ref Level, attenuation and preamp control hardware range; Auto Scale is a one-shot action.

In SDR, Center moves shared capture and C1 together. IQ bandwidth changes shared capture
while preserving receiver tuning. To Center moves capture to the selected receiver. Moving
capture can interrupt all receivers and create recording boundaries.

## Channels and listening

Channels opens the shared workbench. Up to sixteen receiver definitions share one capture;
disabled definitions keep their slots, and deleting a definition frees its slot. Complete
passbands and transitions must fit capture. Choose a mode and frequency, inspect the preview,
and apply. Closing a window does not stop its receiver or recording.

The channel beside main Listen selects the receiver controlled by main tuning, mode and audio
settings. Workbench editor selection is local to that view. Monitor volume, pan, mute and Solo
are local speaker settings. Backend gain, AGC, squelch and noise reduction affect shared PCM
and therefore published-audio recordings. A stopped or failed receiver cannot be monitored.

For analog demodulation, the main panel offers 12, 24 and 48 kHz audio-rate shortcuts; 48 kHz is the default restored by Preset.
These buttons change only the selected channel's sample rate. Other rates set elsewhere remain visible as their actual value. Mode-specific settings remain in the Decode window.

Double-click a collapsed channel name to rename it; Enter submits, Escape or leaving the field cancels. F2/Enter on the focused name also starts editing. The playback meter follows samples consumed by the audio output, with RMS, sample peak, a fixed −60…0 dBFS scale and one-second peak hold. It is measured before local monitor gain/pan/master. Muting, pausing or stale playback clears it. Unmonitored demodulator telemetry is explicitly labeled as not playing.

The RF measurement `level_dbfs` is normalized complex-IQ power before audio processing;
`channel_dbm` is a tuned FFT-bin peak. Neither measures the loudness of demodulated audio.
Audio meters must use playback PCM and an explicit dBFS scale; a digital sample-peak meter
is not a calibrated RF power meter, a true-peak meter or a broadcast loudness compliance meter.

## Decode and presets

Use the mode catalog's availability and reason fields to identify missing providers. Optional
providers are installed separately using the [tools guide](../../tools/README.md). Availability
means the provider is installed, not that the signal has locked. Decoder windows can be
collapsed or popped out while keeping the same receiver session.

Named presets save confirmed channel processing and baseband settings. Loading applies a
preview to an existing target and retains its identity and local monitoring. It does not restore
an entire scene or resume recordings. Session mode memory restores editor candidates;
application still requires confirmation.

## Record and replay

Choose IQ, published audio or decoded results. These share sixteen service-wide task slots;
recording multiple kinds on one receiver uses multiple slots. Tasks continue when the browser
closes. Stop and wait for finalization before playback, export, rename or delete.

New audio recordings default to Opus. WAV preserves published Float32 PCM; FLAC stores
quantized 24-bit PCM; Opus is lossy. Format is fixed when a task starts. Browser monitor
volume/pan/mute/Solo are excluded from recordings. File playback is independent of live listening.

Use the recording library to filter by names/frequencies, inspect segments, download and
manage finished tasks. Source changes and gaps remain distinct segments. Range playback
and offline AM/USB/LSB/NFM rendering are bounded, not continuous whole-archive playback.
See [Recording reference](reference/RECORDINGS.md) for exact limits and formats, and [Known limitations](OPERATIONS.md)
for decoder retention and qualification boundaries.
