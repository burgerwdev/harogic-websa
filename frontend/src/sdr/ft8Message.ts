// Parse an FT8 line into its callsigns and grid. The message is `[被叫方|CQ] [发送方] [内容]`,
// so the sender is the second callsign (or the one after `CQ`); the grid is always the sender's.

export interface Ft8Message {
	/** The station that transmitted the message. */
	sender: string | null;
	/** The station being called (null for a CQ). */
	receiver: string | null;
	/** The sender's Maidenhead grid, when the message carried one. */
	grid: string | null;
}

/** A grid locator: two letters in A..R, two digits, optionally two sub-square letters in A..X. */
const GRID = /^[A-Ra-r]{2}[0-9]{2}([A-Xa-x]{2})?$/;
/** A callsign: letters/digits with at least one digit; `/` allows portable suffixes (`K1ABC/P`). */
const CALLSIGN = /^[A-Za-z0-9/]+\d[A-Za-z0-9/]*$/;
/** `RR73` is the sign-off, shaped like a grid but never one. */
const NOT_A_GRID = new Set(['RR73']);
/** The bare `73` sign-off matches the callsign pattern but is not a callsign. */
const ACK = new Set(['73']);

export function parseFt8Message(text: string): Ft8Message {
	const tokens = text.trim().split(/\s+/).filter(Boolean);
	const callsigns: string[] = [];
	let grid: string | null = null;
	for (const token of tokens) {
		const upper = token.toUpperCase();
		if (GRID.test(token) && !NOT_A_GRID.has(upper)) {
			grid = upper;
			continue;
		}
		if (CALLSIGN.test(token) && !NOT_A_GRID.has(upper) && !ACK.has(upper)) callsigns.push(upper);
	}
	const cq = tokens.length > 0 && ['CQ', 'DE', 'QRZ'].includes(tokens[0].toUpperCase());
	return cq
		? { sender: callsigns[0] ?? null, receiver: null, grid }
		: { receiver: callsigns[0] ?? null, sender: callsigns[1] ?? null, grid };
}
