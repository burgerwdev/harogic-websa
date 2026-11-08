import type { RecordingTask, ReceiverState, ServiceSnapshot } from '../core/multichannel';
import { getLang } from '../core/i18n';
import { frequencyLabel } from './frequencyInput';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
export const recordingFinished = (task: RecordingTask): boolean => ['completed', 'failed'].includes(task.state);
export const recordingStateLabel = (state: string): string => ({ preparing: l('Preparing', '准备中'), waiting: l('Waiting', '等待中'), recording: l('Recording', '录制中'), finalizing: l('Saving', '保存中'), completed: l('Completed', '已完成'), failed: l('Failed', '失败') })[state] ?? state;
export const recordingBytes = (task: RecordingTask): bigint => task.tracks.reduce((sum, track) => sum + BigInt(track.stored_bytes), 0n);
export function recordingSize(value: bigint): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB', 'EB'];
  let unit = 0, divisor = 1n;
  while (unit < units.length - 1 && value >= divisor * 1000n) { unit++; divisor *= 1000n; }
  if (unit > 0 && unit < units.length - 1 && (value * 10n + divisor / 2n) / divisor >= 10000n) { unit++; divisor *= 1000n; }
  const amount = unit ? Number((value * 10n + divisor / 2n) / divisor) / 10 : Number(value);
  return `${new Intl.NumberFormat(getLang() === 'zh' ? 'zh-CN' : 'en', { maximumFractionDigits: 1 }).format(amount)} ${units[unit]}`;
}
export function recordingDuration(task: RecordingTask): string {
  const track = task.tracks.find(t => t.kind === 'audio') ?? task.tracks.find(t => t.kind === 'iq');
  if (!track) return '';
  const seconds = Math.floor(track.segments.reduce((sum, s) => sum + (s.actual_rate_hz ? Number(s.samples) / s.actual_rate_hz : 0), 0));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
}
export function recordingDate(task: RecordingTask): string {
  return task.created_at_ms && Number(task.created_at_ms) > 0 ? new Date(Number(task.created_at_ms)).toLocaleString(getLang() === 'zh' ? 'zh-CN' : 'en-GB', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', year: 'numeric', hour12: false }) : '';
}
export function recordingFrequencies(task: RecordingTask): number[] {
  return [...new Set(task.tracks.flatMap(t => t.segments).map(s => s.frequency_hz).filter((hz): hz is number => typeof hz === 'number' && Number.isFinite(hz)).map(hz => Math.round(hz)))];
}
export function recordingAtFrequency(task: RecordingTask, receiver: ReceiverState): boolean {
  const frequencies = recordingFrequencies(task);
  return frequencies.includes(Math.round(receiver.confirmed.baseband.frequency_hz))
    || !frequencies.length && task.receiver_uid === receiver.receiver_uid;
}
export function recordingsForFrequency(snapshot: ServiceSnapshot, receiver: ReceiverState): RecordingTask[] {
  return snapshot.recordings.filter(task => recordingAtFrequency(task, receiver)
    || !recordingFinished(task) && task.receiver_uid === receiver.receiver_uid).sort(newestRecordingFirst);
}
export const newestRecordingFirst = (a: RecordingTask, b: RecordingTask): number => Number(b.created_at_ms ?? 0) - Number(a.created_at_ms ?? 0) || b.task_id.localeCompare(a.task_id, undefined, { numeric: true });
export function recordingName(task: RecordingTask, snapshot: ServiceSnapshot): string {
  return task.name || task.receiver_name || snapshot.receivers.find(r => r.receiver_uid === task.receiver_uid)?.confirmed.name
    || (recordingFrequencies(task).length ? frequencyLabel(recordingFrequencies(task)[0]) : l('Untitled recording', '未命名录制'));
}
export function recordingChannel(task: RecordingTask, snapshot: ServiceSnapshot): string {
  const receiver = snapshot.receivers.find(r => r.receiver_uid === task.receiver_uid);
  return task.receiver_name || receiver?.confirmed.name || task.name || l('Archived channel', '历史信道');
}
/** Unit suffixes are numeric prefixes: 14M finds 14.x MHz, 14.2M finds 14.2x MHz. */
export function recordingMatches(task: RecordingTask, query: string, snapshot: ServiceSnapshot): boolean {
  const frequencies = recordingFrequencies(task);
  const receiver = snapshot.receivers.find(r => r.receiver_uid === task.receiver_uid);
  if (!frequencies.length && receiver) frequencies.push(Math.round(receiver.confirmed.baseband.frequency_hz));
  const names = snapshot.receivers.filter(r => r.receiver_uid === task.receiver_uid || recordingAtFrequency(task, r)).map(r => `${r.confirmed.name} C${r.display_number}`);
  const text = [task.name, task.receiver_name, task.task_id, ...names, ...task.tracks.flatMap(t => t.segments.map(s => s.mode)),
    ...frequencies.flatMap(hz => [hz, hz / 1e3, hz / 1e6, hz / 1e9])].join(' ').toLowerCase();
  let matches = true;
  const words = query.trim().toLowerCase().replace(/(\d+(?:\.\d+)?)\s*(ghz|mhz|khz|hz|g|m|k)\b/g, (_, value: string, unit: string) => {
    const scale = unit.startsWith('g') ? 1e9 : unit.startsWith('m') ? 1e6 : unit.startsWith('k') ? 1e3 : 1;
    const lower = Number(value) * scale, width = Math.max(1, scale / 10 ** (value.split('.')[1]?.length ?? 0));
    matches &&= frequencies.some(hz => hz >= Math.round(lower) && hz < Math.round(lower) + width);
    return ' ';
  });
  return matches && words.split(/\s+/).every(word => text.includes(word));
}
