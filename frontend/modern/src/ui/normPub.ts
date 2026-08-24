// 归一化公共导出(避免 controls 循环依赖)
import * as S from '../core/store';
import { smoothRefWindow, normRefWindow } from '../dsp/normalize';

export function setNormRefWinUser(v: number) { S.setNormRefWinUser(v); }
export { smoothRefWindow, normRefWindow };

export function buildReferenceTablePub(powers: Float32Array): Float32Array {
  const n = powers.length;
  const sorted = Array.from(powers).sort((a, b) => a - b);
  const noiseFloor = sorted[Math.floor(n * 0.3)];
  const thresh = noiseFloor + 20;
  const isSrc = new Uint8Array(n);
  for (let i = 0; i < n; i++) if (powers[i] > thresh) isSrc[i] = 1;
  const ref = new Float32Array(n);
  const K = 8;
  for (let i = 0; i < n; i++) {
    if (isSrc[i]) { ref[i] = powers[i]; continue; }
    const vals: number[] = [];
    let r = 0;
    while (vals.length < K && r < n) {
      r++;
      if (i - r >= 0 && isSrc[i - r]) vals.push(powers[i - r]);
      if (i + r < n && isSrc[i + r]) vals.push(powers[i + r]);
    }
    if (vals.length) {
      vals.sort((a, b) => a - b);
      ref[i] = vals[vals.length >> 1];
    } else {
      ref[i] = powers[i];
    }
  }
  return ref;
}
