// i18n en/zh dictionary — covers all UI text
export type Lang = 'en' | 'zh';

const dict = {
  en: {
    'ref_level': 'Ref Level:', 'scale': 'Scale:', 'rbw': 'RBW:', 'vbw': 'VBW:',
    'pts': 'Pts:', 'swt': 'SWT:', 'bw_label': 'BW:', 'gnss': 'GNSS:', 'clock_ref': 'Clock Ref:',
    'int': 'Int', 'ext': 'Ext', 'ext_force': 'ExtForce', 'output': 'Output',
    'connect': 'Connect SAN-90',
    'frequency': 'Frequency', 'center': 'Center', 'span': 'Span', 'start': 'Start',
    'stop': 'Stop', 'set': 'Set', 'full_span': 'Full Span', 'span_step': 'Span Step',
    'level': 'Level', 'ref_dbm': 'Ref (dBm)', 'scale_short': 'Scale',
    'db_div': 'dB/div', 'offset': 'Offset', 'db': 'dB', 'atten': 'Atten',
    'auto': 'Auto', 'preamp': 'PreAmp', 'off': 'Off', 'if_gain': 'IF Gain',
    'gain_strategy': 'Gain St', 'low_noise': 'Low Noise', 'high_linearity': 'High Linearity',
    'trace': 'Trace', 'mode': 'Mode', 'clear_write': 'Clear Write', 'max_hold': 'Max Hold',
    'min_hold': 'Min Hold', 'average': 'Average', 'view': 'View (Freeze)', 'freeze': 'Freeze',
    'glyph_down': '▼', 'glyph_up': '▲',
    'persist': 'Persist', 'grain': 'Grain',
    'span_down': 'Narrower span', 'span_up': 'Wider span',
    'ref_down': 'Lower ref level', 'ref_up': 'Raise ref level',
    'span_full': 'Full bandwidth (50.8 MHz)', 'span_full_short': 'Full',
    'smooth': 'Smooth', 'off_short': 'Off', 'cal_wnd': 'Cal Wnd', 'auto_rbw': 'Auto (RBW)',
    'normalize': 'Normalize', 'reset_norm': 'Reset Norm', 'clear': 'Clear',
    'marker': 'Marker', 'active': 'Active', 'tracking': 'Tracking', 'peak': 'Peak', 'valley': 'Valley',
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
    'graph': 'Graph', 'mode_swp': 'SWP', 'mode_rta': 'RTA', 'waterfall': 'Waterfall',
    'speed': 'Speed', 'swt_min': 'minSWT',
    'swt_xn': '×N', 'swt_minsmp': 'minSMP×N', 'swt_sec': 'sec',
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
    'pts': '点数:', 'swt': '扫描时间:', 'bw_label': '带宽:', 'gnss': 'GNSS:', 'clock_ref': '参考时钟:',
    'int': '内部', 'ext': '外部', 'ext_force': '外部强制', 'output': '输出',
    'connect': '连接 SAN-90',
    'frequency': '频率', 'center': '中心频率', 'span': '扫宽', 'start': '起始',
    'stop': '终止', 'set': '设置', 'full_span': '全频段', 'span_step': '扫宽步进',
    'level': '电平', 'ref_dbm': '参考 (dBm)', 'scale_short': '刻度',
    'db_div': 'dB/格', 'offset': '偏移', 'db': 'dB', 'atten': '衰减',
    'auto': '自动', 'preamp': '前置放大', 'off': '关', 'if_gain': '中频增益',
    'gain_strategy': '增益策略', 'low_noise': '低噪声', 'high_linearity': '高线性',
    'trace': '迹线', 'mode': '模式', 'clear_write': '清除写入', 'max_hold': '最大保持',
    'min_hold': '最小保持', 'average': '平均', 'view': '查看(冻结)', 'freeze': '冻结',
    'glyph_down': '▼', 'glyph_up': '▲',
    'persist': '余辉', 'grain': '颗粒',
    'span_down': '收窄带宽', 'span_up': '加宽带宽',
    'ref_down': '降低参考电平', 'ref_up': '升高参考电平',
    'span_full': '全带宽 (50.8 MHz)', 'span_full_short': '全带宽',
    'smooth': '平滑', 'off_short': '关', 'cal_wnd': '校准窗口', 'auto_rbw': '自动(RBW)',
    'normalize': '归一化', 'reset_norm': '重置归一化', 'clear': '清除',
    'marker': '游标', 'active': '活动', 'tracking': '追踪', 'peak': '峰值', 'valley': '谷值',
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
    'graph': '图形', 'mode_swp': '扫频', 'mode_rta': '实时', 'waterfall': '瀑布图',
    'speed': '速度', 'swt_min': 'minSWT',
    'swt_xn': '×N', 'swt_minsmp': '最小采样×N', 'swt_sec': '秒',
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

export function t(key: string, params?: Record<string, string | number>): string {
  const en = dict.en as Record<string, string>;
  const raw = current === 'zh'
    ? ((dict.zh as Record<string, string>)[key] ?? en[key] ?? key)
    : (en[key] ?? key);
  if (!params) return raw;
  return raw.replace(/\{(\w+)\}/g, (_, name: string) =>
    params[name] !== undefined ? String(params[name]) : `{${name}}`);
}

/** True when the dictionary defines a key (t() falls back to the key text itself). */
export function hasKey(key: string): boolean {
  const en = dict.en as Record<string, string>;
  return key in en || (dict.zh as Record<string, string>)[key] !== undefined;
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
    else {
      // Tooltip: data-i18n-title overrides the label key; data-i18n-attr="title" marks
      // title-only elements (glyph buttons keep their symbol as text).
      if (el.dataset.i18nTitle) el.title = t(el.dataset.i18nTitle);
      else if (el.dataset.i18nAttr === 'title') el.title = t(k);
      if (el.dataset.i18nAttr !== 'title') el.textContent = t(k);
    }
  });
}

