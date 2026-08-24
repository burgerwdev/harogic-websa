// Marker 公共: 索引/频率/重定位 (与 render 解耦, 避免循环依赖)
import * as S from './store';

export function markerFreqHz(idx: number): number {
  if (S.freqArray && S.freqArray.length > 1) return S.freqArray[Math.min(idx, S.freqArray.length - 1)];
  return S.centerHz;
}

// 频率轴更新后,把各启用 marker 按其记录的频率值映射到新轴上的最近点
export function retrackMarkers() {
  if (!S.freqArray) return;
  S.markers.forEach(m => {
    if (m.enabled && m.freq != null) {
      let best = 0, bd = Infinity;
      for (let i = 0; i < S.freqArray!.length; i++) {
        const d = Math.abs(S.freqArray![i] - m.freq);
        if (d < bd) { bd = d; best = i; }
      }
      m.idx = best;
    }
  });
}
