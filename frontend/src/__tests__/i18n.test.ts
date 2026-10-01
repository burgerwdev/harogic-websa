/**
 * i18n dictionary integrity.
 *
 * The dictionary used to be a base literal plus thirteen additive Object.assign blocks.
 * That made `keyof typeof dict.en` cover only the base literal (194 of 452 keys) and made
 * a missing translation invisible: `t()` silently falls back to English, so a Chinese UI
 * simply showed English text. These tests pin the two properties that prevent a repeat:
 * the two languages declare the same key set, and the key type covers every key.
 */
import { afterEach, describe, expect, it } from 'vitest';
import { dict, hasKey, setLang, t, type I18nKey } from '../core/i18n';

const en = dict.en as Record<string, string>;
const zh = dict.zh as Record<string, string>;

const placeholders = (s: string) => (s.match(/\{(\w+)\}/g) ?? []).sort();

afterEach(() => setLang('en'));

describe('i18n dictionary', () => {
	it('declares exactly the same keys in en and zh', () => {
		const missingZh = Object.keys(en).filter((k) => !(k in zh));
		const missingEn = Object.keys(zh).filter((k) => !(k in en));
		expect(missingZh, `keys without a zh translation: ${missingZh.join(', ')}`).toEqual([]);
		expect(missingEn, `keys without an en translation: ${missingEn.join(', ')}`).toEqual([]);
	});

	it('has no empty values', () => {
		const empty = Object.entries(en).concat(Object.entries(zh))
			.filter(([, v]) => v.trim() === '')
			.map(([k]) => k);
		expect(empty).toEqual([]);
	});

	it('keeps {placeholders} identical across languages', () => {
		const bad = Object.keys(en)
			.filter((k) => placeholders(en[k]).join(',') !== placeholders(zh[k]).join(','))
			.map((k) => `${k}: en=${placeholders(en[k])} zh=${placeholders(zh[k])}`);
		expect(bad).toEqual([]);
	});

	it('types keys that used to live in the Object.assign blocks', () => {
		// Compile-time assertion: if I18nKey were still derived from the base literal only,
		// any of these (added by a later block) would fail to type-check.
		const keys: I18nKey[] = [
			'ref_level',            // base literal
			'tip_version',          // added by an Object.assign block
			'kp_ok',                // keypad additions
			'trg_level',            // trigger panel additions
			'wf_auto',              // waterfall additions
			'sdr_snap_tip',         // was missing from zh until 2026-09
		];
		for (const k of keys) expect(hasKey(k)).toBe(true);
	});

	it('interpolates params and falls back to the key for unknown text', () => {
		setLang('en');
		expect(t('err_below_min', { key: 'rbw', min: 100 })).toContain('100');
		expect(t('err_below_min', {})).toContain('{min}'); // missing param stays visible
		expect(t('__not_a_key__')).toBe('__not_a_key__');
	});

	it('switches language at runtime', () => {
		const key = 'tip_version';
		setLang('zh');
		expect(t(key)).toBe(zh[key]);
		setLang('en');
		expect(t(key)).toBe(en[key]);
	});
});