// ---------------------------------------------------------------------------
// v1.2 additions: backend error codes, measurement-mode text and remaining
// tooltips. Kept as an additive assign so the original dictionary stays intact.
// ---------------------------------------------------------------------------
Object.assign(dict.en, {
  // Backend command error codes
  'err_command': 'Command failed',
  'err_unknown_command': 'Unknown command: {cmd}',
  'err_device_not_connected': 'Device is not connected',
  'err_caps_unavailable': 'Device capabilities are unavailable',
  'err_missing_param': 'Missing parameter: {key}',
  'err_not_a_number': '{key} must be a number',
  'err_not_finite': '{key} must be a finite value',
  'err_below_min': '{key} must be >= {min}',
  'err_above_max': '{key} must be <= {max}',
  'err_not_an_integer': '{key} must be an integer',
  'err_invalid_choice': '{key} must be one of {choices}',
  'err_freq_mixed_assignment': 'Use center/span or start/stop, not both',
  'err_freq_requires_pair': 'SET_FREQ requires center/span or start/stop',
  'err_span_too_small': 'Stop - start must be >= 100 Hz',
  'err_range_invalid': 'Start must be lower than stop',
  'err_bool_required': '{key} must be a boolean',
  'err_pnm_unsupported': 'Phase-noise measurement is not supported',
  'err_rta_requires_pair': 'SET_RTA requires center or span',
  'err_rta_mode_required': 'SET_RTA can only be used in RTA mode',
  'err_json_object_required': 'JSON payload must be an object',
  'err_cmd_unavailable_measurement': '{cmd} is not available while the {session} measurement is active',
  'err_swp_only': '{cmd} is only available in SWP mode',
  'err_hardware_config': 'Device rejected the configuration: {detail}',
  'err_connect_failed': 'Device connection failed: {detail}',
  // Frontend text
  'alert_device': 'Device',
  'meas_no_trace': 'No trace data',
  'meas_no_signal': 'No signal',
  'meas_invalid_thresholds': 'Invalid thresholds: enter comma-separated 1~60 dB values',
  'measuring': 'measuring...',
  'measuring_pct': 'measuring... {pct}%',
  'measuring_updating': 'measuring... updating',
  'please_wait': 'please wait...',
  'pnm_title_measuring': 'PHASE NOISE  (measuring... {pct}%)',
  'pnm_title_updating': 'PHASE NOISE  (updating...)',
  'pnm_offset_axis': 'Offset',
  'pnm_unit': 'dBc/Hz',
  'pnm_carrier': 'Carrier',
  'pnm_frequency': 'Frequency',
  'pnm_offset': 'Offset',
  'pnm_power': 'Power',
  'pnm_table_title': 'Phase Noise',
  'auto_needs_atten': 'Auto Ref requires Atten Auto',
  // Tooltips
  'tip_collapse_all': 'Collapse All / Expand All',
  'tip_collapse_panel': 'Collapse / Expand panel',
  'tip_waterfall_pause': 'Pause / Resume waterfall',
  'tip_waterfall_reset': 'Restart waterfall from scratch',
  'tip_preset': 'Restore default settings',
  'tip_lang': 'Switch language',
  'tip_fft_window': 'FFT window: FlatTop / B-Nuttall / LowSideLobe / Rectangle / Kaiser',
  'tip_persist': 'Persistence (density fade)',
});

