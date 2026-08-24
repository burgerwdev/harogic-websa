// 合成测试数据: 模拟频谱仪迹线(无设备依赖)
// - synthCW: 单个 CW 信号(高斯峰) + 噪声底
// - synthBandpass: 带通滤波器响应(通带 + 两侧深谷 + 平坦阻带) —— 寻谷测试场景

export function synthCW(pts = 1000, opts: { peakIdx?: number; peakDb?: number; floor?: number; noiseAmp?: number } = {}) {
  const { peakIdx = 500, peakDb = -20, floor = -90, noiseAmp = 0.5 } = opts;
  const p = new Float32Array(pts);
  for (let i = 0; i < pts; i++) {
    const d = (i - peakIdx) / 30;          // 高斯峰宽 ~30bin
    const gauss = Math.exp(-(d * d));
    p[i] = floor + (peakDb - floor) * gauss + (Math.random() - 0.5) * noiseAmp;
  }
  return p;
}

// 带通: 通带 [lo, hi] 为 -20dB, 两侧阻带 -85dB 且各含一个深谷(局部极小),
// 边缘用 sigmoid 过渡 —— 匹配真实滤波器"带通两侧深谷"测量场景
export function synthBandpass(pts = 1000, opts: { lo?: number; hi?: number; passDb?: number; stopDb?: number; dipDb?: number } = {}) {
  const { lo = 0.3, hi = 0.7, passDb = -20, stopDb = -85, dipDb = -110 } = opts;
  const p = new Float32Array(pts);
  const edge = 12;   // 过渡带宽度(bin)
  const dips = [0.15, 0.85];   // 两侧深谷位置(归一化 x)
  for (let i = 0; i < pts; i++) {
    const x = i / pts;
    // 两侧 sigmoid 过渡
    const rise = 1 / (1 + Math.exp(-(x - lo) * 4 * edge));
    const fall = 1 / (1 + Math.exp((x - hi) * 4 * edge));
    const resp = stopDb + (passDb - stopDb) * Math.min(rise, fall);
    // 深谷: 高斯凹陷(宽 ~12bin, 局部极小)
    let dip = 0;
    for (const dx of dips) {
      const d = (x - dx) * pts / 2;   // 尖深谷(相邻 bin 差 >3dB, 匹配真实滤波器谷)
      dip = Math.min(dip, (dipDb - stopDb) * Math.exp(-(d * d)));
    }
    p[i] = resp + dip + (Math.random() - 0.5) * 0.4;
  }
  return p;
}

// 双峰(两个孤立信号, 用于寻峰去重/Excursion)
export function synthTwoPeaks(pts = 1000) {
  const p = new Float32Array(pts);
  for (let i = 0; i < pts; i++) {
    let v = -95;
    for (const c of [{ idx: 300, db: -30 }, { idx: 650, db: -25 }]) {
      const d = (i - c.idx) / 12;
      v = Math.max(v, -95 + (c.db + 95) * Math.exp(-(d * d)));
    }
    p[i] = v;
  }
  return p;
}
