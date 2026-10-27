import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import page from '../../index.html?raw';

vi.mock('../audio/sdrAudio', () => ({
  prepareSdrAudioTransition: vi.fn(), setSdrAudioEnabled: vi.fn(), setSdrAudioMode: vi.fn(),
  setSdrReceiverAudioError: vi.fn(),
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
  sent: Record<string, unknown>[] = [];
  send(message: string) { this.sent.push(JSON.parse(message)); }
}
let socket: DisplaySocket;
beforeEach(() => {
  vi.resetModules(); vi.useFakeTimers(); localStorage.clear();
  document.body.innerHTML = new DOMParser().parseFromString(page, 'text/html').body.innerHTML;
  vi.stubGlobal('WebSocket', class extends DisplaySocket { constructor() { super(); socket = this; } });
  vi.stubGlobal('matchMedia', vi.fn(() => ({ matches: true, addEventListener: vi.fn() })));
});
afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); vi.unstubAllGlobals(); document.body.replaceChildren(); });
const server = (extra: Record<string, unknown> = {}) => ({
  state: 'stopped', bind_address: '127.0.0.1', port: 55132, pid: null,
  uptime_seconds: 0, exit_code: null, error: null, logs: [], log_capacity: 3,
  driver_available: true, dependency_error: null, graph_mode: 'rta', ...extra,
});
function receive(mode: string, serverExtra: Record<string, unknown> = {}, extra: Record<string, unknown> = {}): void {
  socket.onmessage!({ data: JSON.stringify({ cmd: 'STATUS', mode, connected: true,
    center: 1e9, span: 3.125e6, ref: 0, req: { swp: {}, rta: {}, sdr: {} }, actual: {},
    server: server(serverExtra), ...extra }) } as MessageEvent);
}
// Dynamic imports intentionally create a fresh browser/module lifetime after vi.resetModules().
async function load(): Promise<void> {
  const ws = await import('../core/ws');
  const controls = await import('../ui/controls');
  controls.bindActions(); ws.connectWS(); socket.onopen!();
}
function click(id: string): void { (document.getElementById(id) as HTMLButtonElement).click(); }
const commands = (cmd: string) => socket.sent.filter(command => command.cmd === cmd);

