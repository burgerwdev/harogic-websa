import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

class PlaybackWorker {
  static instances: PlaybackWorker[] = [];
  onmessage: ((event: { data: Record<string, unknown> }) => void) | null = null;
  onerror: ((event: { message: string }) => void) | null = null;
  postMessage = vi.fn();
  terminate = vi.fn();
  constructor() { PlaybackWorker.instances.push(this); }
}

class PlaybackContext extends EventTarget {
  sampleRate = 48000;
  state = 'running';
  destination = {};
  resume = vi.fn().mockResolvedValue(undefined);
  createScriptProcessor() { return { connect: vi.fn(), disconnect: vi.fn(), onaudioprocess: null }; }
  createAnalyser() { return { connect: vi.fn(), disconnect: vi.fn(), fftSize: 0 }; }
}

beforeEach(() => { vi.spyOn(document, 'addEventListener'); });
afterEach(() => {
  for (const [type, listener, options] of vi.mocked(document.addEventListener).mock.calls) {
    document.removeEventListener(type, listener, options);
  }
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe('receiver audio recovery', () => {
  it.each(['receiver retired', 'receiver stopped'])('releases ended %s playback without reviving it from stale state', async reason => {
    vi.resetModules(); PlaybackWorker.instances = [];
    vi.stubGlobal('Worker', PlaybackWorker); vi.stubGlobal('AudioContext', PlaybackContext);
    const quiet = vi.spyOn(console, 'error').mockImplementation(() => {});
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioEnabled(true);
    const old = PlaybackWorker.instances[0];
    old.onmessage!({ data: { type: 'stats', playing: true, bufferedSamples: 100 } });
    old.onmessage!({ data: { type: 'meter', generation: 0, rms: .5, peak: .75, playing: true } });
    old.onmessage!({ data: { type: 'ended', reason } });
    expect(old.terminate).toHaveBeenCalledOnce();
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ error: '', playing: false, rms: 0, peak: 0,
      meterAt: 0, bufferedFrames: 0, connectionState: 'stopped' });
    audio.setSdrAudioEnabled(true); audio.resumeNativeAudio();
    expect(PlaybackWorker.instances).toHaveLength(1);
    old.onmessage!({ data: { type: 'meter', generation: 0, rms: 1, peak: 1, playing: true } });
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ playing: false, rms: 0 });
    expect(quiet).not.toHaveBeenCalled();
    audio.setSdrAudioEnabled(false); audio.setSdrAudioEnabled(true);
    expect(PlaybackWorker.instances).toHaveLength(2);
    old.onerror!({ message: 'late retired owner' });
    expect(audio.getSdrChannelAudioError(1)).toBe('');
    audio.setSdrAudioEnabled(false);
  });
  it('silences the failed receiver, ignores stale worker stats, and recreates playback on recovery', async () => {
    vi.resetModules();
    PlaybackWorker.instances = [];
    vi.stubGlobal('Worker', PlaybackWorker);
    vi.stubGlobal('AudioContext', PlaybackContext);
    // Reset the module to exercise a fresh playback graph, independent of other audio tests.
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioMode(1, 'drm');
    audio.setSdrAudioEnabled(true);
    const old = PlaybackWorker.instances[0];
    old.onmessage!({ data: { type: 'stats', playing: true, bufferedSamples: 100 } });
    old.onmessage!({ data: { type: 'meter', generation: 0, rms: .5, peak: .75, playing: true } });
    audio.setSdrReceiverAudioError(1, 'receiver queue full');
    expect(old.terminate).toHaveBeenCalledOnce();
    old.onmessage!({ data: { type: 'stats', playing: true, bufferedSamples: 999 } });
    old.onmessage!({ data: { type: 'meter', generation: 0, rms: 1, peak: 1, playing: true } });
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ error: 'receiver queue full', rms: 0, playing: false, bufferedFrames: 0 });
    audio.setSdrAudioEnabled(false);
    audio.setSdrAudioEnabled(true);
    audio.setSdrAudioMode(1, 'drm');
    expect(PlaybackWorker.instances).toHaveLength(1);
    expect(audio.getSdrChannelAudioError(1)).toBe('receiver queue full');
    audio.setSdrReceiverAudioError(1, null);
    expect(PlaybackWorker.instances).toHaveLength(2);
    const recovered = PlaybackWorker.instances[1];
    recovered.onmessage!({ data: { type: 'stats', playing: true, bufferedSamples: 24 } });
    recovered.onmessage!({ data: { type: 'meter-reset', generation: 2 } });
    recovered.onmessage!({ data: { type: 'meter', generation: 1, rms: 1, peak: 1, playing: true } });
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ rms: 0, peak: 0, playing: false });
    recovered.onmessage!({ data: { type: 'meter', generation: 2, rms: .25, peak: .5, playing: true } });
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ error: '', enabled: true, rms: 0.25, playing: true, bufferedFrames: 24 });
  });
  it('recreates failed playback on an explicit mode change and rejects stale worker callbacks', async () => {
    vi.resetModules();
    PlaybackWorker.instances = [];
    vi.stubGlobal('Worker', PlaybackWorker);
    vi.stubGlobal('AudioContext', PlaybackContext);
    // A fresh module must observe the installed browser capability.
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioMode(1, 'am');
    audio.setSdrAudioEnabled(true);
    const old = PlaybackWorker.instances[0];
    old.onerror!({ message: 'Playback worker stopped' });
    audio.setSdrAudioEnabled(true);
    expect(PlaybackWorker.instances).toHaveLength(1);
    audio.setSdrAudioMode(1, 'cw');
    expect(PlaybackWorker.instances).toHaveLength(2);
    old.onerror!({ message: 'Stale failure' });
    expect(audio.getSdrChannelAudioError(1)).toBe('');
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ enabled: true });
  });
  it('reports unavailable audio setup without throwing or retrying each status refresh', async () => {
    vi.resetModules();
    const unavailable = vi.fn(function () { throw new Error('Audio unavailable'); });
    vi.stubGlobal('AudioContext', unavailable);
    // A fresh module must observe the installed browser capability.
    const audio = await import('../audio/sdrAudio');
    expect(() => audio.setSdrAudioEnabled(true)).not.toThrow();
    expect(audio.getSdrChannelAudioError(1)).toBe('Audio unavailable');
    audio.setSdrAudioEnabled(true);
    expect(unavailable).toHaveBeenCalledOnce();
    audio.setSdrAudioEnabled(false);
    audio.setSdrAudioEnabled(true);
    expect(unavailable).toHaveBeenCalledTimes(2);
  });
  it('starts restored playback automatically when resume is permitted without a gesture', async () => {
    vi.resetModules();
    PlaybackWorker.instances = [];
    class AllowedContext extends PlaybackContext {
      override state = 'suspended';
      override resume = vi.fn(async () => { this.state = 'running'; });
    }
    vi.stubGlobal('AudioContext', AllowedContext);
    vi.stubGlobal('Worker', PlaybackWorker);
    // Reload after installing browser globals; a static import would retain the old context.
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioEnabled(true);
    expect(PlaybackWorker.instances).toHaveLength(0);
    await Promise.resolve();
    expect(PlaybackWorker.instances).toHaveLength(1);
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ enabled: true, error: '', contextState: 'running' });
  });
  it('contains terminal resume errors without opening an audio subscription', async () => {
    vi.resetModules();
    PlaybackWorker.instances = [];
    class SuspendedContext extends PlaybackContext {
      override state = 'suspended';
      override resume = vi.fn().mockRejectedValue(new Error('Audio device unavailable'));
    }
    vi.stubGlobal('AudioContext', SuspendedContext);
    vi.stubGlobal('Worker', PlaybackWorker);
    // Reload after installing browser globals; a static import would retain the old context.
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioEnabled(true);
    await Promise.resolve();
    await Promise.resolve();
    expect(audio.getSdrChannelAudioError(1)).toBe('Audio device unavailable');
    expect(PlaybackWorker.instances).toHaveLength(0);
    audio.setSdrAudioEnabled(true);
    expect(PlaybackWorker.instances).toHaveLength(0);
  });
  it.each(['pending', 'denied'])('unlocks restored audio on an ordinary gesture after %s autoplay', async (policy) => {
    vi.resetModules();
    vi.useFakeTimers();
    PlaybackWorker.instances = [];
    let context!: SuspendedContext;
    let allowPlayback = false;
    class SuspendedContext extends PlaybackContext {
      override state = 'suspended';
      override resume = vi.fn(() => {
        // ES2020 lacks Promise.withResolvers; emulate a browser's pending resume.
        if (!allowPlayback) return policy === 'denied'
          ? Promise.reject(new DOMException('User activation required', 'NotAllowedError'))
          : new Promise<void>(() => {});
        this.state = 'running';
        this.dispatchEvent(new Event('statechange'));
        return Promise.resolve();
      });
      constructor() { super(); context = this; }
    }
    vi.stubGlobal('AudioContext', SuspendedContext);
    vi.stubGlobal('Worker', PlaybackWorker);
    // Reload after installing browser globals; a static import would retain the old context.
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioMode(1, 'am');
    audio.setSdrAudioEnabled(true);
    await Promise.resolve();
    await Promise.resolve();
    vi.advanceTimersByTime(10000);
    expect(PlaybackWorker.instances).toHaveLength(0);
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ enabled: true, error: '', contextState: 'suspended' });

    allowPlayback = true;
    const control = document.createElement('button');
    document.body.append(control);
    control.addEventListener('pointerdown', event => event.stopPropagation());
    control.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    control.remove();
    await Promise.resolve();
    expect(context.resume).toHaveBeenCalledTimes(2);
    expect(PlaybackWorker.instances).toHaveLength(1);
    expect(PlaybackWorker.instances[0].postMessage).toHaveBeenCalledWith({ type: 'enabled', value: true });
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ enabled: true, error: '', contextState: 'running' });
  });
  it('flags autoplay-held silence so the status hint can show it', async () => {
    vi.resetModules();
    PlaybackWorker.instances = [];
    let context!: PlaybackContext;
    class SuspendedContext extends PlaybackContext {
      override state = 'suspended';
      constructor() { super(); context = this; }
    }
    vi.stubGlobal('AudioContext', SuspendedContext);
    vi.stubGlobal('Worker', PlaybackWorker);
    const pending: boolean[] = [];
    const onGesture = (event: Event) => pending.push((event as CustomEvent).detail.pending);
    document.addEventListener('websa:sdr-audio-gesture', onGesture);
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioEnabled(true);
    await Promise.resolve();
    expect(audio.sdrAudioNeedsGesture()).toBe(true);
    expect(pending.at(-1)).toBe(true);
    // Muting the channel removes the wanted playback, so the hint must clear.
    audio.setSdrAudioEnabled(false);
    expect(audio.sdrAudioNeedsGesture()).toBe(false);
    expect(pending.at(-1)).toBe(false);
    audio.setSdrAudioEnabled(true);
    expect(audio.sdrAudioNeedsGesture()).toBe(true);
    // Any gesture that resumes the context clears the flag through statechange.
    context.state = 'running';
    context.dispatchEvent(new Event('statechange'));
    expect(audio.sdrAudioNeedsGesture()).toBe(false);
    expect(pending.at(-1)).toBe(false);
    document.removeEventListener('websa:sdr-audio-gesture', onGesture);
  });
  it('does not unlock a muted channel or revive a disabled channel from a pending resume', async () => {
    vi.resetModules();
    PlaybackWorker.instances = [];
    let context!: SuspendedContext;
    let resumed!: () => void;
    class SuspendedContext extends PlaybackContext {
      override state = 'suspended';
      // ES2020 lacks Promise.withResolvers; retain the pending resume completion.
      override resume = vi.fn(() => new Promise<void>(resolve => { resumed = resolve; }));
      constructor() { super(); context = this; }
    }
    vi.stubGlobal('AudioContext', SuspendedContext);
    vi.stubGlobal('Worker', PlaybackWorker);
    // Reload after installing browser globals; a static import would retain the old context.
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioEnabled(false);
    document.dispatchEvent(new Event('keydown'));
    expect(PlaybackWorker.instances).toHaveLength(0);
    audio.setSdrAudioEnabled(true);
    audio.setSdrAudioEnabled(false);
    context.state = 'running';
    context.dispatchEvent(new Event('statechange'));
    resumed();
    await Promise.resolve();
    document.dispatchEvent(new Event('click'));
    expect(PlaybackWorker.instances).toHaveLength(0);
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ enabled: false, error: '' });
    audio.setSdrAudioEnabled(true);
    expect(PlaybackWorker.instances).toHaveLength(1);
  });
  it('pauses the subscription when the context stops and resumes it when running', async () => {
    vi.resetModules();
    PlaybackWorker.instances = [];
    let context!: InterruptibleContext;
    class InterruptibleContext extends PlaybackContext {
      constructor() { super(); context = this; }
    }
    vi.stubGlobal('AudioContext', InterruptibleContext);
    vi.stubGlobal('Worker', PlaybackWorker);
    // Reload after installing browser globals; a static import would retain the old context.
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioEnabled(true);
    const worker = PlaybackWorker.instances[0];
    context.state = 'suspended';
    context.dispatchEvent(new Event('statechange'));
    expect(worker.postMessage).toHaveBeenLastCalledWith({ type: 'enabled', value: false });
    context.state = 'running';
    context.dispatchEvent(new Event('statechange'));
    expect(worker.postMessage).toHaveBeenLastCalledWith({ type: 'enabled', value: true });
    expect(PlaybackWorker.instances).toHaveLength(1);
  });
  it('cancels pending worklet startup across disable and receiver failure', async () => {
    vi.resetModules();
    PlaybackWorker.instances = [];
    let loaded!: () => void;
    const module = new Promise<void>(resolve => { loaded = resolve; });
    class WorkletContext extends PlaybackContext {
      audioWorklet = { addModule: vi.fn(() => module) };
    }
    class Output {
      static instances: Output[] = [];
      port = { onmessage: null as ((event: { data: unknown }) => void) | null, close: vi.fn() };
      onprocessorerror: (() => void) | null = null;
      connect = vi.fn(() => { queueMicrotask(() => this.port.onmessage?.({ data: { type: 'ready', sampleRate: 48000 } })); });
      disconnect = vi.fn();
      constructor() { Output.instances.push(this); }
    }
    vi.stubGlobal('AudioContext', WorkletContext);
    vi.stubGlobal('AudioWorkletNode', Output);
    vi.stubGlobal('Worker', PlaybackWorker);
    // A fresh module must observe the installed browser capability.
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioEnabled(true);
    audio.setSdrAudioEnabled(false);
    audio.setSdrAudioEnabled(true);
    audio.setSdrReceiverAudioError(1, 'receiver stopped');
    loaded();
    await module;
    await Promise.resolve();
    expect(Output.instances).toHaveLength(0);
    expect(PlaybackWorker.instances).toHaveLength(0);
    audio.setSdrReceiverAudioError(1, null);
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
    expect(PlaybackWorker.instances).toHaveLength(1);
    const output = Output.instances[0];
    const staleFailure = output.onprocessorerror!;
    audio.setSdrAudioEnabled(false);
    staleFailure();
    expect(output.port.close).toHaveBeenCalledOnce();
    expect(audio.getSdrChannelAudioError(1)).toBe('');
  });
});
