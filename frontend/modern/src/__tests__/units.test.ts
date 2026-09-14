/**
 * Unit group <-> input id resolution.
 *
 * Unit groups are keyed with an underscore (`rta_center`, matching `units[]`) while the input
 * element ids use a hyphen (`input-rta-center`). The mismatch made the RTA centre unit buttons
 * silently do nothing (no `commit`, no value conversion) and left the virtual keypad without a
 * unit row for that field.
 */
import { beforeEach, describe, expect, it } from 'vitest';
import { fieldForInput, inputForField, setUnit, UNIT_OPTIONS } from '../core/units';
import { units } from '../core/store';

const mount = (id: string, value = '433') => {
	const el = document.createElement('input');
	el.id = id;
	el.value = value;
	document.body.appendChild(el);
	const group = document.createElement('div');
	group.id = `unit-${id.replace(/^input-/, '').replace(/-/g, '_')}-group`;
	document.body.appendChild(group);
	return el;
};

beforeEach(() => document.body.replaceChildren());

describe('unit field resolution', () => {
	it('finds the input for a key with an underscore', () => {
		mount('input-rta-center');
		expect(inputForField('rta_center')?.id).toBe('input-rta-center');
	});

	it('finds the input for a key that already uses a hyphen', () => {
		mount('input-center');
		expect(inputForField('center')?.id).toBe('input-center');
	});

	it('returns null for an unknown field', () => {
		expect(inputForField('nope')).toBeNull();
	});

	it('maps an input id back to the canonical key', () => {
		expect(fieldForInput({ id: 'input-rta-center' })).toBe('rta_center');
		expect(fieldForInput({ id: 'input-center' })).toBe('center');
		expect(fieldForInput({ id: 'input-not-a-unit' })).toBe('');
		expect(fieldForInput({ id: 'spectrum' })).toBe('');
		expect(UNIT_OPTIONS.rta_center).toBeDefined();
	});

	it('converts and commits when the unit button is used on an untouched field', () => {
		const input = mount('input-rta-center', '1000');
		units.rta_center = 'MHz';
		const events: Array<{ field: string; unit: string; commit: boolean }> = [];
		document.addEventListener('websa:unit-commit', (e) => events.push((e as CustomEvent).detail));
		setUnit('rta_center', 'GHz');
		expect(input.value).toBe('1.000000');            // 1000 MHz -> 1 GHz
		expect(units.rta_center).toBe('GHz');
		expect(events).toHaveLength(1);
		expect(events[0]).toEqual({ field: 'rta_center', unit: 'GHz', commit: false });
	});

	it('does not convert but commits when the user typed a value', () => {
		const input = mount('input-rta-center', '433');
		units.rta_center = 'MHz';
		input.dataset.edited = '1';
		const events: Array<{ commit: boolean }> = [];
		document.addEventListener('websa:unit-commit', (e) => events.push((e as CustomEvent).detail));
		setUnit('rta_center', 'GHz');
		expect(input.value).toBe('433');                 // the typed number keeps its meaning
		expect(events[0].commit).toBe(true);             // ... and is applied as intended
	});
});
