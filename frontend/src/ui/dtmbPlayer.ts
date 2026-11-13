import { t } from '../core/i18n';
import { limitStreamRendering } from '../render/redraw';
import './dtmbPlayer.css';

interface Program { id: number; name: string; provider: string; video: string; audio: string; scrambled: boolean }
export interface DtmbStatus {
  state: string; streamId: string; streamReady: boolean; programs: Program[]; playbackReason: string | null;
  header: number; phaseRotates: boolean; mapping: string; codeRate: number; cnDb: number; merDb: number;
  blocksOk: number; blocksBad: number; dropped: number; transportPackets: number; continuityErrors: number; cfoHz: number;
}
const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);
export function dtmbStatus(value: unknown): DtmbStatus | null {
  if (!object(value) || value.type !== 'dtmb') return null;
  const num = (key: string): number => typeof value[key] === 'number' && Number.isFinite(value[key]) ? value[key] as number : 0;
  const programs = Array.isArray(value.programs) ? value.programs.filter((p): p is Program => object(p)
    && Number.isInteger(p.id) && Number(p.id) > 0 && Number(p.id) <= 65535 && typeof p.name === 'string'
    && typeof p.provider === 'string' && typeof p.video === 'string' && typeof p.audio === 'string' && typeof p.scrambled === 'boolean').slice(0, 64) : [];
  return {
    state: ['searching', 'synchronizing', 'correcting', 'receiving', 'stopped'].includes(String(value.state)) ? String(value.state) : 'searching',
    streamId: typeof value.streamId === 'string' && /^[a-f0-9]{32}$/.test(value.streamId) ? value.streamId : '',
    streamReady: value.streamReady === true, programs, playbackReason: typeof value.playbackReason === 'string' ? value.playbackReason : null,
    header: num('header'), phaseRotates: value.phaseRotates === true, mapping: typeof value.mapping === 'string' ? value.mapping : '',
    codeRate: num('codeRate'), cnDb: num('cnDb'), merDb: num('merDb'), blocksOk: num('blocksOk'), blocksBad: num('blocksBad'),
    dropped: num('dropped'), transportPackets: num('transportPackets'), continuityErrors: num('continuityErrors'), cfoHz: num('cfoHz'),
  };
}

export interface DtmbPlayer {
  update(status: DtmbStatus | null, active: boolean): void;
  beforeMove(): void; afterMove(): void; dispose(): void;
}

function waitFor(target: EventTarget, event: string, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const clear = (): void => { target.removeEventListener(event, done); target.removeEventListener('error', failed); signal.removeEventListener('abort', failed); };
    const done = (): void => { clear(); resolve(); };
    const failed = (): void => { clear(); reject(new Error(signal.aborted ? 'aborted' : 'media')); };
    if (signal.aborted) { failed(); return; }
    target.addEventListener(event, done, { once: true }); target.addEventListener('error', failed, { once: true }); signal.addEventListener('abort', failed, { once: true });
  });
}