Object.assign(dict.zh, {
  'err_command': '命令执行失败',
  'err_unknown_command': '未知命令：{cmd}',
  'err_device_not_connected': '设备未连接',
  'err_caps_unavailable': '设备能力信息不可用',
  'err_missing_param': '缺少参数：{key}',
  'err_not_a_number': '{key} 必须是数字',
  'err_not_finite': '{key} 必须是有限数值',
  'err_below_min': '{key} 必须 ≥ {min}',
  'err_above_max': '{key} 必须 ≤ {max}',
  'err_not_an_integer': '{key} 必须是整数',
  'err_invalid_choice': '{key} 必须是以下之一：{choices}',
  'err_freq_mixed_assignment': '请使用 center/span 或 start/stop，两者不能混用',
  'err_freq_requires_pair': 'SET_FREQ 需要 center/span 或 start/stop',
  'err_span_too_small': '终止频率与起始频率之差必须 ≥ 100 Hz',
  'err_range_invalid': '起始频率必须小于终止频率',
  'err_bool_required': '{key} 必须是布尔值',
  'err_pnm_unsupported': '本设备不支持相位噪声测量',
  'err_rta_requires_pair': 'SET_RTA 需要 center 或 span',
  'err_rta_mode_required': 'SET_RTA 仅可在实时(RTA)模式下使用',
  'err_json_object_required': 'JSON 内容必须是对象',
  'err_cmd_unavailable_measurement': '{session}测量进行中，无法使用 {cmd}',
  'err_swp_only': '{cmd} 仅可在普通频谱(SWP)模式下使用',
  'err_hardware_config': '设备拒绝该配置：{detail}',
  'err_connect_failed': '设备连接失败：{detail}',
  'alert_device': '设备',
  'meas_no_trace': '暂无迹线数据',
  'meas_no_signal': '未检测到信号',
  'meas_invalid_thresholds': '阈值无效：请输入 1~60 dB 的逗号分隔数值',
  'measuring': '测量中…',
  'measuring_pct': '测量中… {pct}%',
  'measuring_updating': '测量中… 更新中',
  'please_wait': '请稍候…',
  'pnm_title_measuring': '相位噪声（测量中… {pct}%）',
  'pnm_title_updating': '相位噪声（更新中…）',
  'pnm_offset_axis': '频偏',
  'pnm_unit': 'dBc/Hz',
  'pnm_carrier': '载波',
  'pnm_frequency': '频率',
  'pnm_offset': '频偏',
  'pnm_power': '功率',
  'pnm_table_title': '相位噪声',
  'auto_needs_atten': '自动参考电平需要将衰减设为自动',
  'tip_collapse_all': '全部折叠 / 展开',
  'tip_collapse_panel': '折叠 / 展开面板',
  'tip_waterfall_pause': '暂停 / 继续瀑布图',
  'tip_waterfall_reset': '重新开始瀑布图',
  'tip_preset': '恢复默认设置',
  'tip_lang': '切换语言',
  'tip_fft_window': 'FFT 窗类型：平顶 / B-Nuttall / 低旁瓣 / 矩形 / Kaiser',
  'tip_persist': '余辉（密度衰减）',
});
