import { afterEach, beforeEach, expect, it } from 'vitest';
import { enablePopoverDrag } from '../ui/popoverDrag';

function pointer(target: EventTarget, type: string, x: number, id = 1): void {
  const event = new MouseEvent(type, { clientX: x, clientY: 10, bubbles: true, cancelable: true });
  Object.defineProperty(event, 'pointerId', { value: id });
  target.dispatchEvent(event);
}

beforeEach(() => {
  document.body.innerHTML = '<div id="device-popover"><header class="gnss-popover-head">Device</header></div>';
  enablePopoverDrag('device-popover');
});
afterEach(() => {
  pointer(window, 'pointerup', 0);
  document.body.replaceChildren();
});

it.each(['pointercancel', 'blur'])('releases a detail-popover drag on %s', reason => {
  const head = document.querySelector<HTMLElement>('.gnss-popover-head')!;
  const popover = document.getElementById('device-popover')!;
  pointer(head, 'pointerdown', 20);
  pointer(window, 'pointermove', 40);
  expect(popover.style.left).toBe('20px');
  if (reason === 'pointercancel') pointer(window, 'pointercancel', 40);
  else window.dispatchEvent(new Event('blur'));
  pointer(window, 'pointermove', 90);
  expect(popover.style.left).toBe('20px');
});

it('keeps a detail-popover drag owned by the initiating pointer', () => {
  const head = document.querySelector<HTMLElement>('.gnss-popover-head')!;
  const popover = document.getElementById('device-popover')!;
  pointer(head, 'pointerdown', 20);
  pointer(window, 'pointermove', 40);
  pointer(window, 'pointermove', 90, 2);
  expect(popover.style.left).toBe('20px');
  pointer(window, 'pointerup', 90, 2);
  pointer(window, 'pointermove', 50);
  expect(popover.style.left).toBe('30px');
});
