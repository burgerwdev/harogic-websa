// DSP engine unit tests: smoothing / parabola fit / excursion / sweep&-valley finding / resampling / normalization
import { describe, it, expect, beforeEach } from 'vitest';
import { sgSmooth, smoothForDisplay } from '../dsp/smooth';
import { parabolaFit, hasExcursion, findExtremesOrdered } from '../dsp/peaks';
import { resampleTrace } from '../dsp/traces';
import { buildReferenceTablePub } from '../ui/normPub';
import { percentileApprox } from '../dsp/stats';
import {
  niceSpanStep,
  normalizeCenterSpan,
  normalizeStartStop,
  steppedSpan,
} from '../core/frequency';
import { setUnit } from '../core/units';
import { updateTrackingMarkers } from '../dsp/markerTracking';
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

describe('频率字段联动', () => {
  it('center/span 在设备边界内生成一致的 start/stop', () => {
    const window = normalizeCenterSpan(1e9, 100e6, 9e3, 9e9)!;
    expect(window.start).toBe(950e6);
    expect(window.stop).toBe(1050e6);
  });

  it('全扫宽会移动 center 并保持完整范围', () => {
    const window = normalizeCenterSpan(1e9, 20e9, 9e3, 9e9)!;
    expect(window.start).toBe(9e3);
    expect(window.stop).toBe(9e9);
  });

  it('start/stop 作为一组生成 center/span', () => {
    const window = normalizeStartStop(950e6, 1050e6, 9e3, 9e9)!;
    expect(window.center).toBe(1e9);
    expect(window.span).toBe(100e6);
  });

  it('默认 span step 使用联动的 1/2/5 档并限制边界', () => {
    expect(niceSpanStep(100e6)).toBe(10e6);
    expect(niceSpanStep(200e6)).toBe(20e6);
    expect(niceSpanStep(50.78125e6)).toBe(5e6);
    expect(steppedSpan(100e6, 10e6, -1, 100, 9e9)).toBe(90e6);
    expect(steppedSpan(100, 10e6, -1, 100, 9e9)).toBe(100);
    expect(steppedSpan(8.999e9, 10e6, 1, 100, 9e9)).toBe(9e9);
  });

  it('单位按钮未编辑时换算显示，编辑后按所选单位提交', () => {
    const previous = document.body.innerHTML;
    document.body.innerHTML = '<input id="input-center" value="2">' +
      '<div id="unit-center-group"><button>MHz</button><button>GHz</button></div>';
    S.units.center = 'MHz';
    const commits: boolean[] = [];
    const listener = (event: Event) => {
      commits.push((event as CustomEvent<{ commit: boolean }>).detail.commit);
    };
    document.addEventListener('websa:unit-commit', listener);

    setUnit('center', 'GHz');
    const input = document.getElementById('input-center') as HTMLInputElement;
    expect(input.value).toBe('0.002000');
    expect(commits).toEqual([false]);

    input.value = '2';
    input.dataset.edited = '1';
    setUnit('center', 'GHz');
    expect(input.value).toBe('2');
    expect(commits).toEqual([false, true]);
    expect(S.units.center).toBe('GHz');

    document.removeEventListener('websa:unit-commit', listener);
    S.units.center = 'MHz';
    document.body.innerHTML = previous;
  });
});

describe('多 Marker Tracking', () => {
  it('SWP 与 RTA 都按峰值强度分配不同信号峰', () => {
    const previous = document.body.innerHTML;
    document.body.innerHTML = '<input id="input-peakthr" value="-80">';
    const powers = synthTwoPeaks(1000);
    S.setFreqArray(new Float64Array(1000).map((_, i) => 500e6 + i * 1e6));
    S.traces[0].powers = powers;
    S.traces[0].mode = 'CLEAR_WRITE';
    S.setActiveTraceIdx(0);
    S.markers.forEach(marker => Object.assign(marker, {
      enabled: marker.id <= 2,
      mode: marker.id <= 2 ? 'NORMAL' : 'OFF',
      tracking: marker.id <= 2,
      idx: 0,
      freq: null,
    }));

    S.setRtaMode(false);
    expect(updateTrackingMarkers()).toBe(true);
    const swpIndexes = [S.markers[0].idx, S.markers[1].idx];
    expect(new Set(swpIndexes).size).toBe(2);
    expect(powers[swpIndexes[0]]).toBeGreaterThanOrEqual(powers[swpIndexes[1]]);

    S.markers[0].idx = 0;
    S.markers[1].idx = 0;
    S.setRtaData({ spec: powers });
    S.setRtaDisplays([powers, null, null, null]);
    S.setRtaMode(true);
    expect(updateTrackingMarkers()).toBe(true);
    const rtaIndexes = [S.markers[0].idx, S.markers[1].idx];
    expect(rtaIndexes).toEqual(swpIndexes);

    S.setRtaMode(false);
    S.setRtaData(null);
    S.setRtaDisplays([null, null, null, null]);
    S.markers.forEach(marker => { marker.enabled = false; marker.mode = 'OFF'; marker.tracking = false; });
    document.body.innerHTML = previous;
  });

  it('后续帧优先跟随邻近峰而不是跳到远端更强杂散', () => {
    const previous = document.body.innerHTML;
    document.body.innerHTML = '<input id="input-peakthr" value="-80">';
    const powers = new Float32Array(1000).fill(-100);
    powers[99] = -70; powers[100] = -10; powers[101] = -70;
    powers[519] = -70; powers[520] = -30; powers[521] = -70;
    S.setFreqArray(new Float64Array(1000).map((_, i) => i));
    S.traces[0].powers = powers;
    S.traces[0].mode = 'CLEAR_WRITE';
    S.setRtaMode(false);
    S.markers.forEach(marker => Object.assign(marker, {
      enabled: marker.id === 1,
      mode: marker.id === 1 ? 'NORMAL' : 'OFF',
      tracking: marker.id === 1,
      idx: marker.id === 1 ? 500 : 0,
      freq: marker.id === 1 ? 500 : null,
    }));

    expect(updateTrackingMarkers()).toBe(true);
    expect(S.markers[0].idx).toBe(520);

    S.markers.forEach(marker => { marker.enabled = false; marker.mode = 'OFF'; marker.tracking = false; });
    document.body.innerHTML = previous;
  });
});

describe('实时分位数统计', () => {
  it('无需排序即可近似噪底和峰值分位数', () => {
    const src = new Float32Array(1000).fill(-90);
    for (let i = 980; i < 1000; i++) src[i] = -20;
    expect(percentileApprox(src, 0.3)).toBeCloseTo(-90, 0);
    expect(percentileApprox(src, 0.98)).toBeCloseTo(-20, 0);
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
