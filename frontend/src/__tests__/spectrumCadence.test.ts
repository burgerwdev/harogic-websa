import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { TriggerMetadata } from '../ui/triggerEvents';

vi.mock('../audio/sdrAudio', () => ({
  prepareSdrAudioTransition: vi.fn(), setSdrAudioEnabled: vi.fn(), setSdrAudioMode: vi.fn(),
}));

class DisplaySocket {
  static OPEN = 1;
  static CONNECTING = 0;
  readyState = 1;
  binaryType = '';
  onopen: (() => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  send() {}
}
let socket: DisplaySocket;

beforeEach(() => {
  vi.resetModules();
  vi.useFakeTimers();
  const startedAt = Date.now();
  vi.spyOn(performance, 'now').mockImplementation(() => Date.now() - startedAt);
  localStorage.clear();
  document.body.innerHTML = '<canvas id="spectrum"></canvas>';
  vi.stubGlobal('WebSocket', class extends DisplaySocket {
    constructor() { super(); socket = this; }
  });
});
afterEach(() => {
  vi.clearAllTimers();
  vi.restoreAllMocks();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  document.body.replaceChildren();
});

// Dynamic imports intentionally reload the websocket owner and its module-local timer per case.
async function load(mode: 'std' | 'rta' | 'sdr') {
  const S = await import('../core/store');
  const ws = await import('../core/ws');
  const graph = await import('../ui/graphMode');
  const trigger = await import('../ui/triggerState');
  const redraw = await import('../render/redraw');
  ws.connectWS();
  socket.onopen!();
  graph.graphMode.confirm(mode);
  graph.deviceMode.confirm(mode);
  trigger.syncTriggerMode(mode);
  S.setViewMode(mode === 'std' ? 'std' : 'rta');
  S.setRtaMode(mode !== 'std');
  S.setSdrMode(mode === 'sdr');
  S.traces.forEach((trace, i) => { trace.mode = i === 0 ? 'CLEAR_WRITE' : 'OFF'; });
  const draws: { at: number; peak: number }[] = [];
  redraw.setRenderer(() => draws.push({ at: performance.now(),
    peak: mode === 'std' ? S.traces[0].powers![16] : S.rtaData.spec[16] }));
  return { S, ws, graph, trigger, redraw, draws };
}

function deliver(data: ArrayBuffer) { socket.onmessage!({ data } as MessageEvent); }
function rta(peak: number, version = 1, shift = 0, trigger?: TriggerMetadata, saturated = false) {
  const points = 32;
  const metadata = trigger ? new TextEncoder().encode(JSON.stringify(trigger)) : null;
  const size = 24 + points * 12 + 2 + 8;
  const data = new ArrayBuffer(size + (metadata ? 8 + metadata.length : 0));
  const header = new DataView(data);
  new Uint8Array(data, 0, 4).set(metadata ? [82, 84, 65, 50] : [82, 84, 65, 70]);
  header.setUint32(4, version, true);
  header.setUint32(8, points, true);
  header.setUint16(12, 1, true);
  header.setFloat64(16, 1e9 + shift, true);
  const freq = new Float64Array(data, 24, points);
  for (let i = 0; i < points; i++) freq[i] = 1e9 + shift + i * 100;
  const spec = new Float32Array(data, 24 + points * 8, points);
  spec.fill(saturated ? peak : -120); spec[16] = peak;
  header.setFloat64(size - 8, 1e9 + shift + 3200, true);
  if (metadata) {
    header.setUint32(size, 0x47495254, true);
    header.setUint32(size + 4, metadata.length, true);
    new Uint8Array(data, size + 8).set(metadata);
  }
  deliver(data);
}
function sweep(kind: 'FREQ' | 'POWR', peak = -60, version = 1, shift = 0, extraSpan = 0) {
  const data = new ArrayBuffer(16 + 32 * (kind === 'FREQ' ? 8 : 4));
  const header = new DataView(data);
  new Uint8Array(data, 0, 4).set([...kind].map(char => char.charCodeAt(0)));
  header.setUint32(4, version, true); header.setUint32(8, 32, true);
  if (kind === 'FREQ') {
    const freq = new Float64Array(data, 16);
    for (let i = 0; i < 32; i++) freq[i] = 1e9 + shift + i * (100 + extraSpan / 31);
  } else {
    const powers = new Float32Array(data, 16); powers.fill(-120); powers[16] = peak;
  }
  deliver(data);
}

describe('spectrum arrival cadence', () => {
  it('retains SWP zoom after a pan settles onto a realigned native grid, but resets it on a span edit', async () => {
    await load('std');
    // These imports must share load()'s fresh graph after vi.resetModules().
    const viewport = await import('../ui/spectrumViewport');
    const axis = await import('../ui/axisDrag');
    sweep('FREQ');
    sweep('POWR');
    viewport.zoomEnabled.set(true);
    viewport.setView({ lo: 1e9 + 1000, hi: 1e9 + 1800 }, 1);
    axis.axisDragStart('x', 400, 450, { x: 0, y: 0, w: 800, h: 400 });
    axis.axisDragMove(600, 450);
    axis.axisDragEnd();
    expect(axis.previewCaptureWindow()).toEqual({ lo: 1e9 - 200, hi: 1e9 + 2900 });
    expect(viewport.isZoomed()).toBe(true);
    // A same-bandwidth SDK retune changes the sampled endpoint by one native bin.
    sweep('FREQ', -60, 2, -200, 100);
    sweep('POWR', -50, 2);
    vi.advanceTimersByTime(100);
    expect(viewport.isZoomed()).toBe(true);
    expect(viewport.getView()).toEqual({ lo: 1e9 + 800, hi: 1e9 + 1600 });
    expect(axis.previewCaptureWindow()).toBeNull();
    // Without a pan in flight, a genuine capture-width edit still drops the view.
    sweep('FREQ', -60, 3, -200, 3200);
    sweep('POWR', -40, 3);
    expect(viewport.isZoomed()).toBe(false);
  });

  it.each(['rta', 'std', 'sdr'] as const)('presents the final %s burst frame without another arrival', async mode => {
    const { S, draws } = await load(mode);
    if (mode === 'std') sweep('FREQ');
    const feed = (peak: number) => mode === 'std' ? sweep('POWR', peak) : rta(peak);
    S.traces[0].mode = 'MAX_HOLD';
    feed(-100);
    vi.advanceTimersByTime(10); feed(-20);
    vi.advanceTimersByTime(10); feed(-90);
    expect(draws.at(-1)!.peak).toBe(-100);
    vi.advanceTimersByTime(20);
    // SWP processes every accepted frame; RTA and SDR coalesce superseded packets.
    expect(mode === 'std' ? S.traces[0].powers![16] : S.rtaDisplays[0]![16]).toBe(mode === 'std' ? -20 : -90);
    expect(draws.at(-1)!.peak).toBe(mode === 'std' ? -20 : -90);
    S.traces[0].mode = 'CLEAR_WRITE';
    feed(-60);
    vi.advanceTimersByTime(100);
    expect(draws.at(-1)!.peak).toBe(-60);
    const count = draws.length;
    vi.advanceTimersByTime(1000);
    expect(draws).toHaveLength(count);
  });

  it.each(['rta', 'sdr'] as const)('draws a new %s axis immediately without restoring queued old-axis data', async mode => {
    const { S, draws } = await load(mode);
    rta(-100);
    vi.advanceTimersByTime(10); rta(-80);
    vi.advanceTimersByTime(1); rta(-60, 2, 10000);
    expect(draws.map(draw => draw.at)).toEqual([0, 11]);
    expect(S.rtaData.ver).toBe(2);
    vi.advanceTimersByTime(100);
    expect(draws).toHaveLength(2);
    expect(S.rtaData.spec[16]).toBe(-60);
  });

  it('processes armed first and completed captures immediately, cancelling normal pending work', async () => {
    const { S, draws, redraw } = await load('rta');
    const events = await import('../ui/triggerEvents');
    events.onTriggerHit(redraw.requestRender);
    rta(-100);
    vi.advanceTimersByTime(10); rta(-80);
    events.confirmTriggerMetadata({ source: 'bus', generation: 1, valid: true });
    expect(events.beginHardwareCapture()).toBe(true);
    vi.advanceTimersByTime(1);
    rta(-60, 1, 0, { source: 'level', generation: 2, valid: true, frames: 1, complete: false });
    vi.advanceTimersByTime(1);
    rta(-40, 1, 0, { source: 'level', generation: 2, valid: true, frames: 2, complete: true });
    expect(draws.map(draw => draw.at)).toEqual([0, 11, 12]);
    expect(S.trigHit).toBe(true);
    vi.advanceTimersByTime(100);
    expect(draws).toHaveLength(3);
    expect(S.rtaData.spec[16]).toBe(-40);
  });

  it('rejects queued freerun data after a newer external capture STATUS arms RTA', async () => {
    const { S, ws, draws } = await load('rta');
    rta(-100, 1, 0, { source: 'bus', generation: 1, valid: true });
    vi.advanceTimersByTime(10);
    rta(-80, 1, 0, { source: 'bus', generation: 1, valid: true });
    ws.updateStatus({ mode: 'rta', connected: true, center: 1e9, span: 3200, ref: 0,
      req: { swp: {}, rta: { trigger_actual: { source: 'level', generation: 2, valid: true } } }, actual: {} });
    expect(S.trigArmed).toBe(true);
    const drawsAfterStatus = draws.length;
    vi.advanceTimersByTime(100);
    expect(draws).toHaveLength(drawsAfterStatus);
    expect(S.rtaData.spec[16]).toBe(-100);
    rta(-60, 1, 0, { source: 'level', generation: 2, valid: true, frames: 1, complete: false });
    expect(S.rtaData.spec[16]).toBe(-60);
    expect(draws.at(-1)!.at).toBe(110);
  });

  it('paces rejected settling packets before eventually displaying persistent bad data', async () => {
    const { S, draws } = await load('rta');
    rta(-100);
    for (let i = 0; i < 100; i++) {
      vi.advanceTimersByTime(1);
      rta(0, 1, 0, undefined, true);
    }
    expect(draws).toHaveLength(1);
    expect(S.rtaData.spec[16]).toBe(-100);
    expect(S.badData).toBe(false);
    for (let i = 100; i < 400; i++) {
      vi.advanceTimersByTime(1);
      rta(0, 1, 0, undefined, true);
    }
    expect(S.badData).toBe(true);
    expect(S.rtaData.spec[16]).toBe(0);
    expect(draws[1].at).toBeGreaterThanOrEqual(300);
  });

  it.each([
    ['rta', 'mode'], ['rta', 'disconnect'], ['rta', 'retune'],
    ['sdr', 'mode'], ['sdr', 'disconnect'], ['sdr', 'retune'],
  ] as const)('cancels deferred %s work on %s', async (mode, boundary) => {
    const { S, draws } = await load(mode);
    rta(-100);
    vi.advanceTimersByTime(10); rta(-80);
    if (boundary === 'disconnect') socket.onclose!();
    else {
      const { send } = await import('../core/wsSend');
      send(boundary === 'mode' ? { cmd: 'SET_MODE', mode: 'std' }
        : { cmd: mode === 'sdr' ? 'SET_SDR' : 'SET_RTA', center: 2e9 });
    }
    // SET_* also withdraws Auto feedback synchronously; that UI repaint is intentional.
    const drawsAfterBoundary = draws.length;
    vi.advanceTimersByTime(100);
    expect(draws).toHaveLength(drawsAfterBoundary);
    expect(S.rtaData.spec[16]).toBe(-100);
  });

  it.each(['center', 'decimate', 'version'] as const)('rejects deferred SDR work after its capture %s changes', async boundary => {
    const { S, draws } = await load('sdr');
    // Static imports would retain state from before resetModules().
    const { sdrCenterHz, sdrDecimate } = await import('../ui/sdrState');
    rta(-100);
    vi.advanceTimersByTime(10); rta(-80);
    if (boundary === 'center') sdrCenterHz.set(sdrCenterHz.get() + 10000);
    else if (boundary === 'decimate') sdrDecimate.set(sdrDecimate.get() * 2);
    else S.setConfigVersion(S.configVersion + 1);
    const drawsAfterBoundary = draws.length;
    vi.advanceTimersByTime(100);
    expect(draws).toHaveLength(drawsAfterBoundary);
    expect(S.rtaData.spec[16]).toBe(-100);
    rta(-60, 2, 10000);
    expect(draws.at(-1)!.peak).toBe(-60);
  });
});
