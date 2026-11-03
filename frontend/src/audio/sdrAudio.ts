// Backend AUD2 playback: each channel has one ingress worker and one output node.
let ctx: AudioContext | null = null;
let spectrumProbe: AnalyserNode | null = null;
let spectrumProbeBin = 1;
let modulePromise: Promise<void> | null = null;
const WORKLET_URL = new URL('./sdrAudioWorklet.js', import.meta.url).href;
const chains = new Map<number | string, AudioChain>();
let nativeOwner = false;
const legacyWanted = new Map<number, boolean>();
let master: GainNode | null = null;
let limiter: DynamicsCompressorNode | null = null;
let masterVolume = 0.7;
let masterMuted = false;
function ramp(param: AudioParam, value: number): void {
  const now = ctx!.currentTime;
  param.cancelScheduledValues(now); param.setValueAtTime(param.value, now);
  param.linearRampToValueAtTime(value, now + 0.015);
}
function nativeBus(audio: AudioContext): GainNode {
  if (!master) {
    master = audio.createGain(); limiter = audio.createDynamicsCompressor();
    limiter.threshold.value = -3; limiter.knee.value = 0; limiter.ratio.value = 20;
    limiter.attack.value = 0.003; limiter.release.value = 0.1;
    master.connect(limiter); limiter.connect(audio.destination);
    master.gain.value = masterMuted ? 0 : masterVolume;
  }
  return master;
}

/** True while autoplay policy holds the context suspended with playback wanted. */
export function sdrAudioNeedsGesture(): boolean {
  return ctx?.state === 'suspended'
    && Array.from(chains.values()).some((playback) => playback.wantsPlayback());
}
let gesturePending: boolean | null = null;
function notifyGestureState(): void {
  const pending = sdrAudioNeedsGesture();
  if (pending === gesturePending) return;
  gesturePending = pending;
  document.dispatchEvent(new CustomEvent('websa:sdr-audio-gesture', { detail: { pending } }));
}

function context(): AudioContext {
  if (!ctx) {
    const requestedRate = new URLSearchParams(location.search).get('audioRate');
    ctx = requestedRate === '44100' || requestedRate === '48000'
      ? new AudioContext({ sampleRate: Number(requestedRate) }) : new AudioContext();
    const resume = () => {
      for (const playback of chains.values()) playback.resume();
    };
    // Capture gestures before tuning controls can stop propagation. Autoplay may
    // leave resume() pending until the first interaction after a reload.
    document.addEventListener('pointerdown', resume, true);
    document.addEventListener('click', resume, true);
    document.addEventListener('keydown', resume, true);
    ctx.addEventListener('statechange', () => {
      for (const playback of chains.values()) playback.syncContext();
      notifyGestureState();
    });
  }
  return ctx;
}
function audioUrl(channel: number): string {
  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${protocol}//${location.host}/ws/audio?channel=${channel}`;
}
class AudioChain {
  enabled = false;
  muted = false;
  error = '';
  private worker: Worker | null = null;
  private node: AudioWorkletNode | ScriptProcessorNode | null = null;
  private initialization: Promise<void> | null = null;
  private receiverFailed = false;
  private generation = 0;
  private watchdog: number | null = null;
  private frames = 0;
  private buffered = 0;
  private underruns = 0;
  private rms = 0;
  private playing = false;
  private playedFrames = 0;
  private renderedFrames = 0;
  private starvedFrames = 0;
  private resets = 0;
  private ratio = 1;
  private streamKey = '';
  private inputIndex = '0';
  private audioIndex = '0';
  private delayUnknown = false;
  private causal = false;
  private mode = 'default';
  private playbackRate = 0;
  private bufferMode = 'default';
  private primeFrames = 0;
  private maxFillFrames = 0;
  private ring = new Float32Array(0);
  private available = 0;
  private read = 0;
  private write = 0;
  private channels = 1;
  private connectionState = '';

