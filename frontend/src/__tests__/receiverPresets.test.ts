import { afterEach, expect, it, vi } from 'vitest';
import { clone } from '../core/multichannel';
import { FixtureReceiverAdapter } from '../dev/fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountReceiverPresets } from '../ui/receiverPresets';

const cleanups: (() => void)[] = [];
afterEach(() => { cleanups.reverse().forEach(fn => fn()); cleanups.length = 0; vi.unstubAllGlobals(); document.body.replaceChildren(); });
const mount = () => {
  const adapter = new FixtureReceiverAdapter(), session = new WorkbenchSession(adapter);
  session.snapshot.service_id = 'boot-a';
  const root = document.createElement('details'); root.open = true; document.body.append(root);
  const review = vi.fn(), dispose = mountReceiverPresets(root, session, 'fixture-c1', review);
  cleanups.push(() => adapter.dispose(), () => session.dispose(), dispose);
  return { session, root, review, dispose };
};
const reply = (value: unknown, status = 200) => ({ ok: status === 200, status, json: async () => value }) as Response;
const get = <T extends HTMLElement>(root: HTMLElement, key: string) => root.querySelector<T>(`[data-preset-${key}]`)!;

it('saves a confirmed revision and previews only tuning/processing, keeping target identity and stopped state', async () => {
  const settings = { purpose: 'iq', baseband: { frequency_hz: 101e6, passband_hz: 3000, transition_hz: 1000, minimum_rate_hz: 6000 }, processors: [],
    enabled: true, name: 'wrong name', receiver_uid: 'old-uid' };
  const data = { version: 1, revision: '9007199254740993', presets: [{ name: 'Saved IQ', settings }] };
  const fetch = vi.fn().mockResolvedValue(reply(data)); vi.stubGlobal('fetch', fetch);
  const { session, root, review } = mount();
  await vi.waitFor(() => expect(get<HTMLSelectElement>(root, 'list').options.length).toBe(2));
  session.snapshot.receivers[0].confirmed.enabled = false;
  const before = clone(session.snapshot), dispatch = vi.spyOn(session.adapter, 'dispatch');
  const list = get<HTMLSelectElement>(root, 'list'); list.value = 'Saved IQ'; list.dispatchEvent(new Event('change'));
  get<HTMLButtonElement>(root, 'load').click();
  expect(review).toHaveBeenCalledWith({ ...before.receivers[0].confirmed, purpose: 'iq', baseband: settings.baseband, processors: [] });
  expect(session.snapshot).toEqual(before); expect(dispatch).not.toHaveBeenCalled();
  get<HTMLInputElement>(root, 'name').value = 'New preset'; get<HTMLButtonElement>(root, 'save').click();
  await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));
  expect(JSON.parse(fetch.mock.calls[1][1].body)).toEqual({ type: 'save', name: 'New preset', expected_revision: data.revision,
    service_id: 'boot-a', receiver_uid: 'fixture-c1', receiver_revision: before.receivers[0].revision });
  expect(session.snapshot).toEqual(before);
});

it('requires refreshing after a concurrent preset change and never retries the mutation automatically', async () => {
  const fetch = vi.fn().mockResolvedValueOnce(reply({ version: 1, revision: '0', presets: [{ name: 'older', settings: {
    purpose: 'iq', baseband: { frequency_hz: 7.12e6, passband_hz: 3000, transition_hz: 1000, minimum_rate_hz: 6000 }, processors: [],
  } }] }))
    .mockResolvedValueOnce(reply({ error: 'presets changed' }, 409)); vi.stubGlobal('fetch', fetch);
  const { root } = mount();
  await vi.waitFor(() => expect(get<HTMLButtonElement>(root, 'save').disabled).toBe(false));
  get<HTMLSelectElement>(root, 'list').value = 'older';
  get<HTMLButtonElement>(root, 'save').click();
  await vi.waitFor(() => expect(root.textContent).toContain('presets changed'));
  expect(get<HTMLButtonElement>(root, 'save').disabled).toBe(true);
  expect(get<HTMLButtonElement>(root, 'load').disabled).toBe(true);
  expect(get<HTMLButtonElement>(root, 'delete').disabled).toBe(true);
  expect(fetch).toHaveBeenCalledTimes(2);
});

it('restores the saved continuity policy with tuning and processing', async () => {
  const settings = { purpose: 'iq', baseband: { frequency_hz: 101e6, passband_hz: 3000, transition_hz: 1000, minimum_rate_hz: 6000 }, processors: [], continuity: 'strict' };
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(reply({ version: 1, revision: '1', presets: [{ name: 'Strict', settings }] })));
  const { root, review } = mount();
  await vi.waitFor(() => expect(get<HTMLSelectElement>(root, 'list').options.length).toBe(2));
  const list = get<HTMLSelectElement>(root, 'list'); list.value = 'Strict'; list.dispatchEvent(new Event('change'));
  get<HTMLButtonElement>(root, 'load').click();
  expect(review).toHaveBeenCalledWith(expect.objectContaining({ continuity: 'strict' }));
});

it('aborts a retired view request and rejects its late response', async () => {
  let resolve!: (response: Response) => void;
  const fetch = vi.fn((_url: string, _init: RequestInit) => new Promise<Response>(r => { resolve = r; })); vi.stubGlobal('fetch', fetch);
  const { session, root, review, dispose } = mount();
  session.snapshot.service_id = 'boot-b'; dispose();
  resolve(reply({ revision: '1', presets: [{ name: 'old', settings: {} }] }));
  await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
  expect(get<HTMLSelectElement>(root, 'list').options.length).toBe(1);
  expect(review).not.toHaveBeenCalled();
  expect(fetch.mock.calls[0][1].signal?.aborted).toBe(true);
});

