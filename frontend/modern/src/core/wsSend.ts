// WS 发送(独立模块, 避免循环依赖)
let ws: WebSocket | null = null;
export function setWS(w: WebSocket | null) { ws = w; }
export function send(obj: object) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj));
}
