Real antenna weak SSB NR, offline experiment (2026-11-03)

Start with the generated review.html and report.txt. This adds no production NR
algorithm and never configures or restarts the receiver. The user's pending AGC
changes are dependencies, not changes owned by this experiment.

Files in this directory:
  study.py       inspect/extract, replay, align, meter, plot and package this corpus
  omlsa.py       reusable 16 kHz OM-LSA + two-stage IMCRA experiment
  test_omlsa.py  numerical and causal-state checks
  requirements.txt  pinned numerical/plotting dependencies (Python 3.12 tested)
  ../../../backend/examples/offline_ssb_nr.rs  native replay example (see below)

Run from the repository root with Python 3.12+ and ffmpeg in PATH. Use the same
uncommitted native AGC revision as this study for equivalent results. Rust needs
the repository's toolchain/dependencies and embedded DeepFilterNet3 model.

  python3.12 -m venv /path/to/independent-experiment/venv
  /path/to/independent-experiment/venv/bin/pip install -r tools/bench/ssb_nr/requirements.txt
  /path/to/independent-experiment/venv/bin/python tools/bench/ssb_nr/test_omlsa.py
  /path/to/independent-experiment/venv/bin/python tools/bench/ssb_nr/study.py \
    --iq ~/Downloads/recording.sigmf \
    --audio ~/Downloads/audio.tar \
    --out /path/to/independent-experiment

The inputs are verified tar containers, not bare IQ/PCM. Only safe regular archive
members are extracted into OUT/input. SHA256 checks prevent reusing an experiment
directory with different original inputs. Original files are opened read-only.
No audio/IQ or third-party software should be committed to the repository.

To reuse existing results while changing plots/listening packaging:
  add --skip-native --skip-omlsa
Cache checks cover IQ/meta hashes and the OM-LSA source, configuration and input
hashes. To pick new excerpts supply --clip-plan plan.json, with this shape:
  {"clips":[["candidate",4,12,"weak speech candidate; unverified"]],
   "level_windows":[[38,44],[55.5,59],[95,103]]}
The default clip plan and narrative report target these supplied recordings.
study.py intentionally requires native cf32_le block-clock annotations, unit
amplitude_scale, continuous integer-stride IQ, and matching USB 2700 Hz audio
provenance. Other formats, discontinuities, modes and calibration factors must be
handled explicitly, not guessed. Reuse omlsa.py independently for other mono PCM.

To run just native replay:
  cargo run --release --manifest-path backend/Cargo.toml \
    --example offline_ssb_nr -- PATH/recording.sigmf-data \
    PATH/recording.sigmf-meta OUT/native
This reuses AnalogDemod::new_prechannelized (the native listening AGC) and
backend/src/nr.rs NoiseReduction. It forks the SAME AGC PCM into NR alternatives,
opens squelch for every branch, applies volume 2 AFTER NR, confirms no DFN
fallback, drains tails and removes the declared NR delay. It does not use the
legacy WASM/reference constructor. Sample rate is read from metadata. The example
fixes USB/2700 Hz/48 kHz output; do not borrow settings from the live receiver.
The native.json manifest includes input/meta and executable SHA256 hashes.

OM-LSA profiles:
  author18: paper/reference-parameter-oriented transplant with medium noise,
    tonal logic and hard low-probability decisions, nominal -18 dB GH0.
    GH0 is noise/smoothed-power dependent and can attenuate more than 18 dB.
    Correct float I/O, boundary WOLA and tail preservation intentionally differ
    from the public MATLAB WAV-header/int16 implementation. This is NOT a
    numerical conformance claim against MATLAB/Octave.
  ssb6 / ssb10: same IMCRA recurrences, occupied-band frame evidence, no automatic
    tone decision, no hard low-probability speech cut; all spectral gains bounded
    at -6 / -10 dB respectively. The bounds describe gains, NOT SNR improvement.
Both use a 32 ms window / 8 ms step; budget 32 ms of frame availability latency.
Each gain estimate uses current/past frames. Do not confuse aligned offline
output timestamps with zero processing latency. WOLA synthesis can depend on
future input up to its explicitly bounded framing delay.

The common demodulator/AGC output stays at 48 kHz for native DFN. OM-LSA uses a
48->16->48 kHz resampling bridge; bridge16_control_float.wav and the matched
listening/full/bridge16.wav isolate that operation. provenance.json reports
full-band and 270-2700 Hz reconstruction errors separately. Deleting resampling
images above the occupied band must not be called a noise/intelligibility gain.

Listen to listening/ PCM24 files (safe headroom), not the unmodified *_float.wav
mothers that intentionally preserve >1 peaks. Matching meters identical shared
speech-candidate intervals with ffmpeg BS.1770/R128, then applies one constant
gain to each whole track. It never normalizes individual pauses or weak syllables.
Default blind options: none, production DFN13, author18, conservative SSB10.
Additional DFN10/20, SSB6, bridge and original user DFN are in listening/full/.
Stereo pairs have left/right identities in the filenames. review.html randomizes
letter identities independently for each clip. Record callsigns/digits before
opening blind_key.json. listening_scores_template.csv is regenerated; an existing
listening_scores.csv is preserved, so reruns retain your annotations.

