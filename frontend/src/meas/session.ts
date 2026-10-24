// Measurement results belong to a successful parameter application, not arrival time.
type Kind = 'pnm' | 'harm';
const sessions: Record<Kind, { requested: string; confirmed: boolean } | null> = { pnm: null, harm: null };
let sequence = 0;
// getRandomValues also works on the device's plain HTTP LAN origin.
const connectionToken = Array.from(crypto.getRandomValues(new Uint32Array(4)), value => value.toString(16)).join('-');

export function beginMeasurementSession(kind: Kind): string {
  const id = `${connectionToken}-${kind}-${++sequence}`;
  sessions[kind] = { requested: id, confirmed: false };
  return id;
}

export function confirmMeasurementSession(message: { response_to?: string; session_id?: string }): void {
  const kind = message.response_to === 'SET_PNM' ? 'pnm' : message.response_to === 'SET_HARM' ? 'harm' : null;
  if (!kind) return;
  const session = sessions[kind];
  if (session && message.session_id === session.requested) session.confirmed = true;
}

export function acceptsMeasurementResult(kind: Kind, sessionId: unknown): boolean {
  const session = sessions[kind];
  return !!session?.confirmed && sessionId === session.requested;
}

export function endMeasurementSessions(): void {
  sessions.pnm = null;
  sessions.harm = null;
}
