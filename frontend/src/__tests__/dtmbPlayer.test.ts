import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { dtmbStatus, mountDtmbPlayer, type DtmbPlayer } from '../ui/dtmbPlayer';
import { setLang } from '../core/i18n';
import { streamRenderInterval } from '../render/redraw';

const status = (overrides = {}) => dtmbStatus({ type: 'dtmb', state: 'receiving', streamId: 'a'.repeat(32), streamReady: true,
  header: 945, cnDb: 18, merDb: 12, mapping: '16QAM', codeRate: .4,
  programs: [{ id: 1, name: '<频道一>', provider: 'TV', video: 'AVS+', audio: 'MPEG audio', scrambled: false },
    { id: 2, name: '频道二', provider: 'TV', video: 'H.264', audio: 'AAC', scrambled: false }], ...overrides })!;

class Source extends EventTarget {
  static isTypeSupported = () => true;
  addSourceBuffer() { return new Buffer(); }
}
class Buffer extends EventTarget {
  buffered = { length: 0 };
  appendBuffer() { queueMicrotask(() => this.dispatchEvent(new Event('updateend'))); }
}

describe('DTMB television player', () => {
  let host: HTMLElement, player: DtmbPlayer;
  let signals: AbortSignal[];
  beforeEach(() => {
    setLang('zh'); signals = [];
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue();
    vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(() => {});
    vi.spyOn(HTMLMediaElement.prototype, 'load').mockImplementation(() => {});
    vi.stubGlobal('MediaSource', Source);
    vi.stubGlobal('fetch', vi.fn(async (_url, init) => {
      const signal: AbortSignal = init.signal; signals.push(signal);
      const body = new ReadableStream<Uint8Array>({ start(controller) {
        controller.enqueue(new Uint8Array([1, 2, 3]));
        signal.addEventListener('abort', () => controller.error(new Error('aborted')), { once: true });
      } });
      return { ok: true, body };
    }));
    vi.stubGlobal('URL', class extends URL {
      static createObjectURL = vi.fn((source: Blob | MediaSource) => {
        if (source instanceof EventTarget) queueMicrotask(() => source.dispatchEvent(new Event('sourceopen')));
        return 'blob:dtmb';
      });
      static revokeObjectURL = vi.fn();
    });
    host = document.createElement('section'); document.body.append(host); player = mountDtmbPlayer(host);
  });
  afterEach(() => { player.dispose(); host.remove(); vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.useRealTimers(); setLang('en'); });
  const button = () => host.querySelector<HTMLButtonElement>('button')!;

  it('keeps signal identification separate from playback and renders broadcast text safely', () => {
    player.update(status({ state: 'correcting', streamReady: false }), true);
    expect(button().disabled).toBe(true);
    expect(host.textContent).toContain('等待纠错');
    expect(host.querySelector('select')!.textContent).toContain('<频道一>');
    expect(host.querySelector('频道一')).toBeNull();
    player.update(status(), true);
    expect(button().disabled).toBe(false);
    expect(fetch).not.toHaveBeenCalled();
    expect(host.textContent).toContain('MER 12.0 dB');
  });

  it('changes programs without resetting the video on telemetry, and releases playback when hidden', async () => {
    player.update(status(), true); button().click();
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
    expect(streamRenderInterval()).toBe(500);
    const video = host.querySelector('video');
    player.update(status({ merDb: 13 }), true);
    expect(host.querySelector('video')).toBe(video);
    expect(signals[0].aborted).toBe(false);
    const select = host.querySelector('select')!; select.value = '2'; select.dispatchEvent(new Event('change'));
    await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));
    expect(signals[0].aborted).toBe(true);
    expect(vi.mocked(fetch).mock.calls[1][0]).toContain('/2/live.mp4');
    player.update(status(), false);
    expect(streamRenderInterval()).toBeLessThan(200);
    expect(signals[1].aborted).toBe(true);
    expect(video?.getAttribute('src')).toBeNull();
    expect(URL.revokeObjectURL).toHaveBeenCalled();
  });

  it('abandons the previous reception when the stream identity changes', async () => {
    player.update(status(), true); button().click();
    await vi.waitFor(() => expect(signals).toHaveLength(1));
    player.update(status({ streamId: 'b'.repeat(32) }), true);
    expect(signals[0].aborted).toBe(true);
    expect(button().textContent).toBe('播放');
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it('reconnects an active program after moving documents without starting an idle player', async () => {
    player.update(status(), true);
    player.beforeMove(); player.afterMove();
    expect(fetch).not.toHaveBeenCalled();
    button().click();
    await vi.waitFor(() => expect(signals).toHaveLength(1));
    player.beforeMove();
    expect(signals[0].aborted).toBe(true);
    const popup = document.implementation.createHTMLDocument();
    popup.body.append(host);
    player.afterMove();
    await vi.waitFor(() => expect(signals).toHaveLength(2));
    expect(vi.mocked(fetch).mock.calls[1][0]).toBe(vi.mocked(fetch).mock.calls[0][0]);
    expect(button().textContent).toBe('停止');
  });

  it('releases a rejected media stream and presents a localized playback error', async () => {
    vi.spyOn(Buffer.prototype, 'appendBuffer').mockImplementation(() => { throw new DOMException('Internal SourceBuffer details', 'InvalidStateError'); });
    player.update(status(), true); button().click();
    await vi.waitFor(() => expect(signals[0]?.aborted).toBe(true));
    expect(host.textContent).not.toContain('Internal SourceBuffer');
    expect(host.querySelector('video')?.getAttribute('src')).toBeNull();
    expect(button().textContent).toBe('播放');
    expect(host.querySelector('.dtmb-message')?.textContent).toBeTruthy();
  });

  it('rejects unsafe identities and does not offer encrypted or unsupported media', () => {
    player.update(status({ streamId: '../../file' }), true);
    expect(button().disabled).toBe(true);
    const next = status(); next.programs.forEach(p => { p.scrambled = true; });
    player.update(next, true); expect(button().disabled).toBe(true);
    player.update(status({ playbackReason: 'Install player' }), true);
    expect(button().disabled).toBe(true); expect(host.textContent).toContain('Install player');
  });

  it('expires an on-air status when transport packets stop advancing', () => {
    vi.useFakeTimers(); player.dispose(); player = mountDtmbPlayer(host);
    const now = vi.spyOn(performance, 'now').mockReturnValue(1000);
    player.update(status({ transportPackets: 100 }), true);
    expect(button().disabled).toBe(false);
    now.mockReturnValue(4000);
    vi.advanceTimersByTime(3000);
    player.update(status({ transportPackets: 100 }), true);
    expect(button().disabled).toBe(true);
    expect(host.textContent).toContain('等待纠错');
    player.update(status({ transportPackets: 101 }), true);
    expect(button().disabled).toBe(false);
  });
});
