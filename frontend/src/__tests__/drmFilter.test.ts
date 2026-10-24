import { afterEach, describe, expect, it, vi } from 'vitest';
import { send } from '../core/wsSend';
import { applySdrDemod } from '../ui/controls';
import { sdrDemod, sdrIfbw } from '../ui/sdrState';

vi.mock('../core/wsSend', () => ({ send: vi.fn(), onCommandSent: vi.fn() }));

afterEach(() => {
	vi.mocked(send).mockClear();
	localStorage.clear();
});

describe('DRM filter control', () => {
	it('chooses enough DDC headroom instead of preserving an unrelated narrow analog IF setting', () => {
		sdrDemod.set('drm');
		sdrIfbw.set(6000);
		applySdrDemod();
		expect(sdrIfbw.get()).toBe(12000);
	});

	it('chooses the VHF 100 kHz DDC rate for DRM+ instead of inheriting DRM30', () => {
		sdrDemod.set('drmplus');
		sdrIfbw.set(12000);
		applySdrDemod();
		expect(sdrIfbw.get()).toBe(100000);
	});
});
