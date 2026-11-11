import { afterEach, expect, it, vi } from 'vitest';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import type { ReceiverState } from '../core/multichannel';
import { ReceiverDiagnostics, receiverDiagnosticDetails } from '../ui/receiverDiagnostics';

const fixture = new FixtureReceiverAdapter();
const row = fixture.snapshot().receivers[0]; fixture.dispose();
afterEach(() => vi.useRealTimers());

it('fetches once on expansion, never polls, and refreshes only on request', async () => {
  vi.useFakeTimers();
  const load = vi.fn(async (_uid: string, _signal: AbortSignal) => row), changed = vi.fn();
  const details = new ReceiverDiagnostics(load, changed);
  details.update(row, 'boot-A', false); await vi.advanceTimersByTimeAsync(10000);
  expect(load).not.toHaveBeenCalled();
  details.update(row, 'boot-A', true); await vi.advanceTimersByTimeAsync(0);
  expect(details.value).toBe(row); expect(changed).toHaveBeenCalledOnce();
  await vi.advanceTimersByTimeAsync(60000); expect(load).toHaveBeenCalledOnce();
  details.update({ ...row, revision: '99', processing_runtime: { state: 'active', processing_generation: '42', oldest_input_wait_ns: 0, input_held_bytes: 0, input_held_media_ns: 0 } }, 'boot-A', true);
  expect(load).toHaveBeenCalledOnce();
  details.refresh(); await vi.advanceTimersByTimeAsync(0);
  expect(load).toHaveBeenCalledTimes(2);
  const signal = load.mock.calls[0]?.[1];
  details.update(row, 'boot-A', false); await vi.advanceTimersByTimeAsync(10000);
  expect(load).toHaveBeenCalledTimes(2); expect(details.value).toBeUndefined();
  expect(signal?.aborted).toBe(true); details.dispose();
});

it('rejects replies from a previous receiver, service, or closed view', async () => {
  vi.useFakeTimers();
  const replies: ((row: ReceiverState) => void)[] = [];
  const load = vi.fn((_uid: string, _signal: AbortSignal) => new Promise<ReceiverState>(resolve => replies.push(resolve)));
  const changed = vi.fn(), details = new ReceiverDiagnostics(load, changed);
  details.update(row, 'boot-A', true);
  const next = { ...row, receiver_uid: 'replacement' };
  details.update(next, 'boot-A', true);
  replies[0](row); await vi.advanceTimersByTimeAsync(0);
  expect(details.value).toBeUndefined(); expect(changed).not.toHaveBeenCalled();
  details.update(next, 'boot-B', true);
  replies[1](next); await vi.advanceTimersByTimeAsync(0);
  expect(details.value).toBeUndefined();
  details.dispose(); replies[2](next); await vi.advanceTimersByTimeAsync(10000);
  expect(changed).not.toHaveBeenCalled(); expect(load).toHaveBeenCalledTimes(3);
});

it('formats a requested diagnostic snapshot only once across live renders', () => {
  const detail = { ...row, processing_statistics: Array.from({ length: 128 }, (_, generation) => ({ generation })) };
  const stringify = vi.spyOn(JSON, 'stringify');
  try {
    const html = receiverDiagnosticDetails(detail);
    for (let i = 0; i < 20; i++) expect(receiverDiagnosticDetails(detail)).toBe(html);
    expect(stringify).toHaveBeenCalledOnce();
    expect(html).toContain('refresh-diagnostics');
    receiverDiagnosticDetails(undefined);
    expect(stringify).toHaveBeenCalledOnce();
  } finally { stringify.mockRestore(); }
});
