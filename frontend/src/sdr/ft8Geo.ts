// One entry point for the window: `geoFor` turns an FT8 line into the sender/receiver countries
// (from callsign prefixes) and the sender's position (from the grid locator, when present).
import { countryFor, type CtyCountry } from './cty';
import { parseFt8Message } from './ft8Message';
import { maidenheadToLatLon, type LatLon } from './maidenhead';

export interface Ft8Geo {
	/** The transmitting station's country/region, or null. */
	sender: CtyCountry | null;
	/** The called station's country/region, or null for a CQ. */
	receiver: CtyCountry | null;
	/** The sender's grid cell centre, or null when the message carried no locator. */
	location: LatLon | null;
}

export function geoFor(text: string): Ft8Geo {
	const { sender, receiver, grid } = parseFt8Message(text);
	return {
		sender: sender ? countryFor(sender) : null,
		receiver: receiver ? countryFor(receiver) : null,
		location: grid ? maidenheadToLatLon(grid) : null,
	};
}

/** A position as a short `28.5°N 113.0°E` string (degrees, one decimal, cardinal). */
export function formatLatLon(p: LatLon): string {
	const ns = p.lat >= 0 ? 'N' : 'S';
	const ew = p.lon >= 0 ? 'E' : 'W';
	return `${Math.abs(p.lat).toFixed(1)}°${ns} ${Math.abs(p.lon).toFixed(1)}°${ew}`;
}
