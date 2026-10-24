// Shared canvas status area (top-right of the plot).
//
// Every indicator that wants to show state on the canvas pushes a block here during a
// render pass; the blocks are drawn stacked and right-aligned. Putting them in one place is
// what keeps them from overlapping each other (trigger chip, limit verdict, ...).
import { ctx, noticeText, statusError, statusWarnings } from '../core/store';
import { plotRect } from './plot';

export interface StatusBlock {
  lines: string[];
  /** Colour of the first line (the rest are dim green unless they start with '!'). */
  accent?: string;
}

const blocks: StatusBlock[] = [];

export function resetStatusBlocks(): void {
  blocks.length = 0;
}

export function pushStatus(block: StatusBlock | null | undefined): void {
  if (block && block.lines.length) blocks.push(block);
}
// Keep a long device diagnostic inside the plot; its full text remains available on hover.
function errorLines(text: string, width: number): string[] {
  if (ctx.measureText(text).width <= width) return [text];
  const lines: string[] = [];
  let line = '';
  for (const word of text.split(/\s+/)) {
    const next = line ? `${line} ${word}` : word;
    if (line && ctx.measureText(next).width > width) {
      if (lines.length === 2) {
        lines.push(`${line} …`);
        return lines;
      }
      lines.push(line);
      line = word;
    } else line = next;
  }
  if (line) lines.push(line);
  return lines;
}

// Errors persist until replaced; the ordinary notice below still owns its own timeout.
export function renderStatusBlocks(): void {
  const p = plotRect();
  ctx.save();
  ctx.font = 'bold 11px monospace';
  if (statusError) pushStatus({ lines: errorLines(statusError, p.w - 12), accent: '#ff7043' });
  // Vendor warnings (e.g. -12 IF overflow) join the same stack as the trigger chip so every
  // canvas indicator lives in one place. Pushed here, not by a view, so it shows in every
  // mode (an unarmed SWP view never pushes the trigger chip).
  if (statusWarnings.length) pushStatus({ lines: statusWarnings, accent: '#ff7043' });
  // Transient feedback (Auto Scale outcome) below the warnings, in the normal (green) colour: the
  // stack is where the condition it answers is drawn, so cause and result sit together.
  if (noticeText) pushStatus({ lines: [noticeText], accent: '#00cc00' });
  // Canvas text is exposed through the diagnostic attribute for e2e readers.
  // Write the attribute only when its value changes.
  const notice = noticeText ?? '';
  if (ctx.canvas.dataset.notice !== notice) ctx.canvas.dataset.notice = notice;
  const error = statusError ?? '';
  if (ctx.canvas.dataset.error !== error) {
    ctx.canvas.dataset.error = error;
    ctx.canvas.title = error;
  }
  if (!blocks.length) { ctx.restore(); return; }
  ctx.textAlign = 'right';
  ctx.textBaseline = 'middle';
  const rx = p.x + p.w - 6;
  const lineHeight = 14;
  let y = p.y + 12;
  blocks.forEach((block, bi) => {
    block.lines.forEach((line, i) => {
      const warn = line.startsWith('!');
      ctx.fillStyle = warn ? '#ff7043' : (i === 0 ? (block.accent ?? '#00cc00') : '#00cc00');
      ctx.fillText(warn ? line.slice(1) : line, rx, y, p.w - 12);
      y += lineHeight;
    });
    if (bi < blocks.length - 1) y += 4;          // gap between blocks
  });
  ctx.restore();
}
