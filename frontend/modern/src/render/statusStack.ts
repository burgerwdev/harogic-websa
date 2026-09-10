// Shared canvas status area (top-right of the plot).
//
// Every indicator that wants to show state on the canvas pushes a block here during a
// render pass; the blocks are drawn stacked and right-aligned. Putting them in one place is
// what keeps them from overlapping each other (trigger chip, limit verdict, ...).
import { ctx } from '../core/store';
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

export function renderStatusBlocks(): void {
  if (!blocks.length) return;
  const p = plotRect();
  ctx.save();
  ctx.font = 'bold 11px monospace';
  ctx.textAlign = 'right';
  ctx.textBaseline = 'middle';
  const rx = p.x + p.w - 6;
  const lineHeight = 14;
  let y = p.y + 12;
  blocks.forEach((block, bi) => {
    block.lines.forEach((line, i) => {
      const warn = line.startsWith('!');
      ctx.fillStyle = warn ? '#ff7043' : (i === 0 ? (block.accent ?? '#00cc00') : '#00cc00');
      ctx.fillText(warn ? line.slice(1) : line, rx, y);
      y += lineHeight;
    });
    if (bi < blocks.length - 1) y += 4;          // gap between blocks
  });
  ctx.restore();
}
