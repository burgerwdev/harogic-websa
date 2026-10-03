/**
 * A trapped DSP worker must not brick the session.
 *
 * A wasm trap kills the worker instance. The reference then points at a dead port, so every
 * later mode change was ignored and only a page refresh helped (reported from the bench: pick
 * DRM, then AM, and the audio never came back). `onerror` now drops the worker, and the next
 * configure starts a fresh one.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';

class FakeWorker {
	static instances: FakeWorker[] = [];
	onerror: ((event: ErrorEvent) => void) | null = null;
	onmessage: ((event: MessageEvent) => void) | null = null;
	posted: unknown[] = [];
	terminated = false;

	constructor() {
		FakeWorker.instances.push(this);
	}

	postMessage(message: unknown): void {
		this.posted.push(message);
	}

	terminate(): void {
		this.terminated = true;
	}
}

describe('DSP worker recovery', () => {
	beforeEach(() => {
		FakeWorker.instances = [];
		(globalThis as unknown as { Worker: unknown }).Worker = FakeWorker;
		vi.resetModules();
	});

	it('starts a new worker after the running one traps', async () => {
		const stream = await import('../sdr/iqStream');
		const params = {
			mode: 'drm',
			fsIn: 48828.125,
			outRate: 48000,
			ifBw: 12000,
			pitch: 700,
			deemphUs: -1,
		};
		stream.setSdrIqEnabled(true);
		expect(FakeWorker.instances.length).toBe(1);
		stream.configureSdrPipeline(params as never);
		expect(FakeWorker.instances[0].posted.some((m) => (m as { type: string }).type === 'configure')).toBe(true);

		// The trap.
		FakeWorker.instances[0].onerror?.({ message: 'memory access out of bounds' } as ErrorEvent);

		// The next backend status round must reach a live worker.
		stream.configureSdrPipeline({ ...params, mode: 'am' } as never);
		expect(FakeWorker.instances.length).toBe(2);
		const fresh = FakeWorker.instances[1];
		expect(fresh.posted.some((m) => (m as { type: string }).type === 'enabled')).toBe(true);
		const configure = fresh.posted.find(
			(m) => (m as { type: string }).type === 'configure',
		) as { params: { mode: string } } | undefined;
		expect(configure?.params.mode).toBe('am');
	});
});
