// Maidenhead locator → cell-centre latitude/longitude.
// Encoding: field (A..R, 20° lon × 10° lat), square (0..9, 2° × 1°), sub (A..X, 5′ × 2.5′).

const A = 'A'.charCodeAt(0);
const ZERO = '0'.charCodeAt(0);

function letterIndex(char: string): number {
	return char.toUpperCase().charCodeAt(0) - A;
}

function digitIndex(char: string): number {
	return char.charCodeAt(0) - ZERO;
}

export interface LatLon {
	/** Degrees north (negative = south). */
	lat: number;
	/** Degrees east (negative = west). */
	lon: number;
}

/** Decode a 4- or 6-character Maidenhead locator to its cell centre; null when it is not one. */
export function maidenheadToLatLon(grid: string): LatLon | null {
	const g = grid.trim().toUpperCase();
	if (!/^[A-R]{2}[0-9]{2}([A-X]{2})?$/.test(g)) return null;
	// Start at the cell's corner, then move to the centre (or the sub-square centre).
	let lon = -180 + letterIndex(g[0]) * 20 + digitIndex(g[2]) * 2;
	let lat = -90 + letterIndex(g[1]) * 10 + digitIndex(g[3]);
	if (g.length >= 6) {
		lon += (letterIndex(g[4]) + 0.5) / 12; // 2° / 24 sub-squares
		lat += (letterIndex(g[5]) + 0.5) / 24; // 1° / 24 sub-squares
	} else {
		lon += 1;   // centre of the 2° square
		lat += 0.5; // centre of the 1° square
	}
	return { lat, lon };
}
