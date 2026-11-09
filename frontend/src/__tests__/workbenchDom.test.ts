import { afterEach, expect, it } from 'vitest';
import { createWorkbenchPatcher, patchWorkbench } from '../ui/workbenchDom';

afterEach(() => document.body.replaceChildren());

it('skips unchanged chrome while retaining separately owned live content', () => {
  const host = document.createElement('div'); document.body.append(host);
  const patch = createWorkbenchPatcher(host), html = '<button data-key="start">Start</button><div data-live></div>';
  expect(patch(html)).toBe(true);
  const live = host.querySelector('[data-live]')!; live.textContent = 'Live result';
  expect(patch(html, el => el.hasAttribute('data-live'))).toBe(false);
  expect(live.textContent).toBe('Live result');
  host.replaceChildren(); patch.reset();
  expect(patch(html)).toBe(true);
  expect(host.querySelector('button')!.textContent).toBe('Start');
});

it('reconciles a preserved edit after blur even when the desired HTML is unchanged', () => {
  const host = document.createElement('div'); document.body.append(host);
  const patch = createWorkbenchPatcher(host);
  patch('<input value="old">');
  const field = host.querySelector('input')!; field.focus(); field.value = 'draft';
  const html = '<input value="confirmed">';
  patch(html, el => el === field);
  expect(field.value).toBe('draft');
  field.blur();
  expect(patch(html)).toBe(true);
  expect(field.value).toBe('confirmed');
});

it('reorders keyed children without replacing their identity or live listeners', () => {
  const host = document.createElement('div');
  patchWorkbench(host, '<button data-key="a">A</button><button data-key="b">B</button>');
  const [a, b] = [...host.children];
  patchWorkbench(host, '<button data-key="b">New B</button><button data-key="a">A</button>');
  expect([...host.children]).toEqual([b, a]);
  expect(b.textContent).toBe('New B');
});
