// WS send (separate module to avoid circular dependency)
let ws: WebSocket | null = null;
let requestSequence = 0;
export function setWS(w: WebSocket | null) { ws = w; if (!w) publishCommandReply({ cmd: 'DISCONNECTED' }); }

const replyListeners = new Set<(reply: Readonly<Record<string, unknown>>) => void>();
export function onCommandReply(listener: (reply: Readonly<Record<string, unknown>>) => void): () => void {
  replyListeners.add(listener); return () => { replyListeners.delete(listener); };
}
export function publishCommandReply(reply: Readonly<Record<string, unknown>>): void {
  for (const listener of replyListeners) listener(reply);
}

/** Observe commands that reached the socket, before any later STATUS can acknowledge them. */
const commandListeners = new Set<(command: Readonly<Record<string, unknown>>) => void>();
export function onCommandSent(listener: (command: Readonly<Record<string, unknown>>) => void): () => void {
  commandListeners.add(listener);
  return () => { commandListeners.delete(listener); };
}

export function send(obj: object) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    const command = obj as Record<string, unknown>;
    const identified = command.request_id === undefined && command.cmd !== 'STATUS'
      ? { ...command, request_id: `ws-command-${++requestSequence}` } : command;
    ws.send(JSON.stringify(identified));
    for (const listener of commandListeners) listener(identified);
  }
}
