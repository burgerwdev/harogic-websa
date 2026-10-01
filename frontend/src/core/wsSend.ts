// WS send (separate module to avoid circular dependency)
let ws: WebSocket | null = null;
export function setWS(w: WebSocket | null) { ws = w; }
export function send(obj: object) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj));
}