export function mountDtmbPlayer(host: HTMLElement): DtmbPlayer {
  const doc = host.ownerDocument;
  const make = <K extends keyof HTMLElementTagNameMap>(tag: K, className = ''): HTMLElementTagNameMap[K] => {
    const node = doc.createElement(tag); node.className = className; return node;
  };
  host.classList.add('dtmb-player');
  const toolbar = make('div', 'dtmb-toolbar');
  const select = make('select', 'dtmb-program');
  const play = make('button', 'btn dtmb-play'); play.type = 'button';
  toolbar.append(select, play);
  const stage = make('div', 'dtmb-stage');
  const video = make('video'); video.controls = true; video.playsInline = true; video.preload = 'none';
  const placeholder = make('div', 'dtmb-placeholder');
  const glyph = make('span', 'dtmb-glyph'); glyph.textContent = '▣'; glyph.setAttribute('aria-hidden', 'true');
  const hint = make('span');
  placeholder.append(glyph, hint); stage.append(video, placeholder);
  const bar = make('div', 'dtmb-signal');
  const badge = make('span', 'dtmb-badge'); const quality = make('span'); const profile = make('span', 'dtmb-profile');
  bar.append(badge, quality, profile);
  const message = make('p', 'dtmb-message'); message.setAttribute('role', 'status');
  const details = make('details', 'dtmb-details'); const summary = make('summary'); const metrics = make('p'); details.append(summary, metrics);
  host.replaceChildren(toolbar, stage, bar, message, details);
  let status: DtmbStatus | null = null, active = false, selected = 0, shape = '', error = '';
  let controller: AbortController | null = null, mediaUrl = '', running = false;
  let releaseDisplay: (() => void) | null = null;
  let resumeAfterMove = false;
  let progress = '', lastProgress = 0;
  const fresh = (): boolean => !!status?.streamReady && performance.now() - lastProgress < 2500;
  const selectedProgram = (): Program | undefined => status?.programs.find(p => p.id === selected);
  const draw = (): void => {
    const program = selectedProgram();
    select.setAttribute('aria-label', t('dtmb_program'));
    select.disabled = !status?.programs.length;
    const nextShape = JSON.stringify(status?.programs.map(p => [p.id, p.name, p.scrambled]) ?? []) + t('dtmb_wait_programs');
    if (nextShape !== shape) {
      shape = nextShape;
      select.replaceChildren();
      if (!status?.programs.length) { const option = make('option'); option.textContent = t('dtmb_wait_programs'); select.append(option); }
      else for (const p of status.programs) {
        const option = make('option'); option.value = String(p.id); option.textContent = p.name + (p.scrambled ? ` · ${t('dtmb_encrypted')}` : ''); select.append(option);
      }
    }
    if (selected) select.value = String(selected);
    play.textContent = t(running ? 'dtmb_stop' : 'dtmb_play');
    play.disabled = !running && (!active || !fresh() || !status?.streamId || !program || program.scrambled || (!program.video && !program.audio) || !!status.playbackReason);
    play.setAttribute('aria-pressed', String(running));
    placeholder.hidden = running;
    video.hidden = !running;
    let state = active ? status?.state ?? 'searching' : 'stopped';
    if (state === 'receiving' && !fresh()) state = 'correcting';
    badge.textContent = t(`dtmb_${state}`);
    badge.classList.toggle('locked', state === 'receiving' && fresh());
    quality.textContent = status?.header ? `C/N ${status.cnDb.toFixed(1)} · MER ${status.merDb.toFixed(1)} dB` : 'C/N — · MER —';
    profile.textContent = status?.mapping ? `${status.mapping} · ${status.codeRate.toFixed(1)}` : '8 MHz';
    hint.textContent = program?.scrambled ? t('dtmb_encrypted') : program && fresh() ? t('dtmb_ready') : t(`dtmb_${state}_hint`);
    message.textContent = error || status?.playbackReason || ''; message.hidden = !message.textContent;
    message.classList.toggle('error', !!error || !!status?.playbackReason);
    summary.textContent = t('dtmb_signal_details');
    metrics.textContent = status ? [status.header ? `PN${status.header} · ${t(status.phaseRotates ? 'dtmb_rotating' : 'dtmb_fixed')}` : '',
      `CFO ${status.cfoHz.toFixed(0)} Hz`, `LDPC ${status.blocksOk.toLocaleString()} / ${status.blocksBad.toLocaleString()}`,
      `TS ${status.transportPackets.toLocaleString()}`, `${t('dtmb_continuity')} ${status.continuityErrors.toLocaleString()}`,
      `${t('dtmb_dropped')} ${status.dropped.toLocaleString()}`, program ? [program.video, program.audio, program.provider].filter(Boolean).join(' · ') : ''].filter(Boolean).join(' · ') : '—';
  };
  const stop = (): void => {
    controller?.abort(); controller = null; running = false;
    releaseDisplay?.(); releaseDisplay = null;
    video.pause(); video.removeAttribute('src'); video.load();
    if (mediaUrl) { URL.revokeObjectURL(mediaUrl); mediaUrl = ''; }
    draw();
  };
  const start = async (): Promise<void> => {
    const program = selectedProgram(), stream = status?.streamId;
    if (!program || !stream || !active) return;
    stop(); error = '';
    const Media = (host.ownerDocument.defaultView as (Window & typeof globalThis) | null)?.MediaSource ?? globalThis.MediaSource;
    const mime = `${program.video ? 'video' : 'audio'}/mp4; codecs="${[program.video ? 'avc1.42c01f' : '', program.audio ? 'mp4a.40.2' : ''].filter(Boolean).join(', ')}"`;
    if (typeof Media === 'undefined' || !Media.isTypeSupported(mime)) { error = t('dtmb_browser_unsupported'); draw(); return; }
    const own = new AbortController(); controller = own; running = true;
    releaseDisplay = limitStreamRendering(2);
    const media = new Media(); mediaUrl = URL.createObjectURL(media); video.src = mediaUrl;
    const opened = waitFor(media, 'sourceopen', own.signal);
    const playing = video.play().catch(() => { if (!own.signal.aborted) { error = t('dtmb_press_video_play'); draw(); } });
    draw();
    try {
      await opened;
      const response = await fetch(`/api/sdr/dtmb/${stream}/${program.id}/live.mp4`, { signal: own.signal, cache: 'no-store' });
      if (!response.ok || !response.body) {
        const body: unknown = await response.json().catch(() => null);
        throw new Error(object(body) && typeof body.error === 'string' ? body.error : t('dtmb_playback_failed'));
      }
      const buffer = media.addSourceBuffer(mime);
      const reader = response.body.getReader();
      try {
        while (!own.signal.aborted) {
          const { value, done } = await reader.read();
          if (done) throw new Error(t('dtmb_stream_ended'));
          buffer.appendBuffer(value);
          await waitFor(buffer, 'updateend', own.signal);
          if (buffer.buffered.length) {
            const end = buffer.buffered.end(buffer.buffered.length-1);
            if (end - video.currentTime > 5) video.currentTime = Math.max(buffer.buffered.start(0), end-1);
            const cut = video.currentTime - 3;
            if (cut > buffer.buffered.start(0) + 1) {
              const trimmed = waitFor(buffer, 'updateend', own.signal); buffer.remove(0, cut); await trimmed;
            }
          }
        }
      } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
    } catch (e) {
      if (controller === own && !own.signal.aborted) {
        error = e instanceof Error && !(e instanceof DOMException) && e.message !== 'media' ? e.message : t('dtmb_playback_failed'); stop();
      }
    } finally { void playing; }
  };
  play.addEventListener('click', () => { if (running) stop(); else void start(); });
  select.addEventListener('change', () => { const resume = running, id = Number(select.value); stop(); selected = id; error = ''; draw(); if (resume) void start(); });
  video.addEventListener('error', () => { if (running) { error = t('dtmb_playback_failed'); stop(); } });
  const timer = window.setInterval(() => {
    if (running && !fresh()) { error = t('dtmb_stream_ended'); stop(); }
    else if (active) draw();
  }, 500);
  draw();
  return {
    update(next, enabled) {
      const changed = status?.streamId !== next?.streamId;
      status = next; active = enabled;
      const nextProgress = `${next?.streamId}:${next?.transportPackets}`;
      if (nextProgress !== progress) { progress = nextProgress; lastProgress = performance.now(); }
      if (changed || !enabled || status?.state === 'stopped') { error = ''; if (running) stop(); }
      if (!status?.programs.some(p => p.id === selected)) {
        if (running) stop();
        selected = status?.programs.find(p => !p.scrambled && !!(p.video || p.audio))?.id ?? status?.programs[0]?.id ?? 0;
      }
      draw();
    },
    beforeMove() { resumeAfterMove = running; if (running) stop(); },
    afterMove() { if (resumeAfterMove) { resumeAfterMove = false; if (active && fresh()) void start(); } },
    dispose() { window.clearInterval(timer); stop(); host.replaceChildren(); },
  };
}
