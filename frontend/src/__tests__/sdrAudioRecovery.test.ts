import { afterEach, describe, expect, it, vi } from 'vitest';

class PlaybackWorker {
  static instances: PlaybackWorker[] = [];
  onmessage: ((event: { data: Record<string, unknown> }) => void) | null = null;
  onerror: ((event: { message: string }) => void) | null = null;
  postMessage = vi.fn();
  terminate = vi.fn();
  constructor() { PlaybackWorker.instances.push(this); }
}

class PlaybackContext {
  sampleRate = 48000;
  state = 'running';
  destination = {};
  resume = vi.fn().mockResolvedValue(undefined);
  createScriptProcessor() { return { connect: vi.fn(), disconnect: vi.fn(), onaudioprocess: null }; }
  createAnalyser() { return { connect: vi.fn(), disconnect: vi.fn(), fftSize: 0 }; }
}

afterEach(() => { vi.unstubAllGlobals(); });

describe('receiver audio recovery', () => {
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
    old.onmessage!({ data: { type: 'stats', rms: 0.5, playing: true, bufferedSamples: 100 } });
    audio.setSdrReceiverAudioError(1, 'receiver queue full');
    expect(old.terminate).toHaveBeenCalledOnce();
    old.onmessage!({ data: { type: 'stats', rms: 1, playing: true, bufferedSamples: 999 } });
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ error: 'receiver queue full', rms: 0, playing: false, bufferedFrames: 0 });
    audio.setSdrAudioEnabled(false);
    audio.setSdrAudioEnabled(true);
    audio.setSdrAudioMode(1, 'drm');
    expect(PlaybackWorker.instances).toHaveLength(1);
    expect(audio.getSdrChannelAudioError(1)).toBe('receiver queue full');
    audio.setSdrReceiverAudioError(1, null);
    expect(PlaybackWorker.instances).toHaveLength(2);
    PlaybackWorker.instances[1].onmessage!({ data: { type: 'stats', rms: 0.25, playing: true, bufferedSamples: 24 } });
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
  it('contains rejected resume and discards callbacks from the failed output', async () => {
    vi.resetModules();
    PlaybackWorker.instances = [];
    class SuspendedContext extends PlaybackContext {
      override resume = vi.fn().mockRejectedValue(new Error('Audio permission denied'));
    }
    vi.stubGlobal('AudioContext', SuspendedContext);
    vi.stubGlobal('Worker', PlaybackWorker);
    // A fresh module must observe the installed browser capability.
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioEnabled(true);
    const old = PlaybackWorker.instances[0];
    await Promise.resolve();
    expect(audio.getSdrChannelAudioError(1)).toBe('Audio permission denied');
    expect(old.terminate).toHaveBeenCalledOnce();
    old.onmessage!({ data: { type: 'stats', playing: true, rms: 1 } });
    expect(audio.getSdrAudioStatus()[0]).toMatchObject({ playing: false, rms: 0 });
    audio.setSdrAudioEnabled(true);
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
