import { getLang } from '../core/i18n';
import type { NativeMeasurement } from './measurement';

/** Optional additive metadata; older numeric-only FT8 producers remain supported. */
export interface Ft8SnrMeasurement extends Partial<NativeMeasurement> {
  value: number | null;
  state?: string;
  quality?: string;
  value_kind?: string;
  reason?: string | null;
  quality_reasons?: string[];
}

export function ft8SnrValue(value: unknown, measurement?: Ft8SnrMeasurement): number | null {
  const v = measurement ? measurement.value : value;
  return typeof v === 'number' && Number.isFinite(v) ? v : null;
}

const reasons: Record<string, [string, string]> = {
  no_reliable_symbol_fit: ['No reliable symbol fit', '无法可靠拟合码元'],
  insufficient_complete_symbols: ['Too few complete symbols', '完整码元不足'],
  invalid_measurement_input: ['Invalid measurement input or codeword', '测量输入或码字无效'],
  nonfinite_iq: ['IQ contains nonfinite samples', 'IQ 含非有限样本'],
  noise_not_resolved: ['Noise power cannot be resolved', '无法分辨噪声功率'],
  signal_not_resolved: ['Signal power cannot be resolved above background', '无法从背景中分辨信号功率'],
  off_tone_interference: ['Noise references contain interference', '噪声参考受到干扰'],
  nonflat_background: ['Background differs across the signal', '信号两侧背景不平坦'],
  limited_reference_support: ['Limited noise references at receive edge', '通带边缘的噪声参考有限'],
  partial_burst: ['Only the observed part of the burst is estimated', '仅估计已观测到的部分突发'],
  clipping_detected: ['Repeated IQ peaks indicate possible clipping', 'IQ 峰值重复，可能存在削顶'],
  sync_power_not_resolved: ['Sync power is poorly resolved', '同步码元功率分辨不足'],
  high_statistical_uncertainty: ['High statistical uncertainty', '统计不确定度较大'],
};

export function ft8SnrPresentation(value: unknown, measurement?: Ft8SnrMeasurement): { text: string; title: string; degraded: boolean } {
  const zh = getLang() === 'zh', v = ft8SnrValue(value, measurement);
  const flags = measurement?.quality_reasons ?? [];
  const why = [...new Set([...(measurement?.reason ? [measurement.reason] : []), ...flags])]
    .map(reason => reasons[reason]?.[zh ? 1 : 0] ?? reason).join('; ');
  const kind = measurement?.value_kind ?? 'estimate';
  const label = kind === 'lower_bound' ? (zh ? '下界' : 'Lower bound')
    : kind === 'upper_bound' ? (zh ? '上界' : 'Upper bound')
    : kind === 'clipped' ? (zh ? '限幅值' : 'Clipped report') : (zh ? '估计值' : 'Estimate');
  const degraded = measurement?.quality === 'degraded' || flags.length > 0;
  const rounded = (value: number, digits: number): string => {
    const text = value.toFixed(digits);
    return Number(text) === 0 ? (0).toFixed(digits) : text;
  };
  return {
    text: v === null ? '—' : `${kind === 'lower_bound' ? '≥' : kind === 'upper_bound' ? '≤' : ''}${rounded(kind === 'lower_bound' ? Math.floor(v) : kind === 'upper_bound' ? Math.ceil(v) : v, 0)}`,
    title: v === null ? `${zh ? 'SNR 无法估计' : 'SNR unavailable'}: ${why || (zh ? '未提供测量原因' : 'No measurement reason supplied')}`
      : `${label} ${rounded(v, 2)} dB / 2500 Hz${degraded ? ` · ${zh ? '质量受限' : 'Limited quality'}` : ''}${why ? `: ${why}` : ''}`,
    degraded,
  };
}
