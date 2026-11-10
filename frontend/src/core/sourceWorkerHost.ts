export interface SourceWorkerHost {
  postMessage(message: unknown, transfer?: Transferable[]): void;
  onmessage: ((event: MessageEvent) => void) | null;
  close?(): void;
}
// UI meters and status need at most 25 deliveries/second. PCM bypasses this
// bus and flows directly to the AudioWorklet with its own credits and clock.
const UI_DELIVERY_MS = 40;
/** Source parsers/queues are isolated; UI deliveries share one bounded-cadence bus. */
export function serveSources(create: (host: SourceWorkerHost) => void): void {
  const root = self as unknown as SourceWorkerHost;
  const sources = new Map<number, SourceWorkerHost>();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const events: { clientId: number; message: unknown }[] = [];
  const transfers: Transferable[] = [];
  const deliver = (): void => {
    timer = undefined;
    root.postMessage({ type: 'source-events', items: events.splice(0) }, transfers.splice(0));
  };
  root.onmessage = event => {
    const data = event.data;
    if (data?.type !== 'source-commands') {
      // Preserve the standalone ingress contract for legacy consumers.
      create(root); root.onmessage?.(event); return;
    }
    for (const { clientId, message } of data.items) {
      let source = sources.get(clientId);
      if (!source) {
        if (message.type !== 'init' && message.type !== 'start') continue;
        source = {
          onmessage: null, close: () => { sources.delete(clientId); },
          postMessage: (message, transfer = []) => {
            events.push({ clientId, message }); transfers.push(...transfer);
            timer ??= setTimeout(deliver, UI_DELIVERY_MS);
          },
        };
        sources.set(clientId, source); create(source);
      }
      source.onmessage?.({ data: message } as MessageEvent);
    }
  };
}
