/**
 * A command ack may only clear the edits its commit knew about.
 *
 * Reported: submit A, then type B while A is in flight. A's ack used to wipe the editor's
 * dirty/edited marks, and the following STATUS refresh then overwrote the boxes with the
 * confirmed values — B vanished without ever having been wrong. The commit now carries the
 * edit generation it was made at; the ack keeps anything typed after it.
 */
import { afterEach, describe, expect, it } from 'vitest';

import {
	beginFrequencyCommit,
	markFrequencyDirty,
	syncFrequencyEditorStatus,
} from '../ui/panels/frequency';

afterEach(() => {
	document.getElementById('swp-freq-settings')?.remove();
});

function editor(): HTMLElement {
	const existing = document.getElementById('swp-freq-settings');
	if (existing) return existing;
	const el = document.createElement('div');
	el.id = 'swp-freq-settings';
	const input = document.createElement('input');
	input.id = 'input-center';
	el.appendChild(input);
	document.body.appendChild(el);
	return el;
}

describe('frequency editor acks', () => {
	it('keeps edits typed after the commit when the ack arrives', () => {
		const el = editor();
		const input = el.querySelector('input') as HTMLElement;

		markFrequencyDirty('swp-freq-settings');
		beginFrequencyCommit('swp-freq-settings');
		// B is typed while A is in flight.
		markFrequencyDirty('swp-freq-settings');
		input.dataset.edited = '1';

		syncFrequencyEditorStatus('SET_FREQ');

		expect(el.dataset.dirty).toBe('1');
		expect(input.dataset.edited).toBe('1');
		expect(el.dataset.pending).toBeUndefined();      // A itself is done
	});

	it('clears everything when nothing was typed after the commit', () => {
		const el = editor();
		const input = el.querySelector('input') as HTMLElement;

		markFrequencyDirty('swp-freq-settings');
		beginFrequencyCommit('swp-freq-settings');

		syncFrequencyEditorStatus('SET_FREQ');

		expect(el.dataset.dirty).toBeUndefined();
		expect(input.dataset.edited).toBeUndefined();
		expect(el.dataset.pending).toBeUndefined();
	});
});
