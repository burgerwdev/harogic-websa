// i18n en/zh dictionary — covers all UI text
export type Lang = 'en' | 'zh';

const dict = {
  en: {
    'ref_level': 'Ref Level:', 'scale': 'Scale:', 'rbw': 'RBW:', 'vbw': 'VBW:',
    'pts': 'Pts:', 'swt': 'SWT:', 'gnss': 'GNSS:', 'clock_ref': 'Clock Ref:',
    'int': 'Int', 'ext': 'Ext', 'ext_force': 'ExtForce', 'output': 'Output',
    'connect': 'Connect SAN-90',
    'frequency': 'Frequency', 'center': 'Center', 'span': 'Span', 'start': 'Start',
    'stop': 'Stop', 'set': 'Set', 'full_span': 'Full Span (100kHz - 9GHz)',
    'level': 'Level', 'ref_dbm': 'Ref (dBm)', 'scale_short': 'Scale',
    'db_div': 'dB/div', 'offset': 'Offset', 'db': 'dB', 'atten': 'Atten',
    'auto': 'Auto', 'preamp': 'PreAmp', 'off': 'Off', 'if_gain': 'IF Gain',
    'gain_strategy': 'Gain St', 'low_noise': 'Low Noise', 'high_linearity': 'High Linearity',
    'trace': 'Trace', 'mode': 'Mode', 'clear_write': 'Clear Write', 'max_hold': 'Max Hold',
    'min_hold': 'Min Hold', 'average': 'Average', 'view': 'View (Freeze)',
    'smooth': 'Smooth', 'off_short': 'Off', 'cal_wnd': 'Cal Wnd', 'auto_rbw': 'Auto (RBW)',
    'normalize': 'Normalize', 'reset_norm': 'Reset Norm', 'clear': 'Clear',
    'marker': 'Marker', 'active': 'Active', 'peak': 'Peak', 'valley': 'Valley',
    'center_btn': 'Center', 'mkr_center': 'Mkr→Center', 'pk_list': 'Pk List',
    'pk_auto': 'Auto', 'pk_off': 'Off', 'count': 'Count',
    'all_on': 'All On', 'all_off': 'All Off',
    'measurement': 'Measurement', 'measure': 'Measure', 'amplitude': 'Amplitude',
    'harmonic': 'Harmonic', 'phase_noise': 'PhaseNoise', 'n_db': 'N dB',
    'fund': 'Fund', 'orders': 'Orders', 'span_short': 'Span', 'carrier': 'Carrier',
    'range': 'Range', 'threshold': 'Threshold', 'avg': 'Avg', 'smooth_short': 'Smooth',
    'meas': 'Meas', 'apply': 'Apply',
    'bw': 'BW', 'manual': 'Manual', 'auto_span2000': 'Auto (Span/2000)',
    'window': 'Window', 'bypass_10x': 'Bypass (10×RBW)', 'eq_rbw': '= RBW',
    'tenth_rbw': '= 0.1×RBW',
    'sweep': 'Sweep', 'points': 'Points', 'spur': 'Spur', 'bypass': 'Bypass',
    'standard': 'Standard', 'enhanced': 'Enhanced', 'gapfill': 'GapFill', 'on': 'On',
    'uid': 'UID:', 'hw': 'HW:', 'mfw': 'MFW:', 'ffw': 'FFW:', 'bus': 'BUS:',
    'api': 'API:', 'warn': 'Warn:', 'preset': 'Preset',
    'theme': 'Theme:', 'dark': 'Dark', 'light': 'Light', 'lang': 'Lang:',
    'status_connected': 'Connected', 'status_disconnected': 'Disconnected',
    'status_locked': 'Locked', 'status_nolock': 'NoLock',
    'gnss_detail': 'GNSS Detail', 'gnss_lock': 'Lock', 'gnss_sats': 'Satellites',
    'gnss_docxo': 'DOCXO', 'gnss_time': 'Time', 'gnss_close': 'Close',
    'gnss_latitude': 'Latitude', 'gnss_longitude': 'Longitude', 'gnss_altitude': 'Altitude',
    'gnss_antenna': 'Antenna', 'gnss_docxo_mode': 'DOCXO Mode',
    'gnss_ext_ant': 'External', 'gnss_int_ant': 'Internal',
    'gnss_utc_time': 'GNSS UTC Time', 'gnss_local_time': 'System Time',
    'gnss_docxo_lock': 'Disciplined', 'gnss_docxo_hold': 'Hold',
    'ref_clock_out': 'Reference clock output on/off',
    // OSD / Status
    'osd_ref': 'Ref', 'osd_scale': 'dB/div', 'osd_rbw': 'RBW', 'osd_vbw': 'VBW',
    'osd_sweep': 'Sweep', 'osd_pts': 'Pts',
    // Marker table header
    'mk_marker': 'Marker', 'mk_mode': 'Mode', 'mk_freq': 'Frequency / ΔFreq',
    'mk_amp': 'Amplitude / ΔPower', 'mk_ref': 'Reference',
    // Measurement overlay
    'm3db_bw': '3dB BW', 'm3db_center': 'Center', 'm3db_q': 'Q',
    'harm_table_title': 'Harmonics', 'pnm_title': 'Phase Noise',
    // Tooltips
    'tip_connect': 'Connect to device', 'tip_norm': 'Store current trace as reference and enable normalize',
    'tip_atten': 'Manual attenuation (Auto=-1)', 'tip_peakthr': 'Peak threshold (dBm); Auto = global peak - 50dB, manual edit keeps your value',
    'tip_peakcnt': 'Number of peaks to display (1~20, single row)',
    'tip_refwin': 'Normalize reference smoothing window',
    'tip_harm_span': 'Narrow sweep span around each harmonic (auto-set by fundamental, 1Hz~100MHz)',
    'tip_harm_orders': 'Harmonic orders 1~10',
    'tip_pnm_carrier': 'Carrier frequency',
    'tip_pnm_thr': 'Carrier detect threshold (dBm), official default -50',
    'tip_pnm_avg': 'Trace average count (1~1000)',
    'tip_pnm_smooth': 'Trace smoothing (frontend, like official software)',
    'tip_pnm_win': 'Smooth window (% of trace length, 0~10)',
    'tip_ampdbs': 'Comma-separated thresholds below peak (1~60 dB)',
    'tip_gain': 'Display amplitude offset for external amp/attenuator (display only)',
  },
  zh: {
    'ref_level': '参考电平:', 'scale': '刻度:', 'rbw': '分辨率带宽:', 'vbw': '视频带宽:',
    'pts': '点数:', 'swt': '扫描时间:', 'gnss': 'GNSS:', 'clock_ref': '参考时钟:',
    'int': '内部', 'ext': '外部', 'ext_force': '外部强制', 'output': '输出',
    'connect': '连接 SAN-90',
    'frequency': '频率', 'center': '中心频率', 'span': '扫宽', 'start': '起始',
    'stop': '终止', 'set': '设置', 'full_span': '全频段 (100kHz - 9GHz)',
    'level': '电平', 'ref_dbm': '参考 (dBm)', 'scale_short': '刻度',
    'db_div': 'dB/格', 'offset': '偏移', 'db': 'dB', 'atten': '衰减',
    'auto': '自动', 'preamp': '前置放大', 'off': '关', 'if_gain': '中频增益',
    'gain_strategy': '增益策略', 'low_noise': '低噪声', 'high_linearity': '高线性',
    'trace': '迹线', 'mode': '模式', 'clear_write': '清除写入', 'max_hold': '最大保持',
    'min_hold': '最小保持', 'average': '平均', 'view': '查看(冻结)',
    'smooth': '平滑', 'off_short': '关', 'cal_wnd': '校准窗口', 'auto_rbw': '自动(RBW)',
    'normalize': '归一化', 'reset_norm': '重置归一化', 'clear': '清除',
    'marker': '游标', 'active': '活动', 'peak': '峰值', 'valley': '谷值',
    'center_btn': '居中', 'mkr_center': '游标→中心', 'pk_list': '峰值列表',
    'pk_auto': '自动', 'pk_off': '关', 'count': '数量',
    'all_on': '全部开启', 'all_off': '全部关闭',
    'measurement': '测量', 'measure': '测量', 'amplitude': '幅度',
    'harmonic': '谐波', 'phase_noise': '相位噪声', 'n_db': 'N dB',
    'fund': '基频', 'orders': '谐波次数', 'span_short': '扫宽', 'carrier': '载波',
    'range': '范围', 'threshold': '阈值', 'avg': '平均', 'smooth_short': '平滑',
    'meas': '测量', 'apply': '应用',
    'bw': '带宽', 'manual': '手动', 'auto_span2000': '自动(扫宽/2000)',
    'window': '窗口', 'bypass_10x': '旁路 (10×RBW)', 'eq_rbw': '= RBW',
    'tenth_rbw': '= 0.1×RBW',
    'sweep': '扫描', 'points': '点数', 'spur': '杂散', 'bypass': '旁路',
    'standard': '标准', 'enhanced': '增强', 'gapfill': '间隙填充', 'on': '开',
    'uid': 'UID:', 'hw': 'HW:', 'mfw': 'MFW:', 'ffw': 'FFW:', 'bus': 'BUS:',
    'api': 'API:', 'warn': '告警:', 'preset': '预设',
    'theme': '主题:', 'dark': '深色', 'light': '浅色', 'lang': '语言:',
    'status_connected': '已连接', 'status_disconnected': '未连接',
    'status_locked': '已锁定', 'status_nolock': '未锁定',
    'gnss_detail': 'GNSS 详情', 'gnss_lock': '锁定', 'gnss_sats': '卫星数',
    'gnss_docxo': 'DOCXO', 'gnss_time': '时间', 'gnss_close': '关闭',
    'gnss_latitude': '纬度', 'gnss_longitude': '经度', 'gnss_altitude': '海拔',
    'gnss_antenna': '天线', 'gnss_docxo_mode': 'DOCXO 模式',
    'gnss_ext_ant': '外部', 'gnss_int_ant': '内部',
    'gnss_utc_time': 'GNSS UTC 时间', 'gnss_local_time': '系统时间',
    'gnss_docxo_lock': '驯服', 'gnss_docxo_hold': '跟踪',
    'ref_clock_out': '参考时钟输出开/关',
    'osd_ref': '参考', 'osd_scale': 'dB/格', 'osd_rbw': 'RBW', 'osd_vbw': 'VBW',
    'osd_sweep': '扫描', 'osd_pts': '点数',
    'mk_marker': '游标', 'mk_mode': '模式', 'mk_freq': '频率 / Δ频率',
    'mk_amp': '幅度 / Δ功率', 'mk_ref': '参考',
    'm3db_bw': '3dB 带宽', 'm3db_center': '中心', 'm3db_q': 'Q值',
    'harm_table_title': '谐波', 'pnm_title': '相位噪声',
    'tip_connect': '连接设备', 'tip_norm': '将当前迹线存为参考并启用归一化',
    'tip_atten': '手动衰减(自动=-1)', 'tip_peakthr': '峰值阈值(dBm); 自动=全局峰值-50dB, 手动编辑保留您的值',
    'tip_peakcnt': '要显示的峰数(1~20, 单行)',
    'tip_refwin': '归一化参考平滑窗口',
    'tip_harm_span': '每个谐波周围的窄扫宽(由基频自动设置, 1Hz~100MHz)',
    'tip_harm_orders': '谐波次数 1~10',
    'tip_pnm_carrier': '载波频率',
    'tip_pnm_thr': '载波检测阈值(dBm), 官方默认 -50',
    'tip_pnm_avg': '迹线平均次数(1~1000)',
    'tip_pnm_smooth': '迹线平滑(前端, 同官方软件)',
    'tip_pnm_win': '平滑窗口(迹线长度百分比, 0~10)',
    'tip_ampdbs': '峰值以下的阈值列表(逗号分隔, 1~60 dB)',
    'tip_gain': '外部放大器/衰减器的显示幅度偏移(仅显示)',
  },
} as const;

export type I18nKey = keyof typeof dict['en'];

let current: Lang = 'en';
const listeners = new Set<(l: Lang) => void>();

export function t(key: string): string {
  const en = dict.en as Record<string, string>;
  if (current === 'zh') {
    const zh = dict.zh as Record<string, string>;
    return zh[key] ?? en[key] ?? key;
  }
  return en[key] ?? key;
}

export function getLang(): Lang { return current; }

export function setLang(l: Lang) {
  current = l;
  document.documentElement.lang = l === 'zh' ? 'zh-CN' : 'en';
  listeners.forEach((fn) => fn(l));
}

export function onLangChange(fn: (l: Lang) => void) {
  listeners.add(fn);
}

// Render all data-i18n nodes (text or placeholder/title)
export function applyI18n(root: HTMLElement | Document = document) {
  root.querySelectorAll<HTMLElement>('[data-i18n]').forEach((el) => {
    const k = el.dataset.i18n!;
    if (el.dataset.i18nAttr === 'placeholder') el.setAttribute('placeholder', t(k));
    else if (el.dataset.i18nAttr === 'title') el.title = t(k);
    else el.textContent = t(k);
  });
}
