import { afterEach, describe, expect, it, vi } from 'vitest';
import { ScanUpdates } from '../ui/sdrScanUpdates';

class Source {
  static OPEN = 1;
  static instances: Source[] = [];
  readyState = 0;
  onopen: (() => void) | null = null;
  onmessage: (() => void) | null = null;
  close = vi.fn(() => { this.readyState = 2; });
  constructor(readonly url: string) { Source.instances.push(this); }
}
afterEach(() => { vi.unstubAllGlobals(); Source.instances = []; });
describe('scan change notifications', () => {
  it('pushes immediately, falls back while disconnected, and releases hidden observers', () => {
    vi.stubGlobal('EventSource', Source);
    const refresh = vi.fn(), updates = new ScanUpdates(refresh);
    updates.setVisible(true); updates.setVisible(true);
    expect(Source.instances).toHaveLength(1);
    const source = Source.instances[0];
    expect(source.url).toBe('/api/sdr/scan/events');
    updates.poll(); expect(refresh).toHaveBeenCalledTimes(1);
    source.readyState = Source.OPEN; source.onopen?.(); source.onmessage?.();
    updates.poll(); expect(refresh).toHaveBeenCalledTimes(3);
    updates.poll(true); expect(refresh).toHaveBeenCalledTimes(4);
    source.readyState = 0; updates.poll(); expect(refresh).toHaveBeenCalledTimes(5);
    updates.setVisible(false); expect(source.close).toHaveBeenCalledOnce();
    source.onmessage?.(); expect(refresh).toHaveBeenCalledTimes(5);
    updates.setVisible(true); expect(Source.instances).toHaveLength(2);
    updates.close(); expect(Source.instances[1].close).toHaveBeenCalledOnce();
  });
  it('keeps polling when EventSource is unavailable', () => {
    vi.stubGlobal('EventSource', undefined);
    const refresh = vi.fn(), updates = new ScanUpdates(refresh);
    updates.setVisible(true); updates.poll(); expect(refresh).toHaveBeenCalledOnce();
  });
});
