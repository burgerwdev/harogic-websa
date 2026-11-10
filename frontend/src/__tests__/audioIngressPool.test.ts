import { afterEach, expect, it, vi } from 'vitest';

interface Item { clientId: number; message: unknown }
class IngressWorker {
  static instances: IngressWorker[] = [];
  onerror: ((event: ErrorEvent) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  terminate = vi.fn();
  constructor() { IngressWorker.instances.push(this); }
  postMessage = vi.fn((message: { items: Item[] }) => {
    this.onmessage?.({ data: { type: 'source-events', items: message.items } } as MessageEvent);
  });
}
afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); });

it('batches 16 isolated source routes and releases the thread after the last receiver', async () => {
  vi.resetModules(); IngressWorker.instances = []; vi.stubGlobal('Worker', IngressWorker);
  const { audioIngressClient } = await import('../audio/audioIngressPool');
  const clients = Array.from({ length: 16 }, () => audioIngressClient()), received = clients.map(() => vi.fn());
  clients.forEach((client, i) => { client.onmessage = received[i]; client.postMessage({ receiver: i }); });
  await Promise.resolve();
  expect(IngressWorker.instances).toHaveLength(1); expect(IngressWorker.instances[0].postMessage).toHaveBeenCalledOnce();
  received.forEach((callback, i) => { expect(callback).toHaveBeenCalledOnce(); expect(callback.mock.calls[0][0].data).toEqual({ receiver: i }); });
  clients[0].terminate(); clients[0].postMessage({ receiver: 'retired' });
  expect(received[0]).toHaveBeenCalledOnce(); expect(IngressWorker.instances[0].terminate).not.toHaveBeenCalled();
  clients[1].postMessage({ receiver: 'continues' }); await Promise.resolve(); expect(received[1]).toHaveBeenCalledTimes(2);
  clients.slice(1).forEach(client => client.terminate());
  expect(IngressWorker.instances[0].terminate).toHaveBeenCalledOnce();
  const restarted = audioIngressClient(); expect(IngressWorker.instances).toHaveLength(2); restarted.terminate();
});

it('coalesces worker deliveries and does not revive a disposed source on a late acknowledgement', async () => {
  vi.useFakeTimers();
  const { serveSources } = await import('../core/sourceWorkerHost');
  const root = { onmessage: null as ((event: MessageEvent) => void) | null, postMessage: vi.fn() };
  vi.stubGlobal('self', root); const created = vi.fn();
  serveSources(host => {
    created(); host.onmessage = event => {
      if (event.data.type === 'dispose') host.close?.(); else host.postMessage(event.data);
    };
  });
  const send = (items: Item[]): void => root.onmessage!({ data: { type: 'source-commands', items } } as MessageEvent);
  send(Array.from({ length: 16 }, (_, i) => ({ clientId: i + 1, message: { type: 'start', receiver: i } })));
  expect(root.postMessage).not.toHaveBeenCalled(); await vi.advanceTimersByTimeAsync(40);
  expect(root.postMessage).toHaveBeenCalledOnce(); expect(root.postMessage.mock.calls[0][0].items).toHaveLength(16);
  send([{ clientId: 1, message: { type: 'dispose' } }, { clientId: 1, message: { type: 'ack' } }, { clientId: 2, message: { type: 'ack' } }]);
  await vi.advanceTimersByTimeAsync(40); expect(created).toHaveBeenCalledTimes(16);
  expect(root.postMessage.mock.calls[1][0].items.map((item: Item) => item.clientId)).toEqual([2]);
});

it('does not transfer a queued initialization after its source has been retired', async () => {
  const { sourceWorkerPool } = await import('../core/sourceWorkerPool');
  const worker = new IngressWorker(), acquire = sourceWorkerPool(() => worker as unknown as Worker);
  const retiring = acquire(), continuing = acquire();
  retiring.postMessage({ type: 'init', url: 'retired' }, [{} as Transferable]);
  retiring.terminate(); continuing.postMessage({ type: 'start', url: 'continuing' });
  await Promise.resolve();
  const [batch, transfer] = worker.postMessage.mock.calls[0] as unknown as [{ items: Item[] }, Transferable[]];
  expect(batch.items.map(item => item.message)).toEqual([{ type: 'dispose' }, { type: 'start', url: 'continuing' }]);
  expect(transfer).toEqual([]); continuing.terminate();
});

it('isolates a failed transferable from another receiver in the same command batch', async () => {
  const { sourceWorkerPool } = await import('../core/sourceWorkerPool');
  const worker = new IngressWorker(), acquire = sourceWorkerPool(() => worker as unknown as Worker);
  worker.postMessage.mockImplementation(message => {
    if (message.items.some(item => (item.message as { bad?: boolean }).bad)) throw new Error('DataCloneError');
    worker.onmessage?.({ data: { type: 'source-events', items: message.items } } as MessageEvent);
  });
  const broken = acquire(), healthy = acquire(), error = vi.fn(), received = vi.fn();
  broken.onerror = error; healthy.onmessage = received; healthy.onerror = vi.fn();
  broken.postMessage({ bad: true }); healthy.postMessage({ type: 'start' });
  await Promise.resolve();
  expect(error).toHaveBeenCalledOnce(); expect(healthy.onerror).not.toHaveBeenCalled();
  expect(received).toHaveBeenCalledOnce(); expect(received.mock.calls[0][0].data).toEqual({ type: 'start' });
  broken.terminate(); healthy.terminate();
});
