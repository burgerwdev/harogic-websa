import { onLangChange, t } from '../core/i18n';
import { send } from '../core/wsSend';
import { currentGraphMode, deviceMode, GRAPH_MODE_TTL_MS, type GraphMode } from './graphMode';

type ServerState = 'stopped' | 'starting' | 'listening' | 'stopping' | 'failed';
type ServerAction = 'start' | 'stop' | 'restart' | 'configure';
export interface ServerLog { seq: number; time_ms: number; source: string; text: string }
export interface ServerStatus {
  state: ServerState; bind_address: string; port: number; pid: number | null;
  uptime_seconds: number; exit_code: number | null; error: string | null;
  logs: ServerLog[]; log_capacity: number; driver_available: boolean; dependency_error: string | null;
  graph_mode?: GraphMode;
}
let status: ServerStatus | null = null;
let connected = false;
let visible = false;
let dirty = false;
let paused = false;
let follow = true;
let clearedThrough = -1;
let lastBackendSeq = -1;
let displayed: ServerLog[] = [];
let localError = '';
let sequence = 0;
let pending: { id: string; cmd: string; target?: string; action?: ServerAction; timer: number } | null = null;
let returnMode: GraphMode | null = null;
let leave: ((mode: GraphMode) => void) | null = null;
let transitionFrame: number | null = null;
let transitionCover: HTMLElement | null = null;
let matrixEdge: HTMLElement | null = null;
let matrixAtlas: HTMLCanvasElement | null = null;
let transitionTimer: number | null = null;
let readyTimer: number | null = null;
let deferredCommand: (() => void) | null = null;
let transitionTarget: string | null = null;
let transitionRevealing = false;
let transitionGeneration = 0;
const el = (id: string) => document.getElementById(id);
const button = (id: string) => el(id) as HTMLButtonElement | null;
const input = (id: string) => el(id) as HTMLInputElement | null;
function text(id: string, value: string): void { const node = el(id); if (node) node.textContent = value; }
function note(id: string, value: string): void { const node = el(id); if (node) { node.textContent = value; node.hidden = !value; } }
function clearPending(): void { if (pending) window.clearTimeout(pending.timer); pending = null; }
function issue(command: Record<string, unknown>, target?: string, action?: ServerAction): void {
  if (!connected || pending) return;
  localError = '';
  const id = `server-${++sequence}-${Date.now()}`;
  pending = { id, cmd: String(command.cmd), target, action, timer: window.setTimeout(() => {
    pending = null; stopTransition(); localError = t('server_timeout');
    if (!visible) note('mode-hint', localError);
    render();
  }, GRAPH_MODE_TTL_MS) };
  const dispatch = () => {
    if (pending?.id === id && connected) send({ ...command, request_id: id });
  };
  if (target && beginTransition(target, true)) {
    deferredCommand = dispatch;
    transitionTimer = window.setTimeout(() => {
      transitionTimer = null;
      // Make black explicit before the command can change backend/UI state.
      transitionCover?.style.setProperty('animation', 'none');
      transitionCover?.style.setProperty('opacity', '1');
      deferredCommand = null;
      dispatch();
    }, 160);
  } else dispatch();
  render();
}
export function requestServerMode(): void {
  if (!connected || visible || pending) return;
  returnMode = currentGraphMode();
  issue({ cmd: 'SET_MODE', mode: 'server' }, 'server');
}
export function trackServerExit(mode: GraphMode): boolean {
  if (!visible || !connected || pending) return false;
  issue({ cmd: 'SET_MODE', mode }, mode);
  return true;
}
export function setServerConnection(next: boolean): void {
  connected = next;
  if (!next) { clearPending(); stopTransition(); localError = t('server_disconnected'); }
  else if (localError === t('server_disconnected')) localError = '';
  render();
}
export function rejectServerCommand(reply: Record<string, unknown>): void {
  if (!pending || pending.id !== reply.request_id || pending.cmd !== reply.response_to) return;
  clearPending();
  stopTransition();
  localError = String(reply.msg ?? reply.error ?? t('err_command'));
  if (!visible) note('mode-hint', localError);
  render();
}
export function syncServerStatus(report: { mode: string; connected?: boolean; server?: ServerStatus; response_to?: string; request_id?: unknown }): void {
  const matching = pending && pending.id === report.request_id && pending.cmd === report.response_to;
  if (matching) {
    if (pending?.action === 'configure' || pending?.action === 'start' || pending?.action === 'restart') dirty = false;
    clearPending(); localError = '';
  }
  if (report.server) {
    status = report.server;
    if (report.mode === 'server' && status.graph_mode) returnMode = status.graph_mode;
    const last = status.logs.at(-1)?.seq;
    // A new backend lifetime starts its sequence again.
    if (last !== undefined) {
      if (last < lastBackendSeq) { displayed = []; clearedThrough = -1; }
      lastBackendSeq = last;
    }
  }
  if (report.mode === transitionTarget && (report.mode === 'server' || report.connected === true)) revealTransition();
  render();
}
export function syncServerViewMode(mode: string): void {
  const next = mode === 'server';
  const changed = next !== visible;
  if (changed && transitionTarget !== mode) beginTransition(mode, false);
  if (changed && transitionCover) {
    transitionCover.style.animation = 'none';
    transitionCover.style.opacity = '1';
  }
  visible = next;
  const updateView = () => {
    const panel = el('server-view');
    if (panel) panel.hidden = !next;
    const screen = document.querySelector<HTMLElement>('.screen-container');
    if (screen) { screen.hidden = next; screen.inert = next; }
    document.body.classList.toggle('server-mode', next);
    if (next && document.activeElement instanceof HTMLElement
        && document.activeElement.closest('.screen-container,.info-bar,#keypad')) document.activeElement.blur();
    render();
  };
  if (changed && !next) { dirty = false; returnMode = null; }
  updateView();
  if (changed && transitionCover) {
    if (next) revealTransition();
    // A server exit can acknowledge before capture/SDR has opened. Keep that
    // transient disconnected state covered, but expose genuine failures promptly.
    else if (readyTimer === null) readyTimer = window.setTimeout(revealTransition, 1000);
  }
}
function configuration(): { bind_address: string; port: number } | null {
  const bind_address = input('server-bind')?.value.trim() ?? '';
  const rawPort = input('server-port')?.value.trim() ?? '';
  const port = Number(rawPort);
  // The backend is authoritative for IP literal validation, including IPv6.
  if (!bind_address || !/^\d+$/.test(rawPort) || !Number.isInteger(port) || port < 1 || port > 65535) {
    localError = t('server_invalid_config'); render(); return null;
  }
  return { bind_address, port };
}
export function commandServer(action: ServerAction): void {
  if (!connected || !visible || !status || pending) return;
  const running = status.state === 'starting' || status.state === 'listening' || status.state === 'stopping';
  if ((action === 'configure' || action === 'start') && running) return;
  const config = action === 'stop' ? null : configuration();
  if (action !== 'stop' && !config) return;
  issue({ cmd: 'SET_SERVER', action, ...(config ?? {}) }, undefined, action);
}
function connectionText(): string {
  if (!status) return '';
  const wildcard = status.bind_address === '0.0.0.0' || status.bind_address === '::';
  const host = wildcard ? location.hostname : status.bind_address;
  const address = host.includes(':') ? `[${host.replace(/^\[|\]$/g, '')}]` : host;
  return `driver=remote,remote=${address}:${status.port},remote:driver=harogic`;
}
async function copy(value: string): Promise<void> {
  try { await navigator.clipboard.writeText(value); localError = ''; }
  catch { localError = t('server_copy_failed'); }
  render();
}
function renderLogs(): void {
  if (!status || paused) return;
  const node = el('server-log');
  if (!node) return;
  const capacity = Math.max(0, status.log_capacity);
  const logs = capacity ? status.logs.filter(log => log.seq > clearedThrough).slice(-capacity) : [];
  const changed = logs.length !== displayed.length || logs.some((log, i) => log.seq !== displayed[i]?.seq
    || log.text !== displayed[i]?.text || log.source !== displayed[i]?.source || log.time_ms !== displayed[i]?.time_ms);
  if (!changed) return;
  displayed = logs;
  const fragment = document.createDocumentFragment();
  for (const log of logs) {
    const row = document.createElement('li'); row.dataset.source = log.source; row.title = `[${log.source}]`;
    const level = /^\s*\[(TRACE|DEBUG|INFO|NOTICE|WARNING|WARN|ERROR|FATAL)\]\s*/i.exec(log.text);
    if (level) {
      const name = level[1].toUpperCase();
      row.dataset.severity = name === 'WARN' || name === 'WARNING' ? 'warning'
        : name === 'ERROR' || name === 'FATAL' ? 'error' : name.toLowerCase();
    }
    const prefix = document.createElement('span'); prefix.className = 'server-log-prefix';
    prefix.textContent = `${new Date(log.time_ms).toISOString()} [${level ? level[1] : log.source}] `;
    const content = document.createElement('span'); content.textContent = level ? log.text.slice(level[0].length) : log.text;
    row.append(prefix, content); fragment.append(row);
  }
  node.replaceChildren(fragment);
  if (follow) node.scrollTop = node.scrollHeight;
}
function render(): void {
  const busy = pending !== null;
  const inServer = visible && deviceMode.get() === 'server';
  const running = status?.state === 'starting' || status?.state === 'listening' || status?.state === 'stopping';
  const modeButton = button('btn-mode-server');
  if (modeButton) { modeButton.disabled = !connected || busy; modeButton.classList.toggle('pending', pending?.target === 'server'); modeButton.classList.toggle('active', visible); }
  if (!visible && (pending?.target === 'server' || localError)) {
    note('mode-hint', pending?.target === 'server' ? t('mode_switching', { mode: t('mode_server') }) : localError);
  }
  for (const action of ['start', 'stop', 'restart', 'configure'] as const) {
    const control = button(`btn-server-${action}`);
    if (!control) continue;
    control.disabled = !connected || !inServer || busy || !status
      || ((action === 'start' || action === 'configure') && !!running)
      || (action === 'stop' && !running);
    control.classList.toggle('pending', pending?.action === action);
  }
  const back = button('btn-server-back');
  if (back) { back.disabled = !connected || busy; back.classList.toggle('pending', !!pending?.target && pending.target !== 'server'); }
  for (const id of ['server-bind', 'server-port']) { const field = input(id); if (field) field.disabled = busy; }
  if (status && !dirty && !busy) {
    const bind = input('server-bind'), port = input('server-port');
    if (bind && document.activeElement !== bind) bind.value = status.bind_address;
    if (port && document.activeElement !== port) port.value = String(status.port);
  }
  const state = el('server-state');
  if (state) { state.textContent = status ? t(`server_state_${status.state}`) : '—'; state.dataset.state = status?.state ?? 'stopped'; }
  text('server-pid', status?.pid == null ? '—' : String(status.pid));
  text('server-uptime', status?.pid == null ? '—' : `${Math.max(0, Math.floor(status.uptime_seconds))} s`);
  text('server-exit', status?.exit_code == null ? '—' : String(status.exit_code));
  text('server-error', status?.error || '—');
  note('server-field-error', !connected ? t('server_disconnected') : localError);
  note('server-dependency', status?.dependency_error || (status?.driver_available === false ? t('server_driver_missing') : ''));
  const ready = status?.state === 'listening';
  const connect = el('server-connect'); if (connect) connect.hidden = !ready;
  text('server-connect-text', connectionText());
  const wildcard = status?.bind_address === '0.0.0.0' || status?.bind_address === '::';
  note('server-connect-hint', ready ? `${t('server_listening_note')}${wildcard ? ` ${t('server_wildcard')}` : ''}` : '');
  const pause = button('btn-server-pause'); if (pause) { pause.dataset.i18n = paused ? 'server_resume' : 'server_pause'; pause.textContent = t(pause.dataset.i18n); pause.classList.toggle('active', paused); pause.setAttribute('aria-pressed', String(paused)); }
  const followButton = button('btn-server-follow'); if (followButton) { followButton.classList.toggle('active', follow); followButton.setAttribute('aria-pressed', String(follow)); }
  renderLogs();
  text('server-log-meta', status ? t('server_log_count', { count: displayed.length, capacity: status.log_capacity }) : '');
}
function stopTransition(): void {
  transitionGeneration++;
  if (transitionFrame !== null) cancelAnimationFrame(transitionFrame);
  if (transitionTimer !== null) window.clearTimeout(transitionTimer);
  if (readyTimer !== null) window.clearTimeout(readyTimer);
  transitionFrame = transitionTimer = readyTimer = null;
  if (deferredCommand) clearPending();
  deferredCommand = null;
  transitionTarget = null;
  transitionRevealing = false;
  transitionCover?.remove();
  transitionCover = null;
  matrixEdge?.remove();
  matrixEdge = null;
  if (matrixAtlas) matrixAtlas.width = matrixAtlas.height = 1;
  matrixAtlas = null;
  document.documentElement.style.removeProperty('--srv-edge');
  const canvas = el('server-transition') as HTMLCanvasElement | null;
  if (canvas) {
    canvas.hidden = true;
    // Release the backing store rather than keeping a viewport-sized idle canvas.
    canvas.width = canvas.height = 1;
  }
}
function finishTransition(): void {
  // Disabling motion removes the cover immediately but preserves the user's click.
  const dispatch = deferredCommand;
  deferredCommand = null;
  stopTransition();
  dispatch?.();
  render();
}
function beginTransition(target: string, fade: boolean): boolean {
  stopTransition();
  const card = document.querySelector<HTMLElement>('.analyzer-card');
  if (!card || !el('server-transition') || !input('server-motion')?.checked
      || window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) return false;
  transitionTarget = target;
  const cover = document.createElement('div');
  cover.className = `server-transition-cover${fade ? ' server-transition-darken' : ''}`;
  cover.setAttribute('aria-hidden', 'true');
  card.append(cover);
  transitionCover = cover;
  return true;
}
function revealTransition(): void {
  if (!transitionCover || transitionRevealing) return;
  transitionRevealing = true;
  if (readyTimer !== null) window.clearTimeout(readyTimer);
  readyTimer = null;
  const canvas = el('server-transition') as HTMLCanvasElement | null;
  const card = document.querySelector<HTMLElement>('.analyzer-card');
  if (!canvas || !card) { stopTransition(); return; }
  const generation = transitionGeneration;
  const root = document.documentElement;
  card.append(canvas);
  canvas.hidden = false;
  const width = canvas.clientWidth, height = canvas.clientHeight;
  if (!width || !height) { stopTransition(); return; }
  // Bound both the backing store and glyph count, independently of UI zoom/DPI.
  const ratio = Math.min(1.5, (window.devicePixelRatio || 1) * canvas.getBoundingClientRect().width / width,
    Math.sqrt(1_200_000 / (width * height)));
  canvas.width = Math.max(1, Math.round(width * ratio));
  canvas.height = Math.max(1, Math.round(height * ratio));
  const context = canvas.getContext('2d');
  if (!context) { stopTransition(); return; }
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  const color = getComputedStyle(root).getPropertyValue('--text').trim();
  const glyphs = '0123456789ABCDEFアイウエオカキクケコサシスセソ';
  const cell = 24;
  const atlas = document.createElement('canvas');
  matrixAtlas = atlas;
  atlas.width = cell * glyphs.length; atlas.height = cell * 2;
  const ink = atlas.getContext('2d');
  if (!ink) { stopTransition(); return; }
  ink.font = '16px monospace'; ink.textAlign = 'center'; ink.textBaseline = 'middle';
  for (let index = 0; index < glyphs.length; index++) {
    ink.fillStyle = color; ink.shadowColor = color; ink.shadowBlur = 4;
    ink.fillText(glyphs[index], index * cell + cell / 2, cell / 2);
    ink.fillStyle = '#efffef'; ink.shadowBlur = 7;
    ink.fillText(glyphs[index], index * cell + cell / 2, cell * 1.5);
  }
  const columns = Math.min(120, Math.max(1, Math.floor(width / 19)));
  const spacing = width / columns;
  const glyphSize = Math.min(24, Math.max(14, spacing));
  const duration = 1440;
  const rowCount = Math.ceil(height / glyphSize);
  // Like ttfx's Matrix effect, each column owns its timing and persistent trail.
  // Prewarm some columns so this short transition starts with uneven rain heights.
  const streams = Array.from({ length: columns }, (_, column) => {
    const length = Math.max(5, Math.floor(rowCount * (0.15 + Math.random() * 0.65)));
    return {
      x: (column + 0.5) * spacing - glyphSize / 2,
      head: Math.random() < 0.6 ? Math.floor(Math.random() * rowCount) : -1,
      delay: Math.random() < 0.5 ? 0 : Math.random() * 0.65,
      interval: 0.010 + Math.random() * 0.055,
      elapsed: 0,
      slot: 0,
      length,
      glyphs: Uint16Array.from({ length: Math.max(5, rowCount) }, () => Math.floor(Math.random() * glyphs.length)),
      brightness: Float32Array.from({ length: Math.max(5, rowCount) }, () => 0.35 + Math.random() * 0.6),
    };
  });
  const edgeLayer = document.createElement('div');
  edgeLayer.className = 'server-matrix-edge';
  edgeLayer.setAttribute('aria-hidden', 'true');
  card.append(edgeLayer);
  matrixEdge = edgeLayer;
  let previousProgress = 0;
  const draw = (progress: number) => {
    const dt = Math.max(0, Math.min(0.05, (progress - previousProgress) * duration / 1000));
    previousProgress = progress;
    const edge = -0.2 + progress * 1.4;
    root.style.setProperty('--srv-edge', `${edge * 100}%`);
    context.clearRect(0, 0, width, height);
    const envelope = Math.min(1, progress / 0.08, (1 - progress) / 0.18);
    for (const stream of streams) {
      if (stream.delay > 0) { stream.delay -= dt; continue; }
      stream.elapsed += dt;
      while (stream.elapsed >= stream.interval) {
        stream.elapsed -= stream.interval;
        stream.head++;
        stream.slot = (stream.slot + 1) % stream.glyphs.length;
        stream.glyphs[stream.slot] = Math.floor(Math.random() * glyphs.length);
        stream.brightness[stream.slot] = 0.35 + Math.random() * 0.6;
      }
      if (stream.head - stream.length > rowCount) {
        stream.head = -1;
        stream.delay = 0.05 + Math.random() * 0.45;
        stream.interval = 0.010 + Math.random() * 0.055;
        stream.length = Math.max(5, Math.floor(rowCount * (0.15 + Math.random() * 0.65)));
        stream.elapsed = 0;
        continue;
      }
      // Sparse elapsed-time mutation keeps letters alive without synchronized flashing.
      const mutate = Math.floor(Math.random() * stream.length);
      if (Math.random() < 1 - Math.exp(-3 * dt)) stream.glyphs[mutate] = Math.floor(Math.random() * glyphs.length);
      if (Math.random() < 1 - Math.exp(-2 * dt)) stream.brightness[mutate] = 0.35 + Math.random() * 0.6;
      for (let row = 0; row < stream.length; row++) {
        const y = (stream.head - row) * glyphSize;
        if (y < -glyphSize || y > height + glyphSize) continue;
        const index = (stream.slot - row + stream.glyphs.length) % stream.glyphs.length;
        const fade = 1 - row / stream.length;
        context.globalAlpha = envelope * (row ? stream.brightness[index] * fade * fade : 1);
        context.drawImage(atlas, stream.glyphs[index] * cell, row ? 0 : cell, cell, cell,
          stream.x, y, glyphSize, glyphSize);
      }
    }
    context.globalAlpha = 1;
  };
  const startRain = () => {
    if (generation !== transitionGeneration) return;
    const start = performance.now();
    const frame = (now: number) => {
      if (generation !== transitionGeneration) return;
      const progress = (now - start) / duration;
      if (progress >= 1) { stopTransition(); return; }
      draw(progress);
      transitionFrame = requestAnimationFrame(frame);
    };
    transitionFrame = requestAnimationFrame(frame);
  };
  transitionCover.classList.add('server-transition-reveal');
  draw(0);
  startRain();
}
export function initServerMode(onLeave: (mode: GraphMode) => void): void {
  leave = onLeave;
  input('server-bind')?.addEventListener('input', () => { dirty = true; localError = ''; render(); });
  input('server-port')?.addEventListener('input', () => { dirty = true; localError = ''; render(); });
  for (const action of ['start', 'stop', 'restart', 'configure'] as const) button(`btn-server-${action}`)?.addEventListener('click', () => commandServer(action));
  button('btn-server-back')?.addEventListener('click', () => { if (connected && !pending) leave?.(returnMode ?? status?.graph_mode ?? currentGraphMode()); });
  button('btn-server-pause')?.addEventListener('click', () => { paused = !paused; render(); });
  button('btn-server-follow')?.addEventListener('click', () => { follow = !follow; if (follow) { const log = el('server-log'); if (log) log.scrollTop = log.scrollHeight; } render(); });
  el('server-log')?.addEventListener('scroll', () => { const log = el('server-log'); if (log && log.scrollHeight - log.scrollTop - log.clientHeight > 24) { follow = false; render(); } });
  button('btn-server-clear')?.addEventListener('click', () => { clearedThrough = status?.logs.at(-1)?.seq ?? clearedThrough; displayed = []; el('server-log')?.replaceChildren(); render(); });
  button('btn-server-copy-logs')?.addEventListener('click', () => {
    void copy(displayed.map(log => `${new Date(log.time_ms).toISOString()} [${log.source}] ${log.text}`).join('\n'));
  });
  button('btn-server-copy-connect')?.addEventListener('click', () => { void copy(connectionText()); });
  const motion = input('server-motion');
  if (motion) {
    motion.checked = localStorage.getItem('web-sa-server-motion') !== '0';
    motion.addEventListener('change', () => { localStorage.setItem('web-sa-server-motion', motion.checked ? '1' : '0'); if (!motion.checked) finishTransition(); });
  }
  window.matchMedia?.('(prefers-reduced-motion: reduce)').addEventListener?.('change', event => { if (event.matches) finishTransition(); });
  onLangChange(render);
  render();
}
