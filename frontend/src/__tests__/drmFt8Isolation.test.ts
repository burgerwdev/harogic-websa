import { afterEach, describe, expect, it, vi } from 'vitest';
import { clearFt8Spots, ft8Spots } from '../sdr/ft8Log';

class FakeWorker {
  static instances: FakeWorker[] = [];
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  posted: Record<string, unknown>[] = [];
  constructor() { FakeWorker.instances.push(this); }
  postMessage(message: Record<string, unknown>): void { this.posted.push(message); }
  terminate(): void {}
}

const params = (mode: string) => ({ mode, fsIn: 48828, outRate: 48000, ifBw: 10000, pitch: 700, deemphUs: -1 });

afterEach(() => {
  vi.unstubAllGlobals();
  FakeWorker.instances = [];
  clearFt8Spots();
});

describe('digital decode message isolation', () => {
  it('discards a late FT8 decode after the page switches to DRM', async () => {
    vi.resetModules();
    vi.stubGlobal('Worker', FakeWorker);
    const { configureSdrPipeline, setSdrIqEnabled } = await import('../sdr/iqStream');
    clearFt8Spots();
    setSdrIqEnabled(true);
    configureSdrPipeline(params('ft8'));
    const worker = FakeWorker.instances[0];
    configureSdrPipeline(params('drm'));
    worker.onmessage?.({ data: { type: 'ft8', text: 'station: CNR-1', centerHz: 13_835_000, frequencyHz: 0 } });
    expect(ft8Spots()).toEqual([]);
  });

  it('clears the previous DRM station and constellation when the worker reports a retune', async () => {
    vi.resetModules();
    vi.stubGlobal('Worker', FakeWorker);
    const stream = await import('../sdr/iqStream');
    const log = await import('../sdr/drmLog');
    stream.setSdrIqEnabled(true);
    stream.configureSdrPipeline(params('drm'));
    const worker = FakeWorker.instances[0];
    log.setDrmDecode(['locked: B, 10 kHz', 'station: CNR-1', 'FAC SNR 16 dB'], 16);
    log.setDrmConstellation([{ re: 0.7, im: 0.7 }]);
    worker.onmessage?.({ data: { type: 'drm-clear' } });
    expect(log.drmStatus()).toEqual({ lines: [], snrDb: null, constellation: [] });
  });

  it('emits a DRM clear on a retune marker or lost IQ frame', async () => {
    vi.resetModules();
    class FakeSocket {
      static instances: FakeSocket[] = [];
      static OPEN = 1;
      static CONNECTING = 0;
      readyState = FakeSocket.OPEN;
      binaryType = '';
      onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
      onclose: (() => void) | null = null;
      onerror: (() => void) | null = null;
      constructor(public url: string) { FakeSocket.instances.push(this); }
      close(): void { this.onclose?.(); }
    }
    vi.stubGlobal('WebSocket', FakeSocket);
    vi.stubGlobal('Worker', undefined);
    const scope = { postMessage: vi.fn(), onmessage: null as null | ((event: { data: unknown }) => void) };
    vi.stubGlobal('self', scope);
    await import('../sdr/iqWorker');
    scope.onmessage?.({ data: { type: 'init', url: 'ws://test/iq' } });
    scope.onmessage?.({ data: { type: 'configure', params: params('drm') } });
    const emitIq = (seq: number) => {
      const data = new ArrayBuffer(32);
      const view = new DataView(data);
      for (const [i, c] of [...'IQBF'].entries()) view.setUint8(i, c.charCodeAt(0));
      view.setUint32(4, 1, true);
      view.setUint32(8, seq, true);
      view.setFloat64(16, 48_000, true);
      view.setFloat64(24, 13_835_000, true);
      FakeSocket.instances[0].onmessage?.({ data });
    };
    emitIq(1);
    expect(scope.postMessage.mock.calls.some(([msg]) => msg.type === 'drm-clear')).toBe(false);
    emitIq(0);
    expect(scope.postMessage.mock.calls.filter(([msg]) => msg.type === 'drm-clear')).toHaveLength(1);
    emitIq(1);
    emitIq(3);
    expect(scope.postMessage.mock.calls.filter(([msg]) => msg.type === 'drm-clear')).toHaveLength(2);
  });

  it('opens the FT8 IQ socket only while FT8 is active', async () => {
    vi.resetModules();
    class FakeSocket {
      static instances: FakeSocket[] = [];
      static OPEN = 1;
      static CONNECTING = 0;
      readyState = FakeSocket.OPEN;
      binaryType = '';
      onmessage: ((event: { data: unknown }) => void) | null = null;
      onclose: (() => void) | null = null;
      onerror: (() => void) | null = null;
      constructor(public url: string) { FakeSocket.instances.push(this); }
      close(): void { this.readyState = 3; this.onclose?.(); }
    }
    vi.stubGlobal('WebSocket', FakeSocket);
    const scope = { postMessage: vi.fn(), onmessage: null as null | ((event: { data: unknown }) => void) };
    vi.stubGlobal('self', scope);
    await import('../sdr/ft8Worker');
    scope.onmessage?.({ data: { type: 'init', url: 'ws://test/iq', port: {} } });
    expect(FakeSocket.instances).toHaveLength(0);
    scope.onmessage?.({ data: { type: 'active', enabled: true } });
    expect(FakeSocket.instances).toHaveLength(1);
    scope.onmessage?.({ data: { type: 'active', enabled: false } });
    expect(FakeSocket.instances[0].readyState).toBe(3);
    scope.onmessage?.({ data: { type: 'active', enabled: true } });
    expect(FakeSocket.instances).toHaveLength(2);
  });
});
