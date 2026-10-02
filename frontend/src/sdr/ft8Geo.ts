// One entry point for the window: `geoFor` turns an FT8 line into a country (from the transmitter
// callsign) and a position (from the grid locator, when present).
import { countryFor, type CtyCountry } from './cty';
import { parseFt8Message } from './ft8Message';
import { maidenheadToLatLon, type LatLon } from './maidenhead';

export interface Ft8Geo {
	/** The transmitter's country/region, or null when the line carried no callsign. */
	country: CtyCountry | null;
	/** The grid locator's cell centre, or null when the line carried no locator. */
	location: LatLon | null;
}

export function geoFor(text: string): Ft8Geo {
	const { tx, grid } = parseFt8Message(text);
	return {
		country: tx ? countryFor(tx) : null,
		location: grid ? maidenheadToLatLon(grid) : null,
	};
}

/** A position as a short `28.5°N 113.0°E` string (degrees, one decimal, cardinal). */
export function formatLatLon(p: LatLon): string {
	const ns = p.lat >= 0 ? 'N' : 'S';
	const ew = p.lon >= 0 ? 'E' : 'W';
	return `${Math.abs(p.lat).toFixed(1)}°${ns} ${Math.abs(p.lon).toFixed(1)}°${ew}`;
}
