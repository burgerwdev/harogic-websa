import { describe, expect, it } from 'vitest';
import { maidenheadToLatLon } from '../sdr/maidenhead';

describe('maidenheadToLatLon', () => {
	it('decodes a 4-character locator to the cell centre', () => {
		// OL68 is the Changsha/Hunan cell (the sample data's `BG7BVP` QTH): 112°..114°E, 28°..29°N.
		const p = maidenheadToLatLon('OL68')!;
		expect(p.lon).toBeCloseTo(113, 9);
		expect(p.lat).toBeCloseTo(28.5, 9);
	});

	it('lands on the corners of the world grid', () => {
		expect(maidenheadToLatLon('AA00')!.lon).toBeCloseTo(-179, 9);
		expect(maidenheadToLatLon('AA00')!.lat).toBeCloseTo(-89.5, 9);
		expect(maidenheadToLatLon('RR99')!.lon).toBeCloseTo(179, 9);
		expect(maidenheadToLatLon('RR99')!.lat).toBeCloseTo(89.5, 9);
	});

	it('narrows a 6-character locator into its sub-square', () => {
		// JN18 is the 2°×1° cell at 2..4°E / 48..49°N; the `eu` sub-square sits inside it.
		const p = maidenheadToLatLon('JN18eu')!;
		expect(p.lon).toBeCloseTo(2 + 4.5 / 12, 9);      // 2.375°E
		expect(p.lat).toBeCloseTo(48 + 20.5 / 24, 9);    // 48.854…°N
	});

	it('accepts lowercase sub-squares and stray case', () => {
		expect(maidenheadToLatLon('ol68')!.lat).toBeCloseTo(28.5, 9);
		expect(maidenheadToLatLon('JN18eu')!.lat).toBeCloseTo(maidenheadToLatLon('JN18EU')!.lat, 9);
	});

	it('returns null for anything that is not a locator', () => {
		expect(maidenheadToLatLon('BG7BVP')).toBeNull(); // a callsign, not a grid
		expect(maidenheadToLatLon('OL6')).toBeNull();
		expect(maidenheadToLatLon('OL680')).toBeNull();
		expect(maidenheadToLatLon('OL6Z')).toBeNull();   // last pair must be A..X
		expect(maidenheadToLatLon('')).toBeNull();
	});
});
