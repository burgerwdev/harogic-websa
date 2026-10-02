import { describe, expect, it } from 'vitest';
import { formatLatLon, geoFor } from '../sdr/ft8Geo';

describe('geoFor', () => {
	it('gives country and position from a CQ call with a grid', () => {
		const geo = geoFor('CQ BG7BVP OL68');
		expect(geo.country?.zh).toBe('中国');
		expect(geo.location?.lat).toBeCloseTo(28.5, 9);
		expect(geo.location?.lon).toBeCloseTo(113, 9);
	});

	it('gives country only from a callsign-only line', () => {
		const geo = geoFor('DC5RE BG7BVP -22');
		expect(geo.country?.zh).toBe('德国');
		expect(geo.location).toBeNull();
	});

	it('gives nothing for a line with no callsign or grid', () => {
		const geo = geoFor('TNX FER QSO');
		expect(geo.country).toBeNull();
		expect(geo.location).toBeNull();
	});
});

describe('formatLatLon', () => {
	it('writes cardinal directions with one decimal', () => {
		expect(formatLatLon({ lat: 28.5, lon: 113 })).toBe('28.5°N 113.0°E');
		expect(formatLatLon({ lat: -37.13, lon: 12.3 })).toBe('37.1°S 12.3°E');
		expect(formatLatLon({ lat: 0, lon: -77.58 })).toBe('0.0°N 77.6°W');
	});
});
