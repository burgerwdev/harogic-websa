import type { ReceiverState } from '../core/multichannel';

export type OverviewLayout = 'auto' | 'free' | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8;
export type OverviewSort = 'created' | 'frequency' | 'name' | 'type' | 'auto' | 'manual';
export interface CardPosition { x: number; y: number }
export interface OverviewState {
  service: string;
  layout: OverviewLayout;
  sort: OverviewSort;
  descending: boolean;
  snap: boolean;
  bandOnly: boolean;
  order: string[];
  positions: Record<string, CardPosition>;
}
const key = ['websa', 'workbench', 'overview', 'v1'].join('-');
const sorts: OverviewSort[] = ['created', 'frequency', 'name', 'type', 'auto', 'manual'];
const collator = new Intl.Collator('zh-CN-u-co-pinyin', { numeric: true, sensitivity: 'base' });
export const cardSize = { width: 232, height: 168, gap: 8, padding: 12 };
export const cardGrid = { x: cardSize.width + cardSize.gap, y: cardSize.height + cardSize.gap };
export function snapOverviewPosition(point: CardPosition, enabled: boolean): CardPosition {
  const coordinate = (value: number, pitch: number): number => {
    const step = enabled ? pitch : 1;
    return Math.max(0, Math.min(Math.floor(100_000 / step) * step, Math.round(value / step) * step));
  };
  return { x: coordinate(point.x, cardGrid.x), y: coordinate(point.y, cardGrid.y) };
}
/** Find the nearest empty card-sized cell without moving any other card. */
export function vacantOverviewPosition(point: CardPosition, occupied: readonly CardPosition[]): CardPosition {
  const target = snapOverviewPosition(point, true);
  const available = (p: CardPosition): boolean => p.x >= 0 && p.y >= 0 && p.x <= 100_000 && p.y <= 100_000
    && !occupied.some(other => Math.abs(other.x - p.x) < cardSize.width && Math.abs(other.y - p.y) < cardSize.height);
  if (available(target)) return target;
  let best = target, distance = Infinity;
  for (let radius = 1; radius <= occupied.length * 2 + 1; radius++) {
    for (let y = -radius; y <= radius; y++) for (let x = -radius; x <= radius; x++) {
      if (Math.abs(x) !== radius && Math.abs(y) !== radius) continue;
      const candidate = { x: target.x + x * cardGrid.x, y: target.y + y * cardGrid.y };
      const nextDistance = (x * cardGrid.x) ** 2 + (y * cardGrid.y) ** 2;
      if (nextDistance < distance && available(candidate)) { best = candidate; distance = nextDistance; }
    }
    if (((radius + 1) * Math.min(cardGrid.x, cardGrid.y)) ** 2 >= distance) break;
  }
  return best;
}
export function receiverCreatedAt(receiver: ReceiverState): number | null {
  const value = Number(receiver.created_at_ms);
  return Number.isFinite(value) && value > 0 && value <= 8.64e15 ? value : null;
}
export function loadOverviewState(service: string): OverviewState {
  const state: OverviewState = { service, layout: 'auto', sort: 'created', descending: false, snap: true, bandOnly: false, order: [], positions: {} };
  try {
    const saved = JSON.parse(localStorage.getItem(key) ?? 'null');
    if (!saved || typeof saved !== 'object') return state;
    if (saved.layout === 'auto' || saved.layout === 'free' || Number.isInteger(saved.layout) && saved.layout >= 1 && saved.layout <= 8) state.layout = saved.layout;
    if (sorts.includes(saved.sort)) state.sort = saved.sort;
    if (typeof saved.descending === 'boolean') state.descending = saved.descending;
    if (typeof saved.snap === 'boolean') state.snap = saved.snap;
    if (typeof saved.bandOnly === 'boolean') state.bandOnly = saved.bandOnly;
    if (saved.service !== service) return state;
    if (Array.isArray(saved.order)) state.order = [...new Set(saved.order.filter((uid: unknown): uid is string => typeof uid === 'string'))].slice(0, 256) as string[];
    if (saved.positions && typeof saved.positions === 'object') for (const uid of state.order) {
      const point = saved.positions[uid];
      if (point && Number.isFinite(point.x) && Number.isFinite(point.y) && point.x >= 0 && point.y >= 0 && point.x <= 100_000 && point.y <= 100_000)
        Object.defineProperty(state.positions, uid, { value: { x: point.x, y: point.y }, enumerable: true, configurable: true, writable: true });
    }
  } catch { /* Storage is optional. */ }
  return state;
}
export function saveOverviewState(state: OverviewState): void {
  try { localStorage.setItem(key, JSON.stringify(state)); } catch { /* Storage is optional. */ }
}
export function reconcileOverview(state: OverviewState, receivers: readonly ReceiverState[], service: string): void {
  if (state.service !== service) { state.service = service; state.order = []; state.positions = {}; }
  const active = new Set(receivers.map(r => r.receiver_uid));
  state.order = state.order.filter(uid => active.has(uid));
  for (const uid of Object.keys(state.positions)) if (!active.has(uid)) delete state.positions[uid];
  for (const r of receivers) if (!state.order.includes(r.receiver_uid)) state.order.push(r.receiver_uid);
}
export function sortOverview(receivers: readonly ReceiverState[], state: OverviewState, priority: (uid: string) => number): ReceiverState[] {
  const rank = new Map(state.order.map((uid, i) => [uid, i]));
  return [...receivers].sort((a, b) => {
    let difference = 0;
    if (state.sort === 'created') {
      const left = receiverCreatedAt(a), right = receiverCreatedAt(b);
      if (left === null || right === null) return left === right ? a.display_number - b.display_number : left === null ? 1 : -1;
      difference = left - right;
    } else if (state.sort === 'frequency') difference = a.confirmed.baseband.frequency_hz - b.confirmed.baseband.frequency_hz;
    else if (state.sort === 'name') difference = collator.compare(a.confirmed.name, b.confirmed.name);
    else if (state.sort === 'type') difference = collator.compare(a.confirmed.processors[0]?.mode ?? 'iq', b.confirmed.processors[0]?.mode ?? 'iq');
    else if (state.sort === 'auto') difference = priority(b.receiver_uid) - priority(a.receiver_uid);
    else difference = (rank.get(a.receiver_uid) ?? 0) - (rank.get(b.receiver_uid) ?? 0);
    return difference ? difference * (state.descending && !['auto', 'manual'].includes(state.sort) ? -1 : 1) : a.display_number - b.display_number;
  });
}
export function moveOverviewCard(state: OverviewState, uid: string, target: string, after: boolean): void {
  if (uid === target || !state.order.includes(uid) || !state.order.includes(target)) return;
  state.order = state.order.filter(id => id !== uid);
  state.order.splice(state.order.indexOf(target) + Number(after), 0, uid);
  state.sort = 'manual';
}
