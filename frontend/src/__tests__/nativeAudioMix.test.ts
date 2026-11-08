import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
class Param {
  value = 0;
  cancelScheduledValues = vi.fn(); setValueAtTime = vi.fn((value: number) => { this.value = value; });
  linearRampToValueAtTime = vi.fn((value: number) => { this.value = value; });
}
class Node {
  gain = new Param(); pan = new Param(); threshold = new Param(); knee = new Param(); ratio = new Param(); attack = new Param(); release = new Param();
  connect = vi.fn(); disconnect = vi.fn(); onaudioprocess = null;
  getFloatTimeDomainData = vi.fn((data: Float32Array) => data.fill(.25));
}
class Context extends EventTarget {
  static instances: Context[] = []; gains: Node[] = []; pans: Node[] = []; compressors: Node[] = [];
  processors: Node[] = []; analysers: Node[] = [];
  sampleRate = 48000; currentTime = 0; state = 'running'; destination = new Node(); resume = vi.fn().mockResolvedValue(undefined);
  constructor() { super(); Context.instances.push(this); }
  createGain() { const node = new Node(); this.gains.push(node); return node; }
  createStereoPanner() { const node = new Node(); this.pans.push(node); return node; }
  createDynamicsCompressor() { const node = new Node(); this.compressors.push(node); return node; }
  createScriptProcessor() { const node = new Node(); this.processors.push(node); return node; }
  createAnalyser() { const node = new Node(); this.analysers.push(node); return node; }
}
class Worker {
  static instances: Worker[] = []; postMessage = vi.fn(); terminate = vi.fn();
  onmessage: ((event: { data: Record<string, unknown> }) => void) | null = null;
  constructor() { Worker.instances.push(this); }
}
const source = { service_id: 'service-a', receiver_uid: 'uid-a', available: true };
const directory = { service_id: 'service-a', receivers: [{ display_number: 1, receiver_uid: 'uid-a' }] };
beforeEach(() => {
  vi.resetModules(); vi.useFakeTimers(); Context.instances = []; Worker.instances = [];
  vi.stubGlobal('AudioContext', Context); vi.stubGlobal('Worker', Worker);
  vi.spyOn(document, 'addEventListener');
});
afterEach(() => {
  for (const [type, listener, options] of vi.mocked(document.addEventListener).mock.calls) document.removeEventListener(type, listener, options);
  vi.unstubAllGlobals(); vi.restoreAllMocks(); vi.useRealTimers();
});
describe('local speaker graph', () => {
  it('samples an existing playing channel on demand and releases its waveform probe without another consumer', async () => {
    const audio = await import('../audio/sdrAudio');
    const release = audio.claimNativeAudio(directory);
    audio.setNativeAudioRoute('uid-a', 'am', true, .7, 0);
    const worker = Worker.instances[0], context = Context.instances[0];
    const samples = new Float32Array(256);
    expect(audio.sampleNativeAudioWaveform('uid-a', samples)).toBe(false);
    worker.onmessage!({ data: { type: 'meter', generation: 0, rms: .2, peak: .4, playing: true } });
    expect(audio.sampleNativeAudioWaveform('uid-a', samples)).toBe(true);
    expect(samples[0]).toBe(.25); expect(Worker.instances).toHaveLength(1);
    const probe = context.analysers.at(-1)!;
    expect(context.processors[0].connect).toHaveBeenCalledWith(probe);
    audio.sampleNativeAudioWaveform('uid-a', samples);
    expect(context.analysers).toHaveLength(1);
    context.state = 'suspended'; expect(audio.sampleNativeAudioWaveform('uid-a', samples)).toBe(false);
    audio.sampleNativeAudioWaveform('uid-a', null);
    expect(context.processors[0].disconnect).toHaveBeenCalledWith(probe); expect(probe.disconnect).toHaveBeenCalledOnce();
    release();
  });
  it('shares one context/master, ramps gain and pan locally, and prevents legacy duplicate playback', async () => {
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioEnabled(true); const legacy = Worker.instances[0];
    const release = audio.claimNativeAudio(); expect(legacy.terminate).toHaveBeenCalledOnce();
    audio.setNativeAudioRoute('uid-a', 'am', true, 0.4, -0.8); audio.setNativeAudioRoute('uid-b', 'usb', true, 0.3, 0.6);
    audio.setSdrAudioEnabled(true); expect(Worker.instances).toHaveLength(3);
    const ctx = Context.instances[0]; expect(Context.instances).toHaveLength(1); expect(ctx.compressors).toHaveLength(1);
    expect(ctx.gains[0].gain.value).toBe(0.4); expect(ctx.gains[2].gain.value).toBe(0.3);
    expect(ctx.pans.map(node => node.pan.value)).toEqual([-0.8, 0.6]);
    const backendPosts = Worker.instances.slice(1).map(worker => worker.postMessage.mock.calls.length);
    audio.setNativeAudioRoute('uid-a', 'am', true, 0.2, 1); audio.setNativeMaster(0.55, false);
    expect(ctx.gains[0].gain.value).toBe(0.2); expect(ctx.pans[0].pan.value).toBe(1); expect(ctx.gains[1].gain.value).toBe(0.55);
    audio.setNativeMaster(0.55, true); expect(ctx.gains[1].gain.value).toBe(0);
    expect(Worker.instances.slice(1).map(worker => worker.postMessage.mock.calls.length)).toEqual(backendPosts);
    audio.setNativeAudioRoute('uid-a', 'am', false, 0.2, 1);
    audio.setNativeAudioRoute('uid-a', 'am', false, 0.3, 1);
    expect(ctx.gains[0].gain.value).toBe(0);
    await vi.advanceTimersByTimeAsync(20);
    expect(Worker.instances[1].terminate).toHaveBeenCalledOnce(); expect(Worker.instances[2].terminate).not.toHaveBeenCalled();
    const dispatch = vi.spyOn(document, 'dispatchEvent');
    Worker.instances[2].onmessage!({ data: { type: 'error', message: 'Backend audio receiver failed' } });
    const failure = dispatch.mock.calls.map(([event]) => event as CustomEvent).find(event => event.type === 'websa:sdr-stream-error');
    expect(failure?.detail).toMatchObject({ stream: 'audio', channel: 'uid-b', message: 'Backend audio receiver failed' });
    expect(audio.getSdrAudioStatus().find(r => r.channel === 'uid-a')?.error).toBe('');
    expect(audio.getSdrAudioStatus().find(r => r.channel === 'uid-b')?.error).toBe('Backend audio receiver failed');
    audio.setNativeAudioRoute('uid-b', 'usb', true, 0.3, 0.6, true);
    expect(Worker.instances).toHaveLength(4);
    expect(audio.getSdrAudioStatus().find(r => r.channel === 'uid-b')?.error).toBe('');
    release(); expect(Worker.instances[2].terminate).toHaveBeenCalledOnce(); expect(Worker.instances[3].terminate).toHaveBeenCalledOnce();
    audio.setSdrAudioEnabled(false);
  });

  it('adopts the main UID consumer with its playback buffer, meter and output nodes intact', async () => {
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioMode(1, 'am', source); audio.setSdrAudioEnabled(true);
    const worker = Worker.instances[0], ctx = Context.instances[0], output = ctx.processors[0];
    expect(worker.postMessage).toHaveBeenCalledWith(expect.objectContaining({ type: 'init',
      url: expect.stringContaining('/ws/audio?receiver_uid=uid-a'), reconnect: true }), []);
    expect(ctx.gains.map(node => node.gain.value)).toEqual([1, 1]);
    expect(ctx.compressors[0].ratio.value).toBe(1); // Main playback retains its original unity output.
    worker.onmessage!({ data: { type: 'stats', frames: 24, playedFrames: 12000, playing: true, bufferedSamples: 22000, resets: 1 } });
    worker.onmessage!({ data: { type: 'meter', generation: 0, rms: .3, peak: .5, playing: true } });
    const before = audio.getSdrAudioStatus()[0], posts = worker.postMessage.mock.calls.length;
    audio.setSdrAudioMode(1, 'am', source); // An unchanged STATUS cannot reset playback either.
    const release = audio.claimNativeAudio(directory);
    audio.setNativeMaster(.7, false); audio.setNativeAudioRoute('uid-a', 'am', true, .7, -.25);
    expect(Worker.instances).toHaveLength(1); expect(worker.terminate).not.toHaveBeenCalled();
    expect(worker.postMessage).toHaveBeenCalledTimes(posts);
    expect(audio.getSdrAudioStatus()).toEqual([{ ...before, channel: 'uid-a', owner: 'workbench', localGain: .7, pan: -.25 }]);
    expect(output.disconnect).not.toHaveBeenCalled(); expect(output.connect).toHaveBeenCalledTimes(2);
    expect(ctx.processors).toHaveLength(1); expect(ctx.gains.map(node => node.gain.value)).toEqual([.7, .7]);
    expect(ctx.compressors[0].ratio.value).toBe(20);
    // A compatibility control/error on the old number must not release the adopted probe.
    audio.setSdrReceiverAudioError(1, 'compatibility decode failed');
    expect(ctx.analysers[0].disconnect).not.toHaveBeenCalled();
    audio.setSdrAudioEnabled(false); release();
  });

  it.each(['service', 'receiver'] as const)('rejects adoption after the %s identity changes', async changed => {
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioMode(1, 'am', source); audio.setSdrAudioEnabled(true);
    const old = Worker.instances[0];
    const uid = changed === 'receiver' ? 'replacement' : 'uid-a';
    const next = { service_id: changed === 'service' ? 'service-b' : 'service-a', receivers: [{ display_number: 1, receiver_uid: uid }] };
    const release = audio.claimNativeAudio(next);
    audio.setNativeAudioRoute(uid, 'am', true, .7, 0);
    expect(old.terminate).toHaveBeenCalledOnce(); expect(Worker.instances).toHaveLength(2);
    old.onmessage!({ data: { type: 'stats', playing: true, bufferedSamples: 99999 } });
    expect(audio.getSdrAudioStatus().filter(row => row.enabled).map(row => row.channel)).toEqual([uid]);
    expect(audio.getSdrAudioStatus().find(row => row.channel === uid)?.bufferedFrames).toBe(0);
    audio.setSdrAudioEnabled(false); release();
  });

  it('adopts autoplay-suspended playback and starts one UID subscription on a later gesture', async () => {
    let context!: SuspendedContext;
    class SuspendedContext extends Context {
      override state = 'suspended';
      constructor() { super(); context = this; }
    }
    vi.stubGlobal('AudioContext', SuspendedContext);
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioMode(1, 'am', source); audio.setSdrAudioEnabled(true);
    const release = audio.claimNativeAudio(directory);
    audio.setNativeAudioRoute('uid-a', 'am', true, .7, 0);
    expect(audio.sdrAudioNeedsGesture()).toBe(true); expect(Worker.instances).toHaveLength(0);
    context.state = 'running'; document.dispatchEvent(new Event('pointerdown'));
    expect(Worker.instances).toHaveLength(1);
    expect(Worker.instances[0].postMessage).toHaveBeenCalledWith(expect.objectContaining({ type: 'init',
      url: expect.stringContaining('receiver_uid=uid-a') }), []);
    audio.setSdrAudioEnabled(false); release();
  });

  it('keeps ended playback latched across adoption until an explicit stream restart', async () => {
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioMode(1, 'am', source); audio.setSdrAudioEnabled(true);
    const old = Worker.instances[0]; old.onmessage!({ data: { type: 'ended', reason: 'receiver stopped' } });
    const release = audio.claimNativeAudio(directory);
    audio.setNativeAudioRoute('uid-a', 'am', true, .7, 0); audio.resumeNativeAudio();
    expect(Worker.instances).toHaveLength(1);
    expect(audio.getSdrAudioStatus().find(row => row.channel === 'uid-a')).toMatchObject({ connectionState: 'stopped', playing: false });
    audio.setNativeAudioRoute('uid-a', 'am', true, .7, 0, true);
    expect(Worker.instances).toHaveLength(2);
    audio.setSdrAudioEnabled(false); release();
  });

  it('keeps main playback off for stopped or non-audio receivers and replaces a reused slot by UID', async () => {
    const audio = await import('../audio/sdrAudio');
    audio.setSdrAudioMode(1, 'ft8', { ...source, available: false }); audio.setSdrAudioEnabled(true);
    expect(Worker.instances).toHaveLength(0);
    audio.setSdrAudioMode(1, 'am', source);
    expect(Worker.instances).toHaveLength(1);
    const old = Worker.instances[0];
    audio.setSdrAudioMode(1, 'am', { ...source, available: false });
    expect(old.terminate).toHaveBeenCalledOnce();
    audio.setSdrAudioMode(1, 'am', { ...source, receiver_uid: 'replacement' });
    expect(Worker.instances).toHaveLength(2);
    expect(Worker.instances[1].postMessage).toHaveBeenCalledWith(expect.objectContaining({ type: 'init',
      url: expect.stringContaining('receiver_uid=replacement') }), []);
    audio.setSdrAudioEnabled(false);
  });
});
