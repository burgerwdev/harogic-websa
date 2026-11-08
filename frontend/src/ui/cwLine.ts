import { t } from '../core/i18n';
import { clock } from '../sdr/ft8Log';
import { CW_WEAK_SHARE, type CwLine, type CwMark } from '../sdr/cwLog';

/** One CW line: the clock time it started at, then the text (the current line is marked).
 *
 * The text is built as runs rather than per character: a clean decode is one text node (the common
 * case by far), and only a doubtful character breaks the run into a span - which keeps a window full
 * of lines cheap to re-render as characters keep arriving. A word-length pause is drawn as a faint
 * dot, so "the sender is thinking" cannot be mistaken for "the page is stuck".
 */
export function cwLineFor(
	line: CwLine | { at: number; text: string; marks: CwMark[] }, current: boolean, doc: Document, utcTime: boolean,
): HTMLElement {
	const row = doc.createElement('div');
	row.className = current ? 'cw-line current' : 'cw-line';
	const at = doc.createElement('span');
	at.className = 'cw-time';
	at.textContent = clock(line.at, utcTime);
	const text = doc.createElement('span');
	text.className = 'cw-text';
	let run = '';
	let weak = false;
	let runShare = 1;
	const flush = (): void => {
		if (!run) return;
		if (weak) {
			const span = doc.createElement('span');
			span.className = 'cw-ch weak';
			span.textContent = run;
			// Per *run*, not per character: a doubtful character is rare and usually lasts a few
			// characters, and one span per character would rebuild thousands of nodes per line.
			span.title = t('cw_confidence_tip', { pct: Math.round(runShare * 100) });
			text.appendChild(span);
		} else {
			text.appendChild(doc.createTextNode(run));
		}
		run = '';
		runShare = 1;
	};
	for (let i = 0; i < line.text.length; i++) {
		const mark = line.marks[i];
		// Never at the start of a line: the timestamp already says a new transmission began, and a dot
		// under it reads as stray punctuation.
		if (mark && mark.gap && !weak && i > 0) {
			flush();
			// Empty on purpose: the dot is drawn by CSS (`.cw-gap::before`), so the pause is visible
			// without ever entering the text - copying a line, or the log's own textContent, must not
			// come back with punctuation the sender never keyed.
			const gap = doc.createElement('span');
			gap.className = 'cw-gap';
			gap.title = t('cw_pause_tip');
			text.appendChild(gap);
		}
		const doubtful = !!mark && mark.share < CW_WEAK_SHARE;
		if (doubtful !== weak) {
			flush();
			weak = doubtful;
		}
		if (mark && mark.share < runShare) runShare = mark.share;
		run += line.text[i];
	}
	flush();
	if (current) {
		const cursor = doc.createElement('span');
		cursor.className = 'cw-cursor';
		cursor.textContent = '▌';
		text.appendChild(cursor);
	}
	row.appendChild(at);
	row.appendChild(text);
	return row;
}
