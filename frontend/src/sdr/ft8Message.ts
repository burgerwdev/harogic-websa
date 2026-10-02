// Pull the transmitter callsign (first callsign-shaped token) and grid locator (`OL68`-shaped)
// out of a decoded FT8 line. `RR73` looks like a locator but is the sign-off, so it is excluded.

export interface Ft8Message {
	/** The transmitting station's callsign (the message's first callsign), or null. */
	tx: string | null;
	/** The Maidenhead locator the message carried (e.g. `OL68`), or null. */
	grid: string | null;
}

/** A grid locator: two letters in A..R, two digits, optionally two sub-square letters in A..X. */
const GRID = /^[A-Ra-r]{2}[0-9]{2}([A-Xa-x]{2})?$/;
/** A callsign: letters/digits with at least one digit; `/` allows portable suffixes (`K1ABC/P`). */
const CALLSIGN = /^[A-Za-z0-9/]+\d[A-Za-z0-9/]*$/;
/** `RR73` is the sign-off, not a grid (its cell is uninhabited Arctic Ocean). */
const NOT_A_GRID = new Set(['RR73']);

export function parseFt8Message(text: string): Ft8Message {
	let tx: string | null = null;
	let grid: string | null = null;
	for (const token of text.trim().split(/\s+/)) {
		const upper = token.toUpperCase();
		if (GRID.test(token) && !NOT_A_GRID.has(upper)) {
			grid = upper;
			continue;
		}
		if (tx === null && CALLSIGN.test(token)) tx = upper;
	}
	return { tx, grid };
}
