export interface SourceWorkerClient {
  onmessage: ((event: MessageEvent) => void) | null;
  onerror: ((event: ErrorEvent) => void) | null;
  postMessage(message: unknown, transfer?: Transferable[]): void;
  terminate(): void;
}
interface Command { clientId: number; message: unknown; transfer: Transferable[] }
/** One batched command/event bus per worker, with independently owned sources. */
export function sourceWorkerPool(create: () => Worker): () => SourceWorkerClient {
  interface Pool {
    worker: Worker; clients: Map<number, SourceWorkerClient>; sequence: number;
    commands: Command[]; scheduled: boolean;
  }
  let pool: Pool | undefined;
  const flush = (owner: Pool): void => {
    owner.scheduled = false;
    if (!owner.commands.length) return;
    const commands = owner.commands.splice(0).filter(item => owner.clients.has(item.clientId)
      || (item.message as { type?: string })?.type === 'dispose');
    if (!commands.length) return;
    const wire = (item: Command): { clientId: number; message: unknown } => ({ clientId: item.clientId, message: item.message });
    try { owner.worker.postMessage({ type: 'source-commands', items: commands.map(wire) }, commands.flatMap(item => item.transfer)); }
    catch {
      // A bad transferable must not reject another receiver's command.
      for (const item of commands) try {
        owner.worker.postMessage({ type: 'source-commands', items: [wire(item)] }, item.transfer);
      } catch (error) {
        owner.clients.get(item.clientId)?.onerror?.(new ErrorEvent('error', { message: String(error) }));
      }
    }
  };
  const send = (owner: Pool, clientId: number, message: unknown, transfer: Transferable[] = []): void => {
    owner.commands.push({ clientId, message, transfer });
    if (!owner.scheduled) { owner.scheduled = true; queueMicrotask(() => flush(owner)); }
  };
  return () => {
    if (!pool) {
      const owner: Pool = { worker: create(), clients: new Map(), sequence: 0, commands: [], scheduled: false };
      pool = owner;
      owner.worker.onerror = event => { for (const client of [...owner.clients.values()]) client.onerror?.(event); };
      owner.worker.onmessage = event => {
        if (event.data?.type !== 'source-events' || !Array.isArray(event.data.items)) return;
        for (const item of event.data.items) owner.clients.get(item.clientId)?.onmessage?.({ data: item.message } as MessageEvent);
      };
    }
    const owner = pool, id = ++owner.sequence;
    let disposed = false;
    const client: SourceWorkerClient = {
      onmessage: null, onerror: null,
      postMessage: (message, transfer = []) => { if (!disposed) send(owner, id, message, transfer); },
      terminate: () => {
        if (disposed) return; disposed = true; owner.clients.delete(id);
        send(owner, id, { type: 'dispose' });
        if (!owner.clients.size) { flush(owner); owner.worker.terminate(); if (pool === owner) pool = undefined; }
      },
    };
    owner.clients.set(id, client);
    return client;
  };
}
