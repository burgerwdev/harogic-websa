// Pure helpers for the control rail (kept DOM free so they can be unit tested).

/**
 * Index of the group currently at the top of the scroll view: the last entry whose
 * top has passed the viewport top (minus a small tolerance so a group becomes
 * "current" right before its header reaches the edge).
 */
export function activeIndex(tops: readonly number[], scrollTop: number, tolerance = 8): number {
  if (!tops.length) return -1;
  let idx = 0;
  for (let i = 0; i < tops.length; i++) {
    if (tops[i] - tolerance <= scrollTop) idx = i;
    else break;
  }
  return idx;
}
