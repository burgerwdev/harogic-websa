// WS send (separate module to avoid circular dependency)
let ws: WebSocket | null = null;
export function setWS(w: WebSocket | null) { ws = w; }

/** Observe commands that reached the socket, before any later STATUS can acknowledge them. */
const commandListeners = new Set<(command: Readonly<Record<string, unknown>>) => void>();
export function onCommandSent(listener: (command: Readonly<Record<string, unknown>>) => void): void {
  commandListeners.add(listener);
}

export function send(obj: object) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(obj));
    for (const listener of commandListeners) listener(obj as Record<string, unknown>);
  }
}
