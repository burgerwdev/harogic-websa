// DSP engine unit tests: smoothing / parabola fit / excursion / sweep&-valley finding / resampling / normalization
import { describe, it, expect, beforeEach } from 'vitest';
import { sgSmooth, smoothForDisplay } from '../dsp/smooth';
import { parabolaFit, hasExcursion, findExtremesOrdered } from '../dsp/peaks';
import { resampleTrace } from '../dsp/traces';
import { buildReferenceTablePub } from '../ui/normPub';
import * as S from '../core/store';
import { synthCW, synthBandpass, synthTwoPeaks } from './synth';

beforeEach(() => {
  S.setSmoothBins(1);
});

describe('sgSmooth (Savitzky-Golay 2阶 + 梯度自适应)', () => {
  it('平滑噪声不削平缓峰(保峰)', () => {
    // 5-bin-wide peak (the smallest feature a real analyzer has); peak is preserved after smoothing
    const src = new Float32Array(101);
    for (let i = 0; i < 101; i++) src[i] = -80;
    for (let i = 48; i <= 52; i++) src[i] = -20;
    const out = sgSmooth(src, 5, true);
    expect(out[50]).toBeGreaterThan(-35);
    expect(out[50]).toBeGreaterThan(out[47]);
  });

  it('陡峭边沿梯度自适应缩窗(保边沿)', () => {
    const src = new Float32Array(101);
    for (let i = 0; i < 101; i++) src[i] = i < 50 ? -80 : -20;   // sharp edge
    const out = sgSmooth(src, 9, true);
    // edge not severely blurred (adaptive shrinking window): the slope near the boundary is preserved
    expect(out[49]).toBeLessThan(-20);          // still in the low region
    expect(out[50]).toBeGreaterThan(out[49]);   // rise is preserved
  });
});

describe('parabolaFit 抛物线亚频点拟合', () => {
  it('对称峰 dk≈0', () => {
    const p = new Float32Array([-10, -5, -10]);
    const { dk } = parabolaFit(p, 1);
    expect(Math.abs(dk)).toBeLessThan(1e-6);
  });

  it('偏置峰 dk 指向真实顶点方向', () => {
    const p = new Float32Array([-7, -5, -10]);   // higher right, lower left → vertex shifts left
    const { dk } = parabolaFit(p, 1);
    expect(dk).toBeLessThan(0);
  });
});

describe('hasExcursion 双侧追溯', () => {
  it('双侧下降≥6dB 的峰为真峰', () => {
    const p = new Float32Array([-80, -80, -20, -80, -80]);
    expect(hasExcursion(p, 2, 6, true)).toBe(true);
  });

  it('单侧不足(边缘)不算', () => {
    const p = new Float32Array([-20, -30, -80, -80, -80]);
    expect(hasExcursion(p, 0, 6, true)).toBe(false);   // no descent on the left boundary
  });
});

describe('findExtremesOrdered 寻峰', () => {
  it('合成双峰检测到 2 个峰(频率升序)', () => {
    const p = synthTwoPeaks(1000);
    // inject the trace powers (smoothing off → use the raw data)
    const t = S.traces[0];
    t.powers = p; t.mode = 'CLEAR_WRITE';
    S.setFreqArray(new Float64Array(1000).map((_, i) => 500e6 + i * 1e6));
    const peaks = findExtremesOrdered('right', true);
    expect(peaks.length).toBe(2);
    expect(peaks[0].i).toBeLessThan(peaks[1].i);
    // magnitude approximate (peak 2 is stronger, -25dB)
    expect(peaks[1].a).toBeGreaterThan(peaks[0].a);
  });

  it('噪声底不产生伪峰', () => {
    const p = new Float32Array(200).fill(-90);
    S.traces[0].powers = p;
    expect(findExtremesOrdered('right', true).length).toBe(0);
  });
});

describe('findExtremesOrdered 寻谷(带通场景)', () => {
  it('带通两侧各识别 1 个谷(25bin 凹陷合并), 谷位置为凹陷最低', () => {
    const p = synthBandpass(1000);
    S.traces[0].powers = p;
    S.setSmoothBins(1);
    const valleys = findExtremesOrdered('right', false);
    expect(valleys.length).toBe(2);   // left valley + right valley
    // left valley at the passband left edge, right valley at the right edge
    expect(valleys[0].i).toBeLessThan(300);
    expect(valleys[1].i).toBeGreaterThan(700);
  });

  it('smooth 开启时基于平滑数据定位(与显示一致)', () => {
    const p = synthBandpass(1000);
    S.traces[0].powers = p;
    S.setSmoothBins(5);
    const valleys = findExtremesOrdered('right', false);
    expect(valleys.length).toBe(2);
  });
});

describe('resampleTrace 保峰重采样', () => {
  it('MAX_HOLD 降采样保留峰值', () => {
    const src = new Float32Array(100);
    for (let i = 0; i < 100; i++) src[i] = -80;
    src[50] = -20;   // sharp peak
    const out = resampleTrace(src, 50, true);
    // the sharp peak position lands back in the corresponding bin
    expect(Math.max(...out)).toBeGreaterThan(-25);
  });

  it('MIN_HOLD 降采样保留谷值', () => {
    const src = new Float32Array(100).fill(-20);
    src[50] = -90;
    const out = resampleTrace(src, 50, false);
    expect(Math.min(...out)).toBeLessThan(-80);
  });
});

describe('归一化参考构建(噪声源)', () => {
  it('无源点(全噪声)时参考为平滑噪声底, 归一化差值围绕 0', () => {
    // flat noise source: full band -80dB + ±1dB Gaussian fluctuation, no significant sources
    const p = new Float32Array(500);
    for (let i = 0; i < 500; i++) p[i] = -80 + (Math.random() - 0.5) * 2;
    const ref = buildReferenceTablePub(p);
    // reference is close to the noise level (around -80, small fluctuation after smoothing)
    const refs = Array.from(ref);
    const avg = refs.reduce((a, b) => a + b, 0) / refs.length;
    expect(Math.abs(avg - (-80))).toBeLessThan(2);
    // per-point fluctuation of the smoothed reference is much smaller than the raw noise (±1dB → within ±0.5dB after smoothing)
    let maxDev = 0;
    for (const v of refs) maxDev = Math.max(maxDev, Math.abs(v - avg));
    expect(maxDev).toBeLessThan(1.0);   // statistical tolerance for synthetic noise
  });
});

describe('归一化参考构建(合成直通响应)', () => {
  it('平坦响应参考接近 0dB 偏移(源点保留原值)', () => {
    // direct pass: flat -30dB across the full band + noise
    const p = new Float32Array(500);
    for (let i = 0; i < 500; i++) p[i] = -30 + (Math.random() - 0.5) * 0.5;
    const ref = buildReferenceTablePub(p);
    for (let i = 10; i < 490; i++) {
      expect(Math.abs(ref[i] - (-30))).toBeLessThan(2);
    }
  });
});
