import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { setLang } from '../core/i18n';
import { setTheme } from '../core/theme';
import { clone } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountWorkbench } from '../ui/workbenchApp';
import { createWorkbenchOverview } from '../ui/workbenchOverview';
import { cardGrid, loadOverviewState, reconcileOverview, saveOverviewState, snapOverviewPosition, sortOverview, vacantOverviewPosition } from '../ui/workbenchOverviewState';

let adapter: FixtureReceiverAdapter, session: WorkbenchSession, ui: ReturnType<typeof mountWorkbench>;
const windowFor = (id: string): HTMLElement => document.querySelector<HTMLElement>(`[data-view-id="${id}"]`)!;
const overview = (): HTMLElement => document.querySelector<HTMLElement>('.mc-is-overview')!;
const cards = (): HTMLElement[] => [...overview().querySelectorAll<HTMLElement>('.mc-overview-card')];
const select = (attribute: string, value: string): void => {
  const field = overview().querySelector<HTMLSelectElement>(`[${attribute}]`)!;
  field.value = value; field.dispatchEvent(new Event('change', { bubbles: true }));
};
const detach = (uid: string): HTMLElement => {
  cards().find(card => card.dataset.uid === uid)!.querySelector<HTMLButtonElement>('[data-action=detail]')!.click();
  return windowFor(`detail-${uid}`);
};
beforeEach(() => {
  vi.useFakeTimers(); localStorage.clear(); setLang('en'); setTheme('dark');
  adapter = new FixtureReceiverAdapter(16, 10); session = new WorkbenchSession(adapter);
  const host = document.createElement('div'); document.body.append(host); ui = mountWorkbench(session, host); ui.showChannels();
});
afterEach(() => {
  ui.dispose(); session.dispose(); adapter.dispose(); document.body.replaceChildren(); setLang('en'); setTheme('dark'); vi.restoreAllMocks(); vi.useRealTimers();
});
describe('channel overview ownership', () => {
  it('shows creation times and accessible icon-only monitoring without sending commands', () => {
    const send = vi.spyOn(session, 'send');
    expect(cards()).toHaveLength(3);
    expect(overview().querySelector('[data-card-capacity]')!.textContent).toContain('3 / 16');
    expect(cards().every(card => card.querySelector('time')?.hasAttribute('datetime'))).toBe(true);
    const listen = cards()[0].querySelector<HTMLButtonElement>('[data-action=listen]')!;
    expect(listen.textContent).toBe(''); expect(listen.getAttribute('aria-label')).toBe('Listen');
    listen.click(); expect(session.listening('fixture-c1')).toBe(true); expect(send).not.toHaveBeenCalled();
  });
  it('takes a card out and returns the same receiver through Dock without retuning or changing monitoring', () => {
    session.monitor('fixture-c1', { enabled: true, gain: .43 });
    const before = clone(session.snapshot), dispatch = vi.spyOn(adapter, 'dispatch');
    const detail = detach('fixture-c1');
    expect(cards().map(card => card.dataset.uid)).toEqual(['fixture-c2', 'fixture-c3']);
    expect(detail.hidden).toBe(false); expect(detail.querySelector<HTMLButtonElement>('[data-dock]')!.hidden).toBe(false);
    expect(overview().querySelector('[data-card-capacity]')!.textContent).toContain('3 / 16');
    detail.querySelector<HTMLButtonElement>('[data-dock]')!.click();
    expect(detail.hidden).toBe(true); expect(cards()).toHaveLength(3);
    expect(session.snapshot).toEqual(before); expect(session.route('fixture-c1')).toMatchObject({ enabled: true, gain: .43 });
    expect(dispatch).not.toHaveBeenCalled();
    ui.recall(); expect(detail.hidden).toBe(true);
  });
  it('returns every detached channel, including a browser popup, to the unique overview', () => {
    const first = detach('fixture-c1'), second = detach('fixture-c2');
    const frame = document.createElement('iframe'); document.body.append(frame);
    const child = frame.contentWindow!; vi.spyOn(child, 'close').mockImplementation(() => {}); vi.spyOn(child, 'focus').mockImplementation(() => {});
    vi.spyOn(window, 'open').mockReturnValue(child);
    first.querySelector<HTMLButtonElement>('[data-pop]')!.click();
    expect(first.ownerDocument).toBe(child.document);
    overview().querySelector<HTMLButtonElement>('[data-overview-collect]')!.click();
    expect(first.ownerDocument).toBe(document); expect(first.hidden).toBe(true); expect(second.hidden).toBe(true);
    expect(cards()).toHaveLength(3); expect(document.querySelectorAll('.mc-is-overview')).toHaveLength(1);
    expect(child.close).toHaveBeenCalled();
  });
  it('transforms the chosen window into the only overview and restores the previous editor', () => {
    const original = overview(), detail = detach('fixture-c1');
    detail.querySelector<HTMLButtonElement>('[data-overview-toggle]')!.click();
    expect(overview()).toBe(detail); expect(original.classList.contains('mc-is-overview')).toBe(false);
    expect(document.querySelectorAll('.mc-card-overview')).toHaveLength(1); expect(cards()).toHaveLength(3);
    ui.showChannels(); expect(overview()).toBe(detail);
    cards().find(card => card.dataset.uid === 'fixture-c1')!.querySelector<HTMLButtonElement>('[data-action=detail]')!.click();
    expect(overview()).toBe(original); expect(detail.classList.contains('mc-is-overview')).toBe(false);
    expect(cards().map(card => card.dataset.uid)).not.toContain('fixture-c1');
  });
  it('moves the live overview DOM between documents and synchronizes language and theme', () => {
    const pane = overview(), card = cards()[0], canvas = card.querySelector('canvas');
    const frame = document.createElement('iframe'); document.body.append(frame);
    const child = frame.contentWindow!; vi.spyOn(child, 'close').mockImplementation(() => {}); vi.spyOn(child, 'focus').mockImplementation(() => {});
    vi.spyOn(window, 'open').mockReturnValue(child);
    pane.querySelector<HTMLButtonElement>('[data-pop]')!.click();
    setLang('zh'); setTheme('light');
    expect(pane.ownerDocument).toBe(child.document); expect(child.document.documentElement.dataset.theme).toBe('light');
    expect(pane.querySelector('.mc-head strong')!.textContent).toBe('全部信道概览');
    expect(pane.querySelector('[data-overview-layout]')!.textContent).toContain('8 列');
    expect(pane.querySelector('canvas')).toBe(canvas);
    pane.querySelector<HTMLButtonElement>('[data-dock]')!.click();
    expect(pane.ownerDocument).toBe(document); expect(pane.querySelector('.mc-overview-card')).toBe(card);
  });
  it('keeps draft edits when switching into and out of overview', () => {
    ui.showOverview(); const pane = windowFor('overview-follow');
    const name = pane.querySelector<HTMLInputElement>('[data-channel-name]')!;
    name.value = 'Unsaved name'; name.dispatchEvent(new Event('input', { bubbles: true }));
    pane.querySelector<HTMLButtonElement>('[data-overview-toggle]')!.click();
    pane.querySelector<HTMLButtonElement>('[data-overview-toggle]')!.click();
    expect(pane.querySelector<HTMLInputElement>('[data-channel-name]')!.value).toBe('Unsaved name');
  });
});
describe('overview layouts and ordering', () => {
  it('supports all eight column presets, free positioning and persistent layout selection', () => {
    for (let n = 1; n <= 8; n++) { select('data-overview-layout', String(n)); expect(overview().querySelector('.mc-card-grid')!.getAttribute('data-layout')).toBe(String(n)); }
    select('data-overview-layout', 'free');
    const card = cards()[0]; card.focus(); card.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', altKey: true, bubbles: true }));
    expect(cards()[0].style.left).toBe('480px');
    const saved = loadOverviewState('fixture'); expect(saved.layout).toBe('free'); expect(saved.positions['fixture-c1'].x).toBe(480);
    select('data-overview-layout', 'auto'); expect(overview().querySelector('.mc-card-grid')!.classList.contains('mc-card-free')).toBe(false);
  });
  it('sorts frequency and creation time in both directions while keeping the card DOM', () => {
    const first = cards()[0]; select('data-overview-sort', 'frequency');
    expect(cards().map(card => card.dataset.uid)).toEqual(['fixture-c2', 'fixture-c1', 'fixture-c3']);
    overview().querySelector<HTMLButtonElement>('[data-overview-direction]')!.click();
    expect(cards().map(card => card.dataset.uid)).toEqual(['fixture-c3', 'fixture-c1', 'fixture-c2']);
    select('data-overview-sort', 'created'); expect(cards().map(card => card.dataset.uid)).toEqual(['fixture-c3', 'fixture-c2', 'fixture-c1']);
    expect(cards().find(card => card.dataset.uid === 'fixture-c1')).toBe(first);
  });
  it('sorts Chinese names by pinyin and leaves unknown creation dates last in either direction', () => {
    const rows = clone(session.snapshot.receivers), state = loadOverviewState('fixture');
    rows.forEach((r, i) => r.confirmed.name = ['张三', '阿尔法', '北京'][i]);
    state.sort = 'name'; state.descending = false;
    expect(sortOverview(rows, state, () => 0).map(r => r.confirmed.name)).toEqual(['阿尔法', '北京', '张三']);
    state.descending = true; expect(sortOverview(rows, state, () => 0).map(r => r.confirmed.name)).toEqual(['张三', '北京', '阿尔法']);
    rows[0].created_at_ms = undefined; state.sort = 'created';
    expect(sortOverview(rows, state, () => 0).at(-1)!.receiver_uid).toBe(rows[0].receiver_uid);
    state.descending = false; expect(sortOverview(rows, state, () => 0).at(-1)!.receiver_uid).toBe(rows[0].receiver_uid);
  });
  it('does not reuse old placements after a service replacement or retain deleted receivers', () => {
    const state = loadOverviewState('fixture'); state.order = ['fixture-c2', 'fixture-c1', 'gone']; state.positions = { gone: { x: 9, y: 9 } }; state.layout = 8;
    reconcileOverview(state, session.snapshot.receivers, 'fixture'); expect(state.order).toEqual(['fixture-c2', 'fixture-c1', 'fixture-c3']); expect(state.positions.gone).toBeUndefined();
    saveOverviewState(state); const restarted = loadOverviewState('new-service'); expect(restarted.layout).toBe(8); expect(restarted.order).toEqual([]); expect(restarted.positions).toEqual({});
  });
});
describe('free canvas dragging', () => {
  const grid = (): HTMLElement => overview().querySelector('.mc-card-grid')!;
  const pointer = (target: EventTarget, type: string, x: number, y: number): void => {
    target.dispatchEvent(new MouseEvent(type, { button: 0, clientX: x, clientY: y, bubbles: true, cancelable: true }));
  };
  const geometry = (): HTMLElement => {
    select('data-overview-layout', 'free');
    const card = cards()[0];
    vi.spyOn(card, 'getBoundingClientRect').mockReturnValue({ left: 100, top: 100, right: 332, bottom: 268, width: 232, height: 168, x: 100, y: 100, toJSON() {} });
    const bounds = { left: 100, top: 100, right: 900, bottom: 700, width: 800, height: 600, x: 100, y: 100, toJSON() {} };
    vi.spyOn(grid(), 'getBoundingClientRect').mockReturnValue(bounds);
    vi.spyOn(overview().querySelector<HTMLElement>('.mc-card-scroll')!, 'getBoundingClientRect').mockReturnValue(bounds);
    return card;
  };
  it('uses independent card-width and card-height steps, and avoids occupied cells', () => {
    expect(snapOverviewPosition({ x: 252, y: 190 }, true)).toEqual({ x: 240, y: 176 });
    expect(snapOverviewPosition({ x: 252, y: 190 }, false)).toEqual({ x: 252, y: 190 });
    expect(vacantOverviewPosition({ x: 0, y: 0 }, [{ x: 0, y: 0 }])).toEqual({ x: 0, y: cardGrid.y });
    expect(snapOverviewPosition({ x: -20, y: 100_001 }, true)).toEqual({ x: 0, y: Math.floor(100_000 / cardGrid.y) * cardGrid.y });
  });
  it('shows card-sized cells only during a snapped drag and persists the previewed drop position', () => {
    const card = geometry();
    expect(grid().classList.contains('mc-card-snap-active')).toBe(false);
    pointer(card, 'pointerdown', 120, 120); pointer(window, 'pointermove', 370, 310);
    expect(grid().classList.contains('mc-card-snap-active')).toBe(true);
    expect(card.style.left).toBe('240px'); expect(card.style.top).toBe('176px');
    const ghost = document.querySelector<HTMLElement>('.mc-card-drag-ghost')!;
    expect(ghost.style.left).toBe('350px'); expect(ghost.style.top).toBe('290px');
    pointer(window, 'pointerup', 370, 310);
    expect(grid().classList.contains('mc-card-snap-active')).toBe(false);
    expect(document.querySelector('.mc-card-drag-ghost')).toBeNull();
    expect(loadOverviewState('fixture').positions['fixture-c1']).toEqual({ x: 240, y: 176 });
  });
  it('moves freely without cells when snapping is disabled', () => {
    const card = geometry(), toggle = overview().querySelector<HTMLInputElement>('[data-overview-snap]')!;
    toggle.checked = false; toggle.dispatchEvent(new Event('change', { bubbles: true }));
    pointer(card, 'pointerdown', 120, 120); pointer(window, 'pointermove', 370, 310);
    expect(grid().classList.contains('mc-card-snap-active')).toBe(false);
    expect(card.style.left).toBe('250px'); expect(card.style.top).toBe('190px');
    pointer(window, 'pointerup', 370, 310);
    expect(loadOverviewState('fixture').positions['fixture-c1']).toEqual({ x: 250, y: 190 });
  });
  it('moves by one cell with the keyboard and skips occupied cells in the requested direction', () => {
    const card = geometry();
    card.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', altKey: true, bubbles: true }));
    expect(card.style.top).toBe('352px');
    expect(card.style.left).toBe('0px');
    card.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowUp', altKey: true, bubbles: true }));
    expect(card.style.top).toBe('0px');
  });
  it('aligns saved pixel placements without stacking cards into the same cell', () => {
    select('data-overview-layout', 'free');
    const points = cards().map(card => ({ x: Number.parseFloat(card.style.left), y: Number.parseFloat(card.style.top) }));
    expect(new Set(points.map(p => `${p.x},${p.y}`)).size).toBe(points.length);
    expect(points.every(p => p.x % cardGrid.x === 0 && p.y % cardGrid.y === 0)).toBe(true);
  });
  it.each(['pointercancel', 'blur'])('clears the grid and restores placement on %s', type => {
    const card = geometry(), before = loadOverviewState('fixture').positions['fixture-c1'];
    pointer(card, 'pointerdown', 120, 120); pointer(window, 'pointermove', 370, 310);
    expect(grid().classList.contains('mc-card-snap-active')).toBe(true);
    window.dispatchEvent(new Event(type));
    expect(grid().classList.contains('mc-card-snap-active')).toBe(false);
    expect(document.querySelector('.mc-card-drag-ghost')).toBeNull();
    expect(loadOverviewState('fixture').positions['fixture-c1']).toEqual(before);
  });
});
describe('card names and main spectrum selection', () => {
  const editName = (name: string): HTMLInputElement => {
    cards()[0].querySelector<HTMLButtonElement>('[data-card-rename]')!.click();
    const field = overview().querySelector<HTMLInputElement>('[data-card-name-edit]')!;
    field.value = name; field.dispatchEvent(new Event('input', { bubbles: true })); return field;
  };
  it('edits a name inline without changing identity, creation time or signal settings', async () => {
    const before = clone(session.snapshot.receivers[0]);
    const field = editName('  My receiver  '); session.notify();
    expect(overview().querySelector('[data-card-name-edit]')).toBe(field); expect(field.value).toBe('  My receiver  ');
    field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    await vi.advanceTimersByTimeAsync(100);
    expect(session.snapshot.receivers[0]).toEqual({ ...before, revision: session.snapshot.receivers[0].revision, confirmed: { ...before.confirmed, name: 'My receiver' } });
    expect(overview().querySelector('[data-card-name-edit]')).toBeNull();
  });
  it('cancels on Escape, rejects invalid names, and saves on blur', async () => {
    const dispatch = vi.spyOn(adapter, 'dispatch');
    let field = editName('cancel'); field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    expect(dispatch).not.toHaveBeenCalled();
    field = editName(''); field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    expect(field.validationMessage).not.toBe(''); expect(dispatch).not.toHaveBeenCalled();
    field.value = 'Saved on blur'; field.dispatchEvent(new Event('input', { bubbles: true })); field.blur();
    await vi.advanceTimersByTimeAsync(100); expect(session.snapshot.receivers[0].confirmed.name).toBe('Saved on blur');
  });
  it('retains a rejected name for correction and retry', async () => {
    adapter.inject('reject', 'fixture-c1');
    const field = editName('Retry name'); field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    await vi.advanceTimersByTimeAsync(100);
    expect(overview().querySelector<HTMLInputElement>('[data-card-name-edit]')!.value).toBe('Retry name');
    expect(field.readOnly).toBe(false);
    field.value = 'Accepted name'; field.dispatchEvent(new Event('input', { bubbles: true })); field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    await vi.advanceTimersByTimeAsync(100); expect(session.snapshot.receivers[0].confirmed.name).toBe('Accepted name');
  });
  it('tracks the main frequency across all matching cards independently of editor selection', () => {
    ui.dispose(); let frequency = session.snapshot.receivers[0].confirmed.baseband.frequency_hz;
    const host = document.createElement('div'); document.body.append(host);
    ui = mountWorkbench(session, host, undefined, () => frequency); ui.showChannels();
    session.snapshot = clone(session.snapshot); session.snapshot.receivers[2].confirmed.baseband.frequency_hz = frequency; session.notify();
    expect(cards().filter(card => card.dataset.frequencyMatch === 'true').map(card => card.dataset.uid)).toEqual(['fixture-c1', 'fixture-c3']);
    frequency = session.snapshot.receivers[1].confirmed.baseband.frequency_hz; vi.advanceTimersByTime(150);
    session.select('fixture-c3');
    expect(cards().filter(card => card.dataset.frequencyMatch === 'true').map(card => card.dataset.uid)).toEqual(['fixture-c2']);
  });
  it('snaps existing free placements when enabled and allows single-pixel movement when disabled', () => {
    select('data-overview-layout', 'free');
    const toggle = overview().querySelector<HTMLInputElement>('[data-overview-snap]')!;
    toggle.checked = false; toggle.dispatchEvent(new Event('change', { bubbles: true }));
    cards()[0].dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', altKey: true, bubbles: true }));
    expect(cards()[0].style.left).toBe('1px');
    const on = overview().querySelector<HTMLInputElement>('[data-overview-snap]')!;
    on.checked = true; on.dispatchEvent(new Event('change', { bubbles: true }));
    expect(cards()[0].style.left).toBe('0px');
  });
});
it('releases waveform probes on hidden cards and disposal without creating audio consumers', () => {
  ui.dispose(); const waveform = vi.fn((_: string, samples: Float32Array | null) => !!samples);
  const manager = createWorkbenchOverview(session, { available: () => true, recording: () => undefined, detached: () => false,
    fit: vi.fn(), dockAll: vi.fn(), readouts: { results: () => ({ entries: [], evicted: 0 }), waveform } });
  const surface = document.createElement('section'); surface.className = 'mc-window'; surface.innerHTML = '<header class="mc-head"><div class="mc-window-actions"></div></header><div class="mc-body"></div>'; document.body.append(surface);
  manager.attach(surface.querySelector('.mc-body')!, surface.querySelector('.mc-head')!);
  expect(waveform).toHaveBeenCalledWith('fixture-c1', expect.any(Float32Array));
  surface.hidden = true; vi.advanceTimersByTime(100); manager.paint();
  expect(waveform).toHaveBeenCalledWith('fixture-c1', null);
  manager.dispose();
});
