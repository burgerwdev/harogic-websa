# Receiver workflow

[Documentation](INDEX.md)

## Spectrum and capture

Use SWP for swept measurements and RTA for real-time spectrum/density. SDR adds native
receivers, audio and decoders. Center and span configure acquisition; display zoom and the
Top/Bottom controls change the view. Display zoom does not improve the captured resolution.
Ref Level, attenuation and preamp control hardware range; Auto Scale is a one-shot action.

In SDR, Center moves shared capture and Current together. IQ bandwidth changes shared capture
while preserving receiver tuning. To Center moves capture to the selected receiver. Moving
capture can interrupt all receivers and create recording boundaries.

## Channels and listening

Channels opens the shared workbench. Current permanently reserves one of sixteen slots,
leaving up to fifteen saved channels. Saved definitions retain their slots while in band,
out of band or manually stopped. Only deleting a saved channel frees its slot; closing a
window, stopping monitoring or hiding a card does not. Reception requires the full passband
and transition to fit the actual capture. Choose a mode and frequency, inspect the preview,
and apply. Closing a window does not stop its receiver or recording.

The grid button beside Channels opens the single all-channel overview. Overview in any
channel window turns that window into the overview and restores the previous overview's
editor. Cards show frequency, mode, bandwidth, creation time and live level; listening uses
an icon button. Sort by creation time, frequency, name, type or activity; the arrow reverses
time, frequency, name and type order. Chinese names use pinyin collation. The count shows
defined channels against service capacity, including channels in separate windows.
Click a card name to edit it; Enter or leaving the field saves, and Escape cancels.
Cards matching the main spectrum frequency show a crosshair and highlighted border,
independently of the workbench's editing selection.

An audio card's output button cycles **L/R → L → R** for the local speaker mix.
**Mute all** in the overview title bar toggles master mute, including channels
in separate windows; clicking again restores their existing listening routes
and volumes. These controls do not change reception or recordings.

Choose an automatic grid, 1–8 columns or free placement. Drag cards to reorder or position
them; a moving placeholder previews the drop position. Alt+arrow keys also move a focused
card. With snapping enabled, dragging reveals a grid of card-sized cells and the placeholder
previews the nearest empty cell. Enabling snapping aligns existing cards too. With snapping
disabled, cards move freely by the pixel on a canvas without grid lines.
Fit sizes the window to its cards; overflowing layouts scroll without shrinking the controls.
Double-click a card or use its separate-window button to take it out. Dock returns that
channel to the overview; Collect all returns every independent channel window. The overview
itself can move to a separate browser window and return to the page.

Visible cards show the existing playback PCM waveform at ten frames per second, before
local monitor volume. The waveform is normalized for visibility; the dBFS meter retains its
absolute scale. Unmonitored channels retain status text, and decoded text keeps its ticker.
Decode and overview subscribe to existing receivers. Current and a saved channel at the same
frequency run separately. Digital status is independent of audio squelch. Raw fields and
diagnostic JSON have a Copy button.
Channels sharing one playback route share its waveform and level readouts. Hidden cards
release their waveform references; opening the overview creates no audio subscriptions.

Current is a fixed temporary tuning source. Main tuning always controls it; editing or
monitoring a saved channel never changes this role. In both the overview and workbench,
Current stays first when no saved channel has its frequency. When one or more saved channels
match, the Current entry is hidden and every matching saved channel is highlighted with a
crosshair. This merges presentation while preserving reception, monitoring and capacity.
Editing selection is separate from the frequency indication. Current has a fixed name and
can be stopped or saved, but cannot be deleted. The main plus button saves an independent
copy; subsequent tuning leaves the saved frequency and settings intact. Repeating the same
save locates an existing copy. Different modes, bandwidths or processing settings at the
same frequency can be saved independently. Use Duplicate channel
to create another identical channel. Existing duplicates remain independent until explicitly
removed; the monitor layer continues to prevent identical audio from playing twice.

Capture center or IQ bandwidth changes pause enabled channels whose full passband and
transition no longer fit. They retain all settings and show Out of band · waiting for coverage.
Full coverage automatically restores reception and the previous local monitor route; manually
stopped channels stay stopped. Display-only zoom and pan do not change reception. Both views
keep out-of-band channels by default and offer a filter for the current band. Waveform and level
updates pause while decoded history remains. Frequency highlighting can coexist with an
out-of-band state.

Main tuning always controls Current. Workbench editor selection is local to that view. Monitor volume, pan, mute and Solo
are local speaker settings. Backend gain, AGC, squelch and noise reduction affect shared PCM
and therefore published-audio recordings. A stopped or failed receiver cannot be monitored.

The **EQ** button beside De-emph opens a movable listening equalizer. Five bands
(80 Hz, 250 Hz, 1 kHz, 4 kHz and 12 kHz) provide ±12 dB, with output trim, bypass
and Flat/Voice/Warm/Bright profiles. It applies to the overall stereo speaker mix
in this browser. Settings persist locally; Preset restores enabled, flat bands
and 0 dB trim. Recording, demodulation and decoder input remain before this EQ.

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

Planned out-of-band gaps finish the current segment and keep the task waiting. Full coverage resumes a fresh segment with interruption timestamps; stopped tasks never resume automatically. Unexpected device or processing faults retain their failure handling.

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