  private gainNode: GainNode | null = null;
  private panNode: StereoPannerNode | null = null;
  private localGain = 0.7;
  private localPan = 0;
  private fadeTimer: ReturnType<typeof setTimeout> | null = null;
  constructor(readonly channel: number | string) {}
  private destination(audio: AudioContext): AudioNode {
    if (typeof this.channel === 'number') return audio.destination;
    if (!this.gainNode) {
      this.gainNode = audio.createGain(); this.panNode = audio.createStereoPanner();
      this.gainNode.gain.value = 0; this.panNode.pan.value = this.localPan;
      this.gainNode.connect(this.panNode); this.panNode.connect(nativeBus(audio));
      ramp(this.gainNode.gain, this.localGain);
    }
    return this.gainNode;
  }
  local(gain: number, pan: number): void {
    const previousGain = this.localGain, previousPan = this.localPan;
    this.localGain = Math.max(0, Math.min(1, gain)); this.localPan = Math.max(-1, Math.min(1, pan));
    if (this.gainNode && this.fadeTimer === null && this.localGain !== previousGain) ramp(this.gainNode.gain, this.localGain);
    if (this.panNode && this.localPan !== previousPan) ramp(this.panNode.pan, this.localPan);
  }
  monitorEnabled(on: boolean): void {
    if (!on && this.fadeTimer !== null) return;
    if (this.fadeTimer !== null) { clearTimeout(this.fadeTimer); this.fadeTimer = null; }
    if (on) {
      if (this.gainNode) ramp(this.gainNode.gain, this.localGain);
      if (!this.enabled) this.setEnabled(true);
    } else if (this.enabled && this.gainNode) {
      ramp(this.gainNode.gain, 0);
      this.fadeTimer = setTimeout(() => { this.fadeTimer = null; this.setEnabled(false); }, 20);
    } else if (this.enabled) this.setEnabled(false);
  }

  /** Enabled playback with no terminal or receiver error: the state autoplay blocks. */
  wantsPlayback(): boolean {
    return this.enabled && !this.receiverFailed && !this.error;
  }
  status(): Record<string, string | number | boolean> {
    return { channel: this.channel, enabled: this.enabled, muted: this.muted, error: this.error,
      frames: this.frames, playedFrames: this.playedFrames, playing: this.playing,
      renderedFrames: this.renderedFrames, starvedFrames: this.starvedFrames, resets: this.resets, ratio: this.ratio,
      bufferedFrames: this.buffered, underruns: this.underruns, rms: this.rms,
      streamKey: this.streamKey, inputIndex: this.inputIndex, audioIndex: this.audioIndex,
      totalDelayUnknown: this.delayUnknown, causalPresentation: this.causal,
      localGain: this.localGain, pan: this.localPan, owner: typeof this.channel === 'string' ? 'workbench' : 'legacy',
      connectionState: this.connectionState,
      masterGain: masterVolume, masterMuted, outputChannels: ctx?.destination.maxChannelCount ?? 2, sampleRate: ctx?.sampleRate || 0, playbackRate: this.playbackRate, bufferMode: this.bufferMode,
      primeFrames: this.primeFrames, maxFillFrames: this.maxFillFrames, contextState: ctx?.state || 'none' };
  }