Interpretation limits:
  - Representative labels are signal-structure candidates, not heard transcripts.
  - User DFN audio has continuous, matching source clocks and two NR settings.
    Alignment uses rational source time and declared delays, checked with six
    waveform windows; no time warping or invented zero-filled missing tail.
  - Initial AGC/filter/resampler/neural history differs: matching audio is NOT
    claimed bit-identical. The beginning is not assumed to be pure noise.
  - No clean reference or confirmed transcript exists. Do not compute SNR,
    SI-SDR, PESQ/STOI against DFN; lower RMS/noise and pretty spectrograms are
    diagnostics, not intelligibility scores.
  - Artificial colored-noise tests verify estimation and timing only.
  - The public author's MATLAB says "All rights reserved". It is downloaded
    separately for this private research, not vendored or integrated into runtime.
    Resolve implementation licensing before any production integration.

Original sources:
  https://israelcohen.com/software/
  https://israelcohen.com/wp-content/uploads/2020/11/omlsa.zip
  https://israelcohen.com/wp-content/uploads/2018/05/sp_Nov2001.pdf
  https://israelcohen.com/wp-content/uploads/2018/05/SAP_Sep2003.pdf
Commercial equipment notes and direct manufacturer sources are in report.txt.

DFN speech protection and selective enhancement (follow-up listening experiment):
  OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 nice -n 10 PYTHON \
    tools/bench/ssb_nr/test_speech_focus.py
  OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 nice -n 10 PYTHON \
    tools/bench/ssb_nr/run_speech_focus.py --study EXISTING_STUDY --out NEW_DIR
Use the same numerical environment above. This reuses the previous native DFN
10/13/20 results and conservative IMCRA diagnostics. It first inverts the known
DFN attenuation mixing rule and must independently reproduce DFN13 within -100 dB
relative reconstruction error. No stem is a clean reference or new trained model.
speech_focus.py combines neural/statistical speech evidence to preserve residual
weak speech while reducing low-evidence residual noise; a second variant adds
bounded, selective presence emphasis. DFN13, DFN20 and fixed-EQ controls distinguish
changes due to stronger attenuation or tone shaping. A fresh output directory is
required so that listening annotations are preserved. The runner has no live
service requests and introduces no production NR integration.

Echo-like coloration follow-up (one parameter at a time):
  OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 nice -n 10 PYTHON \
    tools/bench/ssb_nr/run_speech_naturalness.py --study EXISTING_STUDY \
    --previous EXISTING_STUDY/speech-focus-v1 --out NEW_DIR
This requires a fresh directory. It verifies original v1 speech_guard float32
output bit for bit, retains DFN20, and changes only release 80->24 ms or output
gain smoothing sigma 0->60 Hz. The combination is a supplementary named track.
Diagnostic impulse responses refer only to the frozen original-signal residual
gain, not the whole nonlinear model or a physical room reverberation measurement.
Seven numerical checks cover alignment, constant mixing, silence, future support,
frequency smoothing and envelope release. Listening must establish whether the
echo-like impression actually improves without loss of weak consonants.

Official GTCRN comparison (independent CPU environment):
  git clone https://github.com/Xiaobin-Rong/gtcrn.git /path/to/gtcrn
  git -C /path/to/gtcrn checkout 502ebfab64da7c4a9af78dcb9c6ceef1ebb01c73
  python3.12 -m venv /path/to/gtcrn-env
  /path/to/gtcrn-env/bin/pip install -r tools/bench/ssb_nr/requirements_gtcrn.txt
  OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 MKL_NUM_THREADS=1 nice -n 10 \
    /path/to/gtcrn-env/bin/python tools/bench/ssb_nr/test_gtcrn_offline.py \
    --repo /path/to/gtcrn
  OPENBLAS_NUM_THREADS=1 OMP_NUM_THREADS=1 MKL_NUM_THREADS=1 nice -n 10 \
    /path/to/gtcrn-env/bin/python tools/bench/ssb_nr/run_gtcrn_comparison.py \
    --study EXISTING_STUDY --guard-study EXISTING_STUDY/speech-naturalness-v2 \
    --gtcrn-repo /path/to/gtcrn --out NEW_DIR
The default root DNS3, VCTK, and official streaming ONNX DNS3 are three distinct
models. In this commit the DNS3 root checkpoint is epoch 87 and the ONNX companion
checkpoint is epoch 96: their weights are different. Check ONNX against its paired
checkpoint; do not mistake a weight-version difference for a streaming bug.
gtcrn_offline.py imports the unmodified upstream GTCRN class. Modern complex-tensor
I/O and explicit ISTFT length preserve the waveform tail; model layers/weights,
sqrt-periodic-Hann STFT and input amplitude remain as in the official reference.
It receives the SAME pre-volume native AGC PCM, resampled to the required 16 kHz,
and applies volume 2 after inference. Whole-recording model state is retained;
clips are cut afterward. A separate bridge control isolates sample-rate effects.
The official ONNX model is processed frame by frame with caches preserved, using
offline centered STFT/ISTFT. Graph parity does not certify live PCM scheduling,
device clocks, startup boundaries or Rust production integration. Budget its
32 ms analysis window / 16 ms hop when evaluating future live availability.
Six official-weight/timing tests cover sample positions and waveform tail,
old/new feature representation, future support, paired streaming parity and
resampling offset. Main listening options use raw official outputs. Explicit
*_mix20 controls in full/ add 20 dB residual mixing only as labelled supplements.
Use --reuse-inference only for an incomplete output directory; model/input/
adapter/output hashes are verified. A completed experiment or score table is
never overwritten. No receiver API requests or production NR changes are made.
