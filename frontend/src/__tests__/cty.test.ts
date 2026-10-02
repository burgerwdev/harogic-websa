import { describe, expect, it } from 'vitest';
import { countryFor } from '../sdr/cty';

describe('countryFor', () => {
	it('maps the sample data’s callsigns to their countries (zh display name)', () => {
		expect(countryFor('BG7BVP')?.zh).toBe('中国');
		expect(countryFor('BG9MLT')?.zh).toBe('中国');
		expect(countryFor('SP5ALV')?.zh).toBe('波兰');
		expect(countryFor('4S6ARW')?.zh).toBe('斯里兰卡');
		expect(countryFor('PB0C')?.zh).toBe('荷兰');
		expect(countryFor('DC5RE')?.zh).toBe('德国');
	});

	it('prefers the longest prefix (a specific block over its parent)', () => {
		// `UA9` is Asiatic Russia while plain `UA` is European Russia - both are Russia, so the
		// assertion here pins the *entity* it resolved to, not just the shared zh name.
		expect(countryFor('UA9ABC')?.name).toBe('Asiatic Russia');
		expect(countryFor('UA1ABC')?.name).toBe('European Russia');
		// `TA1` is European Turkey, `TA2` Asiatic Turkey.
		expect(countryFor('TA1ABC')?.name).toBe('European Turkey');
		expect(countryFor('TA2ABC')?.name).toBe('Asiatic Turkey');
	});

	it('honours an exact callsign override', () => {
		// cty.dat pins `ZS1BAK/L` to South Africa even though it is a portable call.
		expect(countryFor('ZS1BAK/L')?.name).toBe('South Africa');
	});

	it('strips a portable suffix for the prefix match', () => {
		expect(countryFor('K1ABC/P')?.name).toBe('United States');
	});

	it('falls back to the English name when there is no Chinese entry', () => {
		// `3Y` is Peter 1 Island: no curated zh name, so zh === English.
		const p = countryFor('3Y0X');
		expect(p?.zh).toBe(p?.name);
	});

	it('returns null for a non-callsign', () => {
		expect(countryFor('')).toBeNull();
		expect(countryFor('  ')).toBeNull();
		expect(countryFor('!!')).toBeNull();
	});
});
