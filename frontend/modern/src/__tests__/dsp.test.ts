// DSP 引擎单测: 平滑 / 抛物线拟合 / Excursion / 寻峰寻谷 / 重采样 / 归一化
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
    // 5bin 宽的峰(频谱仪实际最小特征), 平滑后峰值保留
    const src = new Float32Array(101);
    for (let i = 0; i < 101; i++) src[i] = -80;
    for (let i = 48; i <= 52; i++) src[i] = -20;
    const out = sgSmooth(src, 5, true);
    expect(out[50]).toBeGreaterThan(-35);
    expect(out[50]).toBeGreaterThan(out[47]);
  });

  it('陡峭边沿梯度自适应缩窗(保边沿)', () => {
    const src = new Float32Array(101);
    for (let i = 0; i < 101; i++) src[i] = i < 50 ? -80 : -20;   // 阶跃边沿
    const out = sgSmooth(src, 9, true);
    // 边沿位置不严重模糊(自适应缩窗): 边界附近斜率保持
    expect(out[49]).toBeLessThan(-20);          // 仍在低区
    expect(out[50]).toBeGreaterThan(out[49]);   // 上升保留
  });
});

describe('parabolaFit 抛物线亚频点拟合', () => {
  it('对称峰 dk≈0', () => {
    const p = new Float32Array([-10, -5, -10]);
    const { dk } = parabolaFit(p, 1);
    expect(Math.abs(dk)).toBeLessThan(1e-6);
  });

  it('偏置峰 dk 指向真实顶点方向', () => {
    const p = new Float32Array([-7, -5, -10]);   // 右高左低 → 顶点偏左
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
    expect(hasExcursion(p, 0, 6, true)).toBe(false);   // 左边界无下降
  });
});

describe('findExtremesOrdered 寻峰', () => {
  it('合成双峰检测到 2 个峰(频率升序)', () => {
    const p = synthTwoPeaks(1000);
    // 注入 trace powers(平滑关闭 → 用原始数据)
    const t = S.traces[0];
    t.powers = p; t.mode = 'CLEAR_WRITE';
    S.setFreqArray(new Float64Array(1000).map((_, i) => 500e6 + i * 1e6));
    const peaks = findExtremesOrdered('right', true);
    expect(peaks.length).toBe(2);
    expect(peaks[0].i).toBeLessThan(peaks[1].i);
    // 幅度近似(峰2更强 -25dB)
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
    expect(valleys.length).toBe(2);   // 左谷 + 右谷
    // 左谷在带通左缘, 右谷在右缘
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
    src[50] = -20;   // 尖峰
    const out = resampleTrace(src, 50, true);
    // 尖峰位置落回对应的 bin
    expect(Math.max(...out)).toBeGreaterThan(-25);
  });

  it('MIN_HOLD 降采样保留谷值', () => {
    const src = new Float32Array(100).fill(-20);
    src[50] = -90;
    const out = resampleTrace(src, 50, false);
    expect(Math.min(...out)).toBeLessThan(-80);
  });
});

describe('归一化参考构建(合成直通响应)', () => {
  it('平坦响应参考接近 0dB 偏移(源点保留原值)', () => {
    // 直通: 全频段 -30dB 平坦 + 噪声
    const p = new Float32Array(500);
    for (let i = 0; i < 500; i++) p[i] = -30 + (Math.random() - 0.5) * 0.5;
    const ref = buildReferenceTablePub(p);
    for (let i = 10; i < 490; i++) {
      expect(Math.abs(ref[i] - (-30))).toBeLessThan(2);
    }
  });
});