describe('Server mode', () => {
  it('allows Start to recheck dependencies after the host installation is repaired', async () => {
    await load();
    receive('server', { state: 'failed', driver_available: false, dependency_error: 'Install SoapySDRServer on this host' });
    expect(document.getElementById('server-dependency')!.textContent).toContain('Install SoapySDRServer');
    expect((document.getElementById('btn-server-start') as HTMLButtonElement).disabled).toBe(false);
    click('btn-server-start');
    expect(commands('SET_SERVER')).toMatchObject([{ action: 'start' }]);
  });

  it('waits for the first non-server STATUS before restoring the saved graph mode once', async () => {
    localStorage.setItem('web-sa-mode', 'sdr');
    await load();
    expect(commands('SET_MODE')).toEqual([]);
    receive('std');
    expect(commands('SET_MODE')).toMatchObject([{ mode: 'sdr' }]);
    receive('sdr');
    expect(commands('SET_MODE')).toHaveLength(1);
  });

  it('preserves an active server on refresh and reconnect despite a saved graph preference', async () => {
    localStorage.setItem('web-sa-mode', 'sdr');
    await load();
    expect(commands('SET_MODE')).toEqual([]);
    receive('server', { state: 'listening', pid: 42, uptime_seconds: 9 });
    expect(commands('SET_MODE')).toEqual([]);
    expect(document.querySelector<HTMLElement>('.screen-container')!.hidden).toBe(true);
    expect(document.getElementById('server-view')!.hidden).toBe(false);
    expect(document.getElementById('server-pid')!.textContent).toBe('42');
    expect(localStorage.getItem('web-sa-mode')).toBe('sdr');
    expect(document.getElementById('server-transition')!.hidden).toBe(true);
    socket.onclose!(); vi.advanceTimersByTime(1000); socket.onopen!();
    receive('server', { state: 'listening', pid: 42 });
    expect(commands('SET_MODE')).toEqual([]);
  });

  it('enters only on confirmation, starts separately, and returns to the remembered spectrum mode', async () => {
    await load(); receive('rta');
    // Read state from this test's freshly reloaded module graph.
    const graph = await import('../ui/graphMode');
    click('btn-mode-server');
    const entry = commands('SET_MODE').at(-1)!;
    expect(entry.mode).toBe('server');
    expect(commands('SET_SERVER')).toEqual([]);
    expect(document.getElementById('server-view')!.hidden).toBe(true);
    receive('server', {}, { response_to: entry.cmd, request_id: entry.request_id });
    expect(graph.currentGraphMode()).toBe('rta');
    expect(document.getElementById('server-view')!.hidden).toBe(false);
    click('btn-server-start');
    const start = commands('SET_SERVER').at(-1)!;
    expect(start).toMatchObject({ action: 'start', bind_address: '127.0.0.1', port: 55132 });
    receive('server', { state: 'listening', pid: 42 }, { response_to: start.cmd, request_id: start.request_id });
    expect(document.getElementById('server-connect-text')!.textContent).toContain('remote:driver=harogic');
    click('btn-server-stop');
    const stop = commands('SET_SERVER').at(-1)!;
    receive('server', {}, { response_to: stop.cmd, request_id: stop.request_id });
    expect(document.getElementById('server-view')!.hidden).toBe(false);
    click('btn-server-back');
    const exit = commands('SET_MODE').at(-1)!;
    expect(exit.mode).toBe('rta');
    expect(document.querySelector<HTMLElement>('.screen-container')!.hidden).toBe(true);
    receive('rta', {}, { response_to: exit.cmd, request_id: exit.request_id });
    expect(document.querySelector<HTMLElement>('.screen-container')!.hidden).toBe(false);
    expect(localStorage.getItem('web-sa-mode')).toBe('rta');
  });

  it('preserves drafts across polling, clears pending only for its reply, and shows command errors', async () => {
    await load(); receive('server');
    const bind = document.getElementById('server-bind') as HTMLInputElement;
    bind.value = '0.0.0.0'; bind.dispatchEvent(new Event('input'));
    receive('server'); expect(bind.value).toBe('0.0.0.0');
    click('btn-server-configure');
    const configure = commands('SET_SERVER').at(-1)!;
    receive('server', {}, { response_to: 'STATUS', request_id: configure.request_id });
    expect((document.getElementById('btn-server-start') as HTMLButtonElement).disabled).toBe(true);
    socket.onmessage!({ data: JSON.stringify({ cmd: 'ERROR', response_to: 'SET_SERVER',
      request_id: configure.request_id, msg: 'Address unavailable; choose another bind address' }) } as MessageEvent);
    expect(document.getElementById('server-field-error')!.textContent).toContain('choose another bind');
    expect((document.getElementById('btn-server-start') as HTMLButtonElement).disabled).toBe(false);
    socket.onclose!(); click('btn-server-start');
    expect(commands('SET_SERVER')).toHaveLength(1);
  });

  it('keeps original bounded logs, pauses display, and clears only the local display', async () => {
    await load();
    const line = (seq: number) => ({ seq, time_ms: 1700000000000 + seq, source: 'stdout', text: `raw <${seq}>\tvalue` });
    receive('server', { logs: [line(1), line(2), line(3), line(4)] });
    expect(document.getElementById('server-log')!.children).toHaveLength(3);
    expect(document.getElementById('server-log')!.textContent).toContain('raw <4>\tvalue');
    expect(document.getElementById('server-log')!.querySelector('value')).toBeNull();
    click('btn-server-pause');
    receive('server', { logs: [line(3), line(4), line(5)] });
    expect(document.getElementById('server-log')!.textContent).not.toContain('raw <5>');
    click('btn-server-pause');
    expect(document.getElementById('server-log')!.textContent).toContain('raw <5>');
    click('btn-server-clear'); receive('server', { logs: [line(4), line(5), line(6)] });
    expect(document.getElementById('server-log')!.children).toHaveLength(1);
    expect(document.getElementById('server-log')!.textContent).toContain('raw <6>');
  });

  it('uses message severity independently of stdout or stderr and shows each level once', async () => {
    await load();
    const entries = [
      { source: 'stderr', text: '[INFO] Listening for connections', severity: 'info', label: '[INFO]' },
      { source: 'stdout', text: '[INFO] Client connected', severity: 'info', label: '[INFO]' },
      { source: 'stderr', text: 'Unclassified diagnostic', severity: undefined, label: '[stderr]' },
      { source: 'stdout', text: 'Unclassified output', severity: undefined, label: '[stdout]' },
      { source: 'stderr', text: '[WARNING] Client is slow', severity: 'warning', label: '[WARNING]' },
      { source: 'stdout', text: '[WARN] Queue is full', severity: 'warning', label: '[WARN]' },
      { source: 'stdout', text: '[ERROR] Bind failed', severity: 'error', label: '[ERROR]' },
      { source: 'stderr', text: '[FATAL] Device unavailable', severity: 'error', label: '[FATAL]' },
      { source: 'stderr', text: 'Payload contains [ERROR] as text', severity: undefined, label: '[stderr]' },
    ];
    receive('server', { log_capacity: entries.length, logs: entries.map((entry, seq) => ({
      seq, time_ms: 1700000000000 + seq, source: entry.source, text: entry.text,
    })) });
    const rows = document.querySelectorAll<HTMLElement>('#server-log li');
    expect(rows).toHaveLength(entries.length);
    entries.forEach((entry, index) => {
      const row = rows[index];
      expect(row.dataset.severity).toBe(entry.severity);
      expect(row.title).toBe(`[${entry.source}]`);
      expect(row.textContent!.split(entry.label)).toHaveLength(2);
      expect(row.querySelector('.server-log-prefix')!.textContent).toBe(
        `${new Date(1700000000000 + index).toISOString()} ${entry.label} `);
      if (entry.severity) expect(row.textContent).not.toContain(`[${entry.source}]`);
      else expect(row.lastElementChild!.textContent).toBe(entry.text);
    });
  });

  it('copies original source and full raw text while the displayed logs are paused', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    try {
      await load();
      const logs = [
        { seq: 1, time_ms: 1700000000001, source: 'stderr', text: '  [INFO]  <ready>\tvalue  ' },
        { seq: 2, time_ms: 1700000000002, source: 'stdout', text: '[WARN] Keep original spacing\t' },
      ];
      receive('server', { logs });
      click('btn-server-pause');
      receive('server', { logs: [...logs, { seq: 3, time_ms: 1700000000003, source: 'stderr', text: '[ERROR] Later' }] });
      click('btn-server-copy-logs');
      expect(writeText).toHaveBeenCalledWith(logs.map(log =>
        `${new Date(log.time_ms).toISOString()} [${log.source}] ${log.text}`).join('\n'));
      expect(document.getElementById('server-log')!.textContent).not.toContain('Later');
      expect(document.getElementById('server-log')!.querySelector('ready')).toBeNull();
    } finally {
      Reflect.deleteProperty(navigator, 'clipboard');
    }
  });

  it.each([false, true])('restores SDR once after Server opens the device (browser reload: %s)', async reload => {
    localStorage.setItem('web-sa-sdr-audio', '1');
    localStorage.setItem('web-sa-sdr-center', '7100000');
    localStorage.setItem('web-sa-sdr-listen', '7074000');
    localStorage.setItem('web-sa-sdr-demod', 'am');
    localStorage.setItem('web-sa-sdr-ifbw', '2700');
    localStorage.setItem('web-sa-sdr-decimate', '32');
    localStorage.setItem('web-sa-sdr-am-sync', '1');
    localStorage.setItem('web-sa-sdr-volume', '0.37');
    await load();
    // These observers must share load()'s fresh module graph to exercise browser reload.
    const audio = await import('../audio/sdrAudio');
    const state = await import('../ui/sdrState');
    const graph = await import('../ui/graphMode');
    const receiver = { center: 7.1e6, decimate: 32, listen: 7.074e6, demod: 'am', if_bw: 2700,
      channels: [{ channel: 1, enabled: true, freq: 7.074e6, mode: 'am', if_bw: 2700,
        volume: 0.37, am_sync: true, am_sync_generation: 1 }] };
    vi.mocked(audio.setSdrAudioEnabled).mockClear();
    if (!reload) {
      receive('std');
      click('btn-mode-sdr');
      const beforeOpen = socket.sent.length;
      receive('sdr', {}, { connected: false, center: 7.1e6, sdr: receiver });
      vi.advanceTimersByTime(4000);
      expect(socket.sent.slice(beforeOpen).filter(command => command.cmd !== 'STATUS')).toEqual([]);
      expect(state.sdrIfbw.get()).toBe(2700);
      receive('sdr', {}, { center: 7.1e6, sdr: receiver });
      vi.advanceTimersByTime(800);
      expect(audio.setSdrAudioEnabled).toHaveBeenLastCalledWith(true);
      click('btn-mode-server');
    } else {
      expect(graph.currentGraphMode()).toBe('std');
    }
    const beforeServer = socket.sent.length;
    const serverEntry = commands('SET_MODE').at(-1);
    receive('server', { graph_mode: 'sdr' }, { connected: false,
      ...(reload ? {} : { response_to: serverEntry?.cmd, request_id: serverEntry?.request_id }) });
    vi.advanceTimersByTime(1000);
    expect(socket.sent.slice(beforeServer).filter(command => command.cmd !== 'STATUS')).toEqual([]);
    expect(state.sdrDemod.get()).toBe('am');
    expect(state.sdrIfbw.get()).toBe(2700);
    expect(state.sdrAmSync.get()).toBe(true);
    vi.mocked(audio.setSdrAudioEnabled).mockClear();
    click('btn-server-back');
    const exit = commands('SET_MODE').at(-1)!;
    expect(exit).toMatchObject({ mode: 'sdr', request_id: expect.stringMatching(/^server-/) });
    const beforeAck = socket.sent.length;
    // Polls cannot consume the exit request identity or send receiver settings in Server.
    receive('server', { graph_mode: 'sdr' }, { connected: false, response_to: 'STATUS', request_id: exit.request_id });
    expect((document.getElementById('btn-server-back') as HTMLButtonElement).disabled).toBe(true);
    expect(socket.sent).toHaveLength(beforeAck);
    receive('sdr', { graph_mode: 'sdr' }, { connected: false, response_to: exit.cmd,
      request_id: exit.request_id, center: 7.1e6, sdr: receiver });
    vi.advanceTimersByTime(4000);
    expect(socket.sent.slice(beforeAck).filter(command => command.cmd !== 'STATUS')).toEqual([]);
    expect(audio.setSdrAudioEnabled).not.toHaveBeenCalledWith(true);
    expect(localStorage.getItem('web-sa-sdr-audio')).toBe('1');
    receive('sdr', { graph_mode: 'sdr' }, { center: 7.1e6, sdr: receiver });
    const replay = socket.sent.slice(beforeAck).filter(command => command.cmd !== 'STATUS');
    expect(replay.filter(command => command.cmd === 'SET_SDR')).toEqual([
      expect.objectContaining({ center: 7.1e6, decimate: 32 }),
    ]);
    expect(replay).toContainEqual(expect.objectContaining({ cmd: 'SET_SDR_CHANNEL', channel: 1, freq: 7.074e6 }));
    expect(replay).toContainEqual(expect.objectContaining({ cmd: 'SET_SDR_CHANNEL', channel: 1,
      mode: 'am', ifbw: 2700, volume: 0.37, am_sync: true }));
    vi.advanceTimersByTime(800);
    expect(vi.mocked(audio.setSdrAudioEnabled).mock.calls.filter(([on]) => on)).toEqual([[true]]);
    const afterRestore = socket.sent.length;
    receive('sdr', { graph_mode: 'sdr' }, { center: 7.1e6, sdr: receiver });
    expect(socket.sent).toHaveLength(afterRestore);
    // An ordinary SDR disconnect/recovery does not replay an already completed entry.
    receive('sdr', {}, { connected: false, sdr: receiver });
    receive('sdr', {}, { center: 7.1e6, sdr: receiver });
    expect(socket.sent).toHaveLength(afterRestore);
  });

});