it('discards a previous service response while the view remains mounted', async () => {
  let resolve!: (response: Response) => void;
  vi.stubGlobal('fetch', vi.fn(() => new Promise<Response>(r => { resolve = r; })));
  const { session, root, review } = mount();
  session.snapshot.service_id = 'boot-b';
  resolve(reply({ revision: '1', presets: [{ name: 'old', settings: {} }] }));
  await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
  expect(get<HTMLSelectElement>(root, 'list').options.length).toBe(1);
  expect(review).not.toHaveBeenCalled();
});

it('retires all preset actions when the owning editor is disposed', async () => {
  const settings = { purpose: 'iq', baseband: { frequency_hz: 7.12e6, passband_hz: 3000, transition_hz: 1000, minimum_rate_hz: 6000 }, processors: [] };
  const fetch = vi.fn().mockResolvedValue(reply({ version: 1, revision: '1', presets: [{ name: 'Saved', settings }] }));
  vi.stubGlobal('fetch', fetch);
  const { root, review, dispose } = mount();
  await vi.waitFor(() => expect(get<HTMLSelectElement>(root, 'list').options.length).toBe(2));
  const list = get<HTMLSelectElement>(root, 'list'); list.value = 'Saved'; list.dispatchEvent(new Event('change'));
  get<HTMLInputElement>(root, 'name').value = 'New name';
  dispose();
  get<HTMLButtonElement>(root, 'load').click(); get<HTMLButtonElement>(root, 'save').click();
  get<HTMLButtonElement>(root, 'refresh').click();
  await Promise.resolve();
  expect(review).not.toHaveBeenCalled();
  expect(fetch).toHaveBeenCalledOnce();
});

it('aborts an owned preset request immediately when the service changes before repaint', async () => {
  let resolve!: (response: Response) => void;
  const fetch = vi.fn((_url: string, _init: RequestInit) => new Promise<Response>(done => { resolve = done; })); vi.stubGlobal('fetch', fetch);
  const { session, root } = mount();
  session.snapshot.service_id = 'boot-b'; session.notify();
  try {
    expect(fetch.mock.calls[0][1].signal?.aborted).toBe(true);
    expect(get<HTMLButtonElement>(root, 'refresh').disabled).toBe(true);
  } finally { resolve(reply({ version: 1, revision: '1', presets: [] })); }
});

it('rejects malformed saved settings without publishing them and recovers after an explicit refresh', async () => {
  const fetch = vi.fn().mockResolvedValueOnce(reply({ version: 1, revision: '4', presets: [
    { name: 'Broken', settings: { purpose: 'listen', baseband: {}, processors: [null] } },
  ] })).mockResolvedValue(reply({ version: 1, revision: '9007199254740993', presets: [{ name: 'Valid', settings: {
    purpose: 'iq', baseband: { frequency_hz: 7.12e6, passband_hz: 3000, transition_hz: 1000, minimum_rate_hz: 6000 }, processors: [],
  } }] })); vi.stubGlobal('fetch', fetch);
  const { root, review } = mount();
  await vi.waitFor(() => expect(get<HTMLButtonElement>(root, 'refresh').disabled).toBe(false));
  root.dispatchEvent(new Event('toggle'));
  expect(fetch).toHaveBeenCalledOnce();
  const list = get<HTMLSelectElement>(root, 'list'); list.value = 'Broken'; list.dispatchEvent(new Event('change'));
  get<HTMLButtonElement>(root, 'load').click();
  expect(review).not.toHaveBeenCalled(); expect(list.options).toHaveLength(1);
  expect(get<HTMLButtonElement>(root, 'save').disabled).toBe(true);
  get<HTMLButtonElement>(root, 'refresh').click();
  await vi.waitFor(() => expect(list.options).toHaveLength(2));
  list.value = 'Valid'; list.dispatchEvent(new Event('change')); get<HTMLButtonElement>(root, 'load').click();
  expect(review).toHaveBeenCalledWith(expect.objectContaining({ purpose: 'iq', processors: [] }));
  expect(fetch).toHaveBeenCalledTimes(2);
});

it.each(['purpose', 'continuity', 'processor kind'] as const)('rejects an array in the saved %s enum without publishing it', async field => {
  const processor: Record<string, unknown> = { id: 'primary', kind: 'demodulator', mode: 'nfm', minimum_rate_hz: 0 };
  const settings: Record<string, unknown> = {
    purpose: 'listen', continuity: 'strict',
    baseband: { frequency_hz: 7.12e6, passband_hz: 3000, transition_hz: 1000, minimum_rate_hz: 6000 },
    processors: [processor],
  };
  if (field === 'processor kind') processor.kind = ['demodulator'];
  else settings[field] = [settings[field]];
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(reply({ version: 1, revision: '1', presets: [{ name: 'Broken', settings }] })));
  const { root, review } = mount();
  await vi.waitFor(() => expect(get<HTMLButtonElement>(root, 'refresh').disabled).toBe(false));
  const list = get<HTMLSelectElement>(root, 'list'); list.value = 'Broken'; list.dispatchEvent(new Event('change'));
  get<HTMLButtonElement>(root, 'load').click();
  expect(review).not.toHaveBeenCalled();
  expect(list.options).toHaveLength(1);
  expect(get<HTMLButtonElement>(root, 'save').disabled).toBe(true);
});
