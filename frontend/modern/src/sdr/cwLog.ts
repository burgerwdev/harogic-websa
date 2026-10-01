// The CW decode log: what the Morse decoder heard, as readable lines.
//
// The decoder reports characters as they are keyed, so the operator sees the text appear live. A
// line ends when the sender pauses between transmissions (the decoder says so with `endsLine`), and
// each finished line keeps the clock time it started at - the same shape the FT8 table uses, because
// that is how a band's traffic is read back.
//
// Two extra facts ride along with each character, because ggmorse reports neither and both are what
// makes a decode readable rather than a wall of characters:
//
//   * the tone gate's share - how much of the level sat in one narrow band while the character was
//     keyed. That is a *proxy* for confidence (ggmorse emits no per-character confidence at all), and
//     a good one: a keyed sidetone reads 0.6-0.7, noise reads below the gate's threshold, so a
//     character that squeaked through can be shown as the doubtful one it is;
//   * the pause before it - a word gap is the sender thinking, not the page stuck. It comes from the
//     decoded text itself: ggmorse separates words with a space, and no timing rule can do better
//     (measuring the keying got the inside of words wrong, because the decoder's reporting lag adds
//     to every gap it sees - reported: dots between characters that should run together).
//
// The live level (rms and the keyed lamp) is *separate* state with its own listeners: it arrives with
// every audio block (tens per second) and must not re-render the log's lines.
//
// A leaf module on purpose: the worker's ingress fills it, the decode window renders it, and
// neither needs to know about the other.
export interface CwMark {
	/** The tone gate's narrow-band share while this character was keyed (0..1). */
	share: number;
	/** A word-length pause preceded this character. */
	gap: boolean;
}

export interface CwLine {
	/** Wall clock (epoch ms) the line started at. */
	at: number;
	/** The decoded text of the line. */
	text: string;
	/** One entry per character of `text`. */
	marks: CwMark[];
}

/** The decoder's live level, for the window's meter and keyed lamp. */
export interface CwLevel {
	/** Audio level (rms, 0..1). */
	rms: number;
	/** A sidetone is arriving right now (the lamp). */
	keyed: boolean;
}

/** How many lines are kept (a band's worth of traffic; the window scrolls). */
export const MAX_CW_LINES = 200;

/** Below this share a character is drawn as doubtful. It matches the engine's tone gate
 * (`TONE_MIN_SHARE`), which is the line between "a sidetone was there" and "noise got through". */
export const CW_WEAK_SHARE = 0.45;

let lines: CwLine[] = [];
let current = '';
let currentAt = 0;
let currentMarks: CwMark[] = [];
let level: CwLevel = { rms: 0, keyed: false };
const listeners = new Set<() => void>();
const levelListeners = new Set<() => void>();

function notify(): void {
	for (const fn of listeners) fn();
}

/** Append decoded characters. `endsLine` closes the line (the sender paused). */
export function addCwText(text: string, endsLine: boolean, share = 1): void {
	// No text is the normal case for the *end* of a transmission: the decoder closes the line on the
	// pause, long after the last character. Returning early here dropped that signal and ran every
	// transmission into one line (reported).
	if (!text && !endsLine) return;
	if (text && !current) currentAt = Date.now();
	for (let i = 0; i < text.length; i++) {
		const gap = i === 0 ? current.endsWith(' ') : text[i - 1] === ' ';
		currentMarks.push({ share, gap });
	}
	current += text;
	if (endsLine) {
		lines = [...lines, { at: currentAt, text: current, marks: currentMarks }].slice(-MAX_CW_LINES);
		current = '';
		currentMarks = [];
	}
	notify();
}

/** The finished lines and the line being keyed right now (empty when nothing is pending). */
export function cwLog(): { lines: CwLine[]; current: string; marks: CwMark[] } {
	return { lines, current, marks: currentMarks };
}

/** One audio block's level and keyed state: the meter and the lamp, nothing else. */
export function setCwLevel(rms: number, keyed: boolean): void {
	level = { rms, keyed };
	for (const fn of levelListeners) fn();
}

export function cwLevel(): CwLevel {
	return level;
}

export function subscribeCwLevel(fn: () => void): () => void {
	levelListeners.add(fn);
	return () => levelListeners.delete(fn);
}

export function clearCwLog(): void {
	lines = [];
	current = '';
	currentMarks = [];
	notify();
}

export function subscribeCwLog(fn: () => void): () => void {
	listeners.add(fn);
	return () => listeners.delete(fn);
}