  private debug(): void {
    if (this.channel !== 1) return;
    const canvas = document.getElementById('spectrum');
    if (canvas) canvas.dataset.sdrAudio =
      `enabled=${this.enabled} muted=${this.muted} frames=${this.frames}` +
      ` buffered_ms=${(1000 * this.buffered / (this.playbackRate || ctx?.sampleRate || 48000)).toFixed(1)}` +
      ` underruns=${this.underruns} rms=${this.rms.toFixed(4)}` +
      ` playing=${this.playing ? 1 : 0} played_frames=${this.playedFrames} stream_key=${this.streamKey}` +
      ` input_index=${this.inputIndex} audio_index=${this.audioIndex} delay_unknown=${this.delayUnknown ? 1 : 0} causal=${this.causal ? 1 : 0}` +
      ` sample_rate=${ctx?.sampleRate || 0}` +
      ` playback_rate=${this.playbackRate} buffer_mode=${this.bufferMode} prime_frames=${this.primeFrames} max_fill_frames=${this.maxFillFrames}` +
      ` ctx=${ctx?.state || 'none'} worklet=${typeof AudioWorkletNode !== 'undefined' && this.node instanceof AudioWorkletNode} worker=${!!this.worker}` +
      ` error=${this.error}`;
    notifyGestureState();
  }
  private clearRing(): void {
    this.read = this.write = this.available = this.buffered = 0;
    this.playing = false;
  }
  private fail(message: string): void {
    if (message === 'Backend audio receiver failed') {
      this.setReceiverError(message);
      document.dispatchEvent(new CustomEvent('websa:sdr-stream-error', { detail: { stream: 'audio', message } }));
      return;
    }
    this.error = message;
    this.dispose();
    console.error(`SDR audio channel ${this.channel}: ${message}`);
    this.debug();
    if (typeof this.channel === 'string') document.dispatchEvent(new CustomEvent('websa:monitor-status'));
  }
  private dispose(): void {
    this.generation++;
    if (this.fadeTimer !== null) clearTimeout(this.fadeTimer);
    this.fadeTimer = null;
    this.clearWatchdog();
    this.worker?.terminate();
    this.worker = null;
    if (this.node) {
      if ('port' in this.node) { this.node.onprocessorerror = null; this.node.port.close(); }
      else this.node.onaudioprocess = null;
      this.node.disconnect();
    }
    this.node = null;
    this.gainNode?.disconnect(); this.panNode?.disconnect(); this.gainNode = null; this.panNode = null;
    if (this.channel === 1) { spectrumProbe?.disconnect(); spectrumProbe = null; }
    this.initialization = null;
    this.connectionState = '';
    this.rms = 0;
    this.clearRing();
  }
  resume(): void {
    if (!this.enabled || this.receiverFailed || this.error || !ctx) return;
    this.syncContext();
    if (ctx.state === 'running') return;
    const generation = this.generation;
    void ctx.resume().then(() => {
      if (generation === this.generation) this.syncContext();
    }).catch((error) => {
      // An autoplay refusal is recoverable on a later gesture; it must not
      // dispose the chain or require the user to change their mute preference.
      if (error instanceof DOMException && error.name === 'NotAllowedError') return;
      if (generation === this.generation && this.enabled) this.fail(error instanceof Error ? error.message : String(error));
    });
  }
  syncContext(): void {
    if (!this.enabled || this.receiverFailed || this.error || !ctx) return;
    if (ctx.state !== 'running') {
      // Never subscribe to live PCM while the consumer clock is stopped: its
      // bounded queue would overflow before autoplay can be unlocked.
      this.worker?.postMessage({ type: 'enabled', value: false });
      this.clearRing();
      this.debug();
      return;
    }
    if (!this.initialization) {
      const generation = this.generation;
      this.initialization = this.initialize().catch((error) => {
        if (generation !== this.generation) return;
        this.initialization = null;
        this.fail(error instanceof Error ? error.message : String(error));
      });
    }
    this.worker?.postMessage({ type: 'enabled', value: true });
    this.debug();
  }
  private legacy(context: AudioContext): void {
    this.ring = new Float32Array(Math.ceil(context.sampleRate * 3) * 2);
    const node = context.createScriptProcessor(2048, 0, 2);
    node.onaudioprocess = (event: AudioProcessingEvent) => {
      const left = event.outputBuffer.getChannelData(0);
      const right = event.outputBuffer.getChannelData(1);
      let starved = false;
      for (let i = 0; i < left.length; i++) {
        if (!this.enabled || this.muted || this.available === 0) {
          left[i] = right[i] = 0;
          if (this.enabled && !this.muted) starved = true;
          continue;
        }
        left[i] = this.ring[this.read];
        right[i] = this.channels === 2 ? this.ring[(this.read + 1) % this.ring.length] : left[i];
        this.read = (this.read + this.channels) % this.ring.length;
        this.available -= this.channels;
      }
      if (starved) this.underruns++;
      this.buffered = this.available / this.channels;
      this.debug();
    };
    node.connect(this.destination(context));
    this.node = node;
  }
  private message(data: Record<string, unknown>): void {
    if (data.type === 'samples' && data.samples instanceof Float32Array) {
      const channels = data.stereo ? 2 : 1;
      const samples = data.samples;
      if (channels !== this.channels) { this.clearRing(); this.channels = channels; }
      if (this.available + samples.length > ctx!.sampleRate * 3 * channels) {
        this.fail('Audio consumer is too slow: compatibility queue exceeded three seconds');
        return;
      }
      for (let i = 0; i < samples.length; i++) {
        this.ring[this.write] = samples[i];
        this.write = (this.write + 1) % this.ring.length;
      }
      this.available += samples.length;
      this.worker?.postMessage({ type: 'accepted', delivery: data.delivery, generation: data.generation });
    } else if (data.type === 'reset') {
      this.clearRing();
    } else if (data.type === 'identity') {
      this.streamKey = String(data.streamKey || '');
      this.inputIndex = String(data.inputIndex || '0');
      this.audioIndex = String(data.audioIndex || '0');
      this.delayUnknown = Boolean(data.totalDelayUnknown);
      this.causal = Boolean(data.causalPresentation);
      this.muted = false;
      this.clearWatchdog();
      this.worker?.postMessage({ type: 'mute', value: false });
    } else if (data.type === 'connection') {
      this.connectionState = String(data.state ?? '');
      if (this.connectionState === 'reconnecting') { this.playing = false; this.rms = 0; }
    } else if (data.type === 'error') {
      this.fail(String(data.message));
    } else if (data.type === 'stats') {
      if (typeof data.frames === 'number') this.frames = data.frames;
      if (typeof data.rms === 'number') this.rms = data.rms;
      if (typeof data.bufferedSamples === 'number') this.buffered = data.bufferedSamples;
      if (typeof data.underruns === 'number') this.underruns = data.underruns;
      if (typeof data.playing === 'boolean') this.playing = data.playing;
      if (typeof data.playedFrames === 'number') this.playedFrames = data.playedFrames;
      if (typeof data.renderedFrames === 'number') this.renderedFrames = data.renderedFrames;
      if (typeof data.starvedFrames === 'number') this.starvedFrames = data.starvedFrames;
      if (typeof data.resets === 'number') this.resets = data.resets;
      if (typeof data.ratio === 'number') this.ratio = data.ratio;
      if (typeof data.streamKey === 'string') this.streamKey = data.streamKey;
      if (typeof data.inputIndex === 'string') this.inputIndex = data.inputIndex;
      if (typeof data.audioIndex === 'string') this.audioIndex = data.audioIndex;
      if (typeof data.totalDelayUnknown === 'boolean') this.delayUnknown = data.totalDelayUnknown;
      if (typeof data.causalPresentation === 'boolean') this.causal = data.causalPresentation;
      if (typeof data.sampleRate === 'number') this.playbackRate = data.sampleRate;
      if (typeof data.bufferMode === 'string') this.bufferMode = data.bufferMode;
      if (typeof data.primeFrames === 'number') this.primeFrames = data.primeFrames;
      if (typeof data.maxFillFrames === 'number') this.maxFillFrames = data.maxFillFrames;
    }
    this.debug();
    if (typeof this.channel === 'string' && ['connection', 'identity'].includes(String(data.type))) document.dispatchEvent(new CustomEvent('websa:monitor-status'));
  }
  setReceiverError(message: string | null): void {
    if (message === null) {
      if (!this.receiverFailed) return;
      this.receiverFailed = false;
      this.error = '';
      this.setEnabled(this.enabled);
      return;
    }
    this.receiverFailed = true;
    this.error = message;
    this.dispose();
    this.muted = false;
    this.debug();
  }
  private async initialize(): Promise<void> {
    const generation = this.generation;
    const audio = context();
    let playbackRate = audio.sampleRate;
    let port: MessagePort | null = null;
    if (audio.audioWorklet && typeof AudioWorkletNode !== 'undefined') {
      let candidate: AudioWorkletNode | null = null;
      try {
        if (!modulePromise) modulePromise = audio.audioWorklet.addModule(WORKLET_URL);
        await modulePromise;
        if (generation !== this.generation) return;
        candidate = new AudioWorkletNode(audio, 'sdr-audio-processor', {
          numberOfInputs: 0, numberOfOutputs: 1, outputChannelCount: [2],
        });
        const output = candidate;
        await new Promise<void>((resolve, reject) => {
          const timer = window.setTimeout(() => reject(new Error('AudioWorklet ready timeout')), 1000);
          output.onprocessorerror = () => { window.clearTimeout(timer); reject(new Error('AudioWorklet startup failed')); };
          output.port.onmessage = (event: MessageEvent) => {
            if (event.data?.type === 'ready') {
              const rate = Number(event.data.sampleRate);
              window.clearTimeout(timer);
              if (!Number.isFinite(rate) || rate <= 0) { reject(new Error('Invalid AudioWorklet playback rate')); return; }
              playbackRate = rate;
              resolve();
            }
          };
          output.connect(this.destination(audio));
        });
        if (generation !== this.generation) { candidate.disconnect(); candidate.port.close(); return; }
        this.node = candidate;
        port = candidate.port;
        candidate.onprocessorerror = () => { if (generation === this.generation) this.fail('AudioWorklet processor stopped'); };
      } catch (error) {
        candidate?.disconnect();
        candidate?.port.close();
        if (generation !== this.generation) return;
        console.warn(`SDR audio channel ${this.channel}: using compatibility output`, error);
        this.legacy(audio);
      }
    } else this.legacy(audio);
    if (this.channel === 1 && this.node) {
      spectrumProbe = audio.createAnalyser();
      spectrumProbe.fftSize = 16384;
      this.node.connect(spectrumProbe);
      spectrumProbeBin = audio.sampleRate / spectrumProbe.fftSize;
    }
    this.playbackRate = playbackRate;
    const worker = new Worker(new URL('./sdrAudioWorker.ts', import.meta.url), { type: 'module' });
    this.worker = worker;
    worker.onmessage = (event: MessageEvent) => { if (this.worker === worker) this.message(event.data || {}); };
    worker.onerror = (event: ErrorEvent) => { if (this.worker === worker) this.fail(event.message || 'Audio worker failed'); };
    worker.postMessage({ type: 'init', url: typeof this.channel === 'number' ? audioUrl(this.channel)
      : `${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/ws/audio?receiver_uid=${encodeURIComponent(this.channel)}`, reconnect: typeof this.channel === 'string', targetRate: playbackRate, mode: this.mode, flowControl: Boolean(port), port }, port ? [port] : []);
    worker.postMessage({ type: 'enabled', value: this.enabled && audio.state === 'running' });
    worker.postMessage({ type: 'mute', value: this.muted });
  }
  setMode(mode: string): void {
    const next = mode || 'default';
    if (next === this.mode) return;
    this.mode = next;
    if (this.error && !this.receiverFailed && this.enabled) { this.setEnabled(false); this.setEnabled(true); return; }
    this.worker?.postMessage({ type: 'mode', mode: next });
  }
  setEnabled(on: boolean): void {
    if (on && this.enabled && this.error && !this.receiverFailed) return;
    this.clearWatchdog();
    this.enabled = on;
    if (this.receiverFailed) { this.debug(); return; }
    this.muted = false;
    this.error = '';
    this.clearRing();
    if (!on) { this.dispose(); this.debug(); return; }
    if (on) {
      try { context(); } catch (error) { this.fail(error instanceof Error ? error.message : String(error)); return; }
      this.resume();
    }
    this.worker?.postMessage({ type: 'mute', value: false });
    this.debug();
  }
  private clearWatchdog(): void {
    if (this.watchdog !== null) window.clearTimeout(this.watchdog);
    this.watchdog = null;
  }
  transition(): void {
    if (!this.enabled || this.receiverFailed) return;
    this.clearWatchdog();
    this.muted = true;
    this.clearRing();
    this.worker?.postMessage({ type: 'mute', value: true });
    this.watchdog = window.setTimeout(() => {
      this.watchdog = null;
      this.muted = false;
      this.worker?.postMessage({ type: 'mute', value: false });
      this.debug();
    }, 700);
    this.debug();
  }
}
function chain(channel: number): AudioChain {
  if (!Number.isInteger(channel) || channel < 1 || channel > 3) throw new Error('Audio channel must be 1, 2, or 3');
  let playback = chains.get(channel);
  if (!playback) { playback = new AudioChain(channel); chains.set(channel, playback); }
  return playback;
}
export function setSdrAudioMode(channel: number, mode: string): void { chain(channel).setMode(mode); }
export function setSdrChannelAudioEnabled(channel: number, on: boolean): void {
  legacyWanted.set(channel, on); if (!nativeOwner) chain(channel).setEnabled(on);
}
/** The workbench owns local speakers for this page; legacy STATUS must not duplicate playback. */
export function claimNativeAudio(): () => void {
  if (nativeOwner) throw new Error('Native audio already has an owner');
  nativeOwner = true; for (const [key, playback] of chains) if (typeof key === 'number') playback.setEnabled(false);
  document.dispatchEvent(new CustomEvent('websa:monitor-owner', { detail: { native: true } }));
  return () => {
    for (const [key, playback] of chains) if (typeof key === 'string') { playback.setEnabled(false); chains.delete(key); }
    nativeOwner = false; for (const [key, wanted] of legacyWanted) chain(key).setEnabled(wanted);
    document.dispatchEvent(new CustomEvent('websa:monitor-owner', { detail: { native: false } }));
  };
}
export function setNativeAudioRoute(uid: string, mode: string, on: boolean, gain: number, pan: number, restart = false): void {
  if (!nativeOwner) throw new Error('Native monitor has no owner');
  let playback = chains.get(uid);
  if (!playback && !on) return;
  if (!playback) { playback = new AudioChain(uid); chains.set(uid, playback); }
  if (restart) { playback.setEnabled(false); playback.setReceiverError(null); }
  playback.local(gain, pan); playback.setMode(mode); playback.monitorEnabled(on);
}
export function forgetNativeAudio(uid: string): void { chains.get(uid)?.setEnabled(false); chains.delete(uid); }
export function nativeAudioOwnsSpeakers(): boolean { return nativeOwner; }
export function nativeAudioIsMuted(): boolean { return masterMuted; }
export function setNativeMaster(gain: number, muted: boolean): void {
  const changed = masterVolume !== gain || masterMuted !== muted;
  masterVolume = Math.max(0, Math.min(1, gain)); masterMuted = muted;
  if (master && changed) ramp(master.gain, muted ? 0 : masterVolume);
  if (changed) document.dispatchEvent(new CustomEvent('websa:monitor-master'));
}
export function prepareSdrChannelAudioTransition(channel: number): void { chain(channel).transition(); }
export function getSdrChannelAudioError(channel: number): string { return chain(channel).error; }
export function setSdrReceiverAudioError(channel: number, message: string | null): void { chain(channel).setReceiverError(message); }
export function setSdrAudioEnabled(on: boolean): void { setSdrChannelAudioEnabled(1, on); }
export function prepareSdrAudioTransition(): void { prepareSdrChannelAudioTransition(1); }
export function audioSampleRate(): number {
  try { return context().sampleRate; } catch { return 48000; }
}
export function getSdrAudioStatus(): Array<Record<string, string | number | boolean>> {
  return Array.from(chains.values(), (playback) => playback.status());
}
Object.assign(window, { websaAudioStatus: getSdrAudioStatus });
export function sampleSpectrum(top = 6): Array<{ hz: number; db: number; relativeDb: number; loudestDb: number }> {
  if (!spectrumProbe) return [];
  const bins = new Float32Array(spectrumProbe.frequencyBinCount);
  spectrumProbe.getFloatFrequencyData(bins);
  const loudest = Math.max(...bins);
  const indices = Array.from({ length: bins.length - 1 }, (_, i) => i + 1).sort((a, b) => bins[b] - bins[a]);
  const out: Array<{ hz: number; db: number; relativeDb: number; loudestDb: number }> = [];
  const used: number[] = [];
  for (const i of indices) {
    if (!Number.isFinite(bins[i]) || used.some((u) => Math.abs(u - i) < 4)) continue;
    used.push(i);
    out.push({ hz: i * spectrumProbeBin, db: bins[i], relativeDb: bins[i] - loudest, loudestDb: loudest });
    if (out.length >= top) break;
  }
  return out;
}
export async function sampleEnvelope(ms = 1000): Promise<Record<string, number | number[]>> {
  const empty = { stepMs: 20, count: 0, min: 0, mean: 0, max: 0, ratio: 1, levels: [] as number[] };
  if (!spectrumProbe) return empty;
  const buffer = new Float32Array(Math.round(spectrumProbe.context.sampleRate * 0.02));
  const levels: number[] = [];
  const until = performance.now() + Math.max(0, ms);
  while (performance.now() < until) {
    spectrumProbe.getFloatTimeDomainData(buffer);
    let sum = 0;
    for (const sample of buffer) sum += sample * sample;
    levels.push(Math.sqrt(sum / buffer.length));
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  }
  if (levels.length === 0) return empty;
  const min = Math.min(...levels);
  const max = Math.max(...levels);
  const mean = levels.reduce((sum, value) => sum + value, 0) / levels.length;
  return { stepMs: 20, count: levels.length, min, mean, max, ratio: max / Math.max(min, 1e-9), levels };
}

export function resumeNativeAudio(): void { for (const playback of chains.values()) playback.resume(); }

let mixProbe: [AnalyserNode, AnalyserNode] | null = null;
/** Diagnostic samples from the actual post-limiter stereo output; creates no additional playback path. */
export function nativeMixLevels(): { left: number; right: number } {
  if (!ctx || !limiter) return { left: 0, right: 0 };
  if (!mixProbe) {
    const splitter = ctx.createChannelSplitter(2); limiter.connect(splitter);
    mixProbe = [ctx.createAnalyser(), ctx.createAnalyser()];
    mixProbe.forEach((probe, index) => { probe.fftSize = 2048; splitter.connect(probe, index); });
  }
  const levels = mixProbe.map(probe => {
    const data = new Float32Array(probe.fftSize); probe.getFloatTimeDomainData(data);
    return Math.sqrt(data.reduce((power, value) => power + value * value, 0) / data.length);
  });
  return { left: levels[0], right: levels[1] };
}
Object.assign(window, { websaNativeMixLevels: nativeMixLevels });
