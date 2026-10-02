import { describe, expect, it } from 'vitest';
import { parseFt8Message } from '../sdr/ft8Message';

describe('parseFt8Message', () => {
	it('reads the transmitter and grid from a CQ call', () => {
		expect(parseFt8Message('CQ BG7BVP OL68')).toEqual({ tx: 'BG7BVP', grid: 'OL68' });
		expect(parseFt8Message('CQ JO1WKO PM95')).toEqual({ tx: 'JO1WKO', grid: 'PM95' });
	});

	it('reads the transmitter from a directed message with no grid', () => {
		expect(parseFt8Message('DC5RE BG7BVP -22')).toEqual({ tx: 'DC5RE', grid: null });
		expect(parseFt8Message('BG9MLT BG7BVP RR73')).toEqual({ tx: 'BG9MLT', grid: null });
		expect(parseFt8Message('PB0C BG7BVP 73')).toEqual({ tx: 'PB0C', grid: null });
	});

	it('reads both from a directed message carrying the sender’s grid', () => {
		expect(parseFt8Message('SP5ALV BG7BVP OL68')).toEqual({ tx: 'SP5ALV', grid: 'OL68' });
		expect(parseFt8Message('K1ABC W9XYZ EN37')).toEqual({ tx: 'K1ABC', grid: 'EN37' });
	});

	it('skips a CQ direction word', () => {
		expect(parseFt8Message('CQ DX BG7BVP OL68')).toEqual({ tx: 'BG7BVP', grid: 'OL68' });
	});

	it('keeps portable suffixes and skips non-callsign traffic', () => {
		expect(parseFt8Message('K1ABC/P W9XYZ R-17').tx).toBe('K1ABC/P');
		expect(parseFt8Message('4S6ARW BG7BVP R-17')).toEqual({ tx: '4S6ARW', grid: null });
	});

	it('returns nulls for text without a callsign or grid', () => {
		expect(parseFt8Message('TNX FER QSO')).toEqual({ tx: null, grid: null });
		expect(parseFt8Message('')).toEqual({ tx: null, grid: null });
	});
});
