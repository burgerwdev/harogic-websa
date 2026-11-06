import { afterEach, describe, expect, it, vi } from 'vitest';
class Param {
  value = 0;
  cancelScheduledValues = vi.fn(); setValueAtTime = vi.fn((value: number) => { this.value = value; });
  linearRampToValueAtTime = vi.fn((value: number) => { this.value = value; });
}
class Node {
  gain = new Param(); pan = new Param(); threshold = new Param(); knee = new Param(); ratio = new Param(); attack = new Param(); release = new Param();
  connect = vi.fn(); disconnect = vi.fn(); onaudioprocess = null;
}
class Context extends EventTarget {
  static instances: Context[] = []; gains: Node[] = []; pans: Node[] = []; compressors: Node[] = [];
  sampleRate = 48000; currentTime = 0; state = 'running'; destination = new Node(); resume = vi.fn().mockResolvedValue(undefined);
  constructor() { super(); Context.instances.push(this); }
  createGain() { const node = new Node(); this.gains.push(node); return node; }
  createStereoPanner() { const node = new Node(); this.pans.push(node); return node; }
  createDynamicsCompressor() { const node = new Node(); this.compressors.push(node); return node; }
  createScriptProcessor() { return new Node(); }
  createAnalyser() { return new Node(); }
}
class Worker {
  static instances: Worker[] = []; postMessage = vi.fn(); terminate = vi.fn();
  onmessage: ((event: { data: Record<string, unknown> }) => void) | null = null;
  constructor() { Worker.instances.push(this); }
}
afterEach(() => { vi.unstubAllGlobals(); vi.restoreAllMocks(); vi.useRealTimers(); });
describe('local speaker graph', () => {
  it('shares one context/master, ramps gain and pan locally, and prevents legacy duplicate playback', async () => {
    vi.resetModules(); vi.useFakeTimers(); Context.instances = []; Worker.instances = [];
    vi.stubGlobal('AudioContext', Context); vi.stubGlobal('Worker', Worker);
    vi.spyOn(document, 'addEventListener');
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
    for (const [type, listener, options] of vi.mocked(document.addEventListener).mock.calls) document.removeEventListener(type, listener, options);
  });
});
