import { parseFt8Message } from '../sdr/ft8Message';

/** The callsign pair a decode belongs to (unordered; a CQ groups by the caller alone). */
export function ft8SessionKey(text: string): string {
	const { sender, receiver } = parseFt8Message(text);
	if (!sender) return '';
	return receiver ? [sender, receiver].sort().join('↔') : sender;
}

/** Highlight the hovered decode and every decode of the same callsign pair (null clears). */
export function markQsoRows(body: HTMLElement, row: Element | null): void {
	const key = (row as HTMLElement | null)?.dataset?.qso || '';
	for (const peer of Array.from(body.querySelectorAll('tr.ft8-row'))) {
		const el = peer as HTMLElement;
		const pk = el.dataset.qso || '';
		// A CQ row groups by the caller alone, so it marks every decode that call took part in.
		const match = key !== '' && pk !== '' && (pk === key || (!key.includes('↔') && pk.split('↔').includes(key)));
		peer.classList.toggle('qso-focus', match && peer === row);
		peer.classList.toggle('qso-peer', match && peer !== row);
	}
}
