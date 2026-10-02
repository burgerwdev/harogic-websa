import { describe, expect, it } from 'vitest';
import { parseFt8Message } from '../sdr/ft8Message';

describe('parseFt8Message', () => {
	it('reads the sender and grid from a CQ call', () => {
		expect(parseFt8Message('CQ BG7BVP OL68')).toEqual({ sender: 'BG7BVP', receiver: null, grid: 'OL68' });
		expect(parseFt8Message('CQ JO1WKO PM95')).toEqual({ sender: 'JO1WKO', receiver: null, grid: 'PM95' });
	});

	it('reads sender and receiver from a directed message with no grid', () => {
		expect(parseFt8Message('DC5RE BG7BVP -22')).toEqual({ sender: 'BG7BVP', receiver: 'DC5RE', grid: null });
		expect(parseFt8Message('BG9MLT BG7BVP RR73')).toEqual({ sender: 'BG7BVP', receiver: 'BG9MLT', grid: null });
		expect(parseFt8Message('PB0C BG7BVP 73')).toEqual({ sender: 'BG7BVP', receiver: 'PB0C', grid: null });
	});

	it('reads both callsigns and the sender’s grid', () => {
		expect(parseFt8Message('SP5ALV BG7BVP OL68')).toEqual({ sender: 'BG7BVP', receiver: 'SP5ALV', grid: 'OL68' });
		expect(parseFt8Message('K1ABC W9XYZ EN37')).toEqual({ sender: 'W9XYZ', receiver: 'K1ABC', grid: 'EN37' });
	});

	it('skips a CQ direction word', () => {
		expect(parseFt8Message('CQ DX BG7BVP OL68')).toEqual({ sender: 'BG7BVP', receiver: null, grid: 'OL68' });
	});

	it('keeps portable suffixes and skips non-callsign traffic', () => {
		expect(parseFt8Message('K1ABC/P W9XYZ R-17').sender).toBe('W9XYZ');
		expect(parseFt8Message('4S6ARW BG7BVP R-17')).toEqual({ sender: 'BG7BVP', receiver: '4S6ARW', grid: null });
	});

	it('returns nulls for text without a callsign or grid', () => {
		expect(parseFt8Message('TNX FER QSO')).toEqual({ sender: null, receiver: null, grid: null });
		expect(parseFt8Message('')).toEqual({ sender: null, receiver: null, grid: null });
	});
});
