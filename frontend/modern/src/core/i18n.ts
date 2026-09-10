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
  // Labels that are computed (not plain dictionary entries) must also follow the language.
  // The theme button shows the *target* theme, which depends on the active theme.
  const themeBtn = document.getElementById('btn-theme');
  if (themeBtn) {
    themeBtn.textContent = document.documentElement.dataset.theme === 'dark' ? t('dark') : t('light');
  }
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

// Reference-clock transient hint + hover tooltip (approach C)
Object.assign(dict.en, {
    'refclk_applied': 'applied',
  'refclk_fallback': 'external lost lock, fell back to internal',
  'refclk_forced': 'external forced (no fallback on unlock)',
  'refclk_unverified': 'applied, lock not confirmed',
  'tip_refclk_requested': 'Requested',
  'tip_refclk_actual': 'Actual source',
  'tip_refclk_freq': 'Reported reference frequency',
  'tip_refclk_ppm': 'Frequency offset (after GNSS calibration)',
  'tip_refclk_output': 'Clock output',
  'tip_refclk_state': 'State',
  'tip_refclk_forced_warn': 'Warning: ExtForce never falls back; a lost reference detunes the analyzer.',
});

Object.assign(dict.zh, {
    'refclk_applied': '已应用',
  'refclk_fallback': '外部失锁，已回退内部',
  'refclk_forced': '外部强制（失锁不回落）',
  'refclk_unverified': '已应用，锁定未确认',
  'tip_refclk_requested': '请求源',
  'tip_refclk_actual': '实际源',
  'tip_refclk_freq': '回读参考频率',
  'tip_refclk_ppm': '频率偏差（GNSS 校准后可用）',
  'tip_refclk_output': '时钟输出',
  'tip_refclk_state': '状态',
  'tip_refclk_forced_warn': '警告：外部强制失锁不回落，参考丢失会导致整机频率失准。',
});

Object.assign(dict.en, {
  'refclk_detail': 'Reference Clock',
  'refclk_current': 'Requested',
  'refclk_actual_src': 'Actual source',
  'refclk_fell_back': 'fell back',
  'refclk_gnss': 'GNSS',
  'refclk_cal': 'Calibration',
  'refclk_calibrate': 'Calibrate',
  'refclk_calibrating': 'Calibrating...',
  'refclk_cal_need_gnss': 'GNSS is not locked; calibration needs 1PPS.',
  'refclk_cal_failed': 'Calibration failed or timed out.',
});
Object.assign(dict.zh, {
  'refclk_detail': '参考时钟',
  'refclk_current': '请求参考',
  'refclk_actual_src': '实际源',
  'refclk_fell_back': '已回退',
  'refclk_gnss': 'GNSS',
  'refclk_cal': '校准',
  'refclk_calibrate': '校准',
  'refclk_calibrating': '校准中…',
  'refclk_cal_need_gnss': 'GNSS 未锁定，校准需要 1PPS 信号。',
  'refclk_cal_failed': '校准失败或超时。',
});

Object.assign(dict.en, { 'avg': 'Avg', 'export_csv': 'CSV' });
Object.assign(dict.zh, { 'avg': '平均', 'export_csv': 'CSV' });

Object.assign(dict.en, { 'avg_opt_16': '16 (default)' });
Object.assign(dict.zh, { 'avg_opt_16': '16（默认）' });

Object.assign(dict.en, { 'detector': 'Detector' });
Object.assign(dict.zh, { 'detector': '检波' });

Object.assign(dict.en, {
  'tip_detector': 'Trace detector (SWP only). Auto Sample follows the signal (default). '
    + 'Sample / Pos Peak / Neg Peak / RMS are fixed detectors. '
    + 'Auto Peak is not offered in the UI: measured on SAN-90 it loses a steady CW carrier, so use it only via the API for pulsed signals.',
});
Object.assign(dict.zh, {
  'tip_detector': '迹线检波器（仅普通频谱 SWP）。自动取样跟随信号（默认）；随机/正峰值/负峰值/RMS 为固定检波方式；'
    + '自动峰值不在界面提供：在 SAN-90 上实测会丢失稳定 CW 载波，仅在脉冲场景下通过 API 使用。',
});

Object.assign(dict.en, { 'norm_cleared': 'Normalization cleared (measurement settings changed)' });
Object.assign(dict.zh, { 'norm_cleared': '归一化已清除（测量参数已变化）' });

// Tooltips for every interactive control + remaining bottom-bar literals
Object.assign(dict.en, {
  'tip_select-refclk': 'Reference clock source: Int / Ext / ExtForce (output switch is in the popover)',
  'tip_btn-refclk-out': 'Toggle the 10 MHz reference clock output',
  'tip_btn-connect': 'Connect to the analyzer over USB',
  'tip_btn-waterfall': 'Waterfall view (newest row on top)',
  'tip_btn-mode-rta': 'Switch between swept spectrum (SWP) and real-time spectrum (RTA)',
  'tip_input-rta-center': 'RTA center frequency; type a value then click a unit to apply',
  'tip_select-rta-span': 'RTA analysis bandwidth: 50.78 MHz / 2^n',
  'tip_select-rta-bins': 'Amplitude bins of the probability-density background',
  'tip_input-center': 'Center frequency; type a value then click a unit to apply',
  'tip_input-span': 'Span; type a value then click a unit to apply',
  'tip_input-start': 'Start frequency of the sweep',
  'tip_input-stop': 'Stop frequency of the sweep',
  'tip_input-span-step': 'Span step used by the ▼/▲ buttons; follows the span while Auto is on',
  'tip_btn-span-step-auto': 'Let the span step follow the current span automatically',
  'tip_input-ref': 'Reference level: top of the display scale (Manual also applies it to the device)',
  'tip_btn-ref-set': 'Apply the reference level to the device',
  'tip_btn-ref-auto': 'Automatic reference level (requires Atten = Auto)',
  'tip_select-preamp': 'Preamplifier: Auto or forced off',
  'tip_select-ifgain': 'IF gain grade (0-3); higher grade raises the noise floor',
  'tip_select-gainstrategy': 'Gain strategy: low noise or high linearity',
  'tip_select-trace-mode': 'Trace mode: Clear Write / Max Hold / Min Hold / Average / Off',
  'tip_btn-view-freeze': 'Freeze the active trace (View) and restore its previous mode',
  'tip_select-trace-avg': 'Average depth for the active trace: 2-256 or ∞ (exponential, never freezes)',
  'tip_select-smooth': 'Display smoothing: Savitzky-Golay, peak preserving',
  'tip_btn-markers-all': 'Turn all four markers on or off',
  'tip_btn-marker-tracking': 'Track the nearest signal peak with the active marker',
  'tip_btn-peaklist': 'Show the peak list table',
  'tip_btn-meas-onoff': 'Enable the measurement modes (Amplitude / Harmonic / Phase Noise)',
  'tip_tab-amp': 'n-dB bandwidth and center frequency of a peak',
  'tip_tab-harm': 'Harmonic measurement: the device auto-tunes to H1..Hn',
  'tip_tab-pnm': 'Phase-noise measurement at 100 Hz - 10 MHz offsets',
  'tip_btn-amp-meas': 'Measure the n-dB bandwidths listed on the left',
  'tip_btn-amp-clear': 'Clear the amplitude measurement result',
  'tip_btn-harm-set': 'Apply the fundamental frequency, orders and per-harmonic span',
  'tip_btn-pnm-set': 'Apply the phase-noise parameters',
  'tip_btn-pnm-thr': 'Apply the carrier detection threshold',
  'tip_btn-pnm-avg': 'Apply the trace average count for phase noise',
  'tip_input-rbw': 'Resolution bandwidth used when RBW mode is Manual',
  'tip_select-rbw-mode': 'RBW mode: Manual or Auto (span / 2000)',
  'tip_input-vbw': 'Video bandwidth used when VBW mode is Manual',
  'tip_select-vbw-mode': 'VBW mode: bypass (10×RBW), = RBW, 0.1×RBW or Manual',
  'tip_btn-vbw-set': 'Apply the video bandwidth settings',
  'tip_input-points': 'Trace points requested from the device (51-4000; the device may return fewer)',
  'tip_btn-points': 'Apply the requested point count',
  'tip_select-sweep-mode': 'Sweep time: minSWT family, Manual seconds or minSMP×N',
  'tip_select-spur': 'Spur rejection level (SWP only; a slower sweep trades for cleaner spurs)',
  'tip_btn-gapfill': 'Fill invalid frequency gaps in the trace by interpolation',
  'device': 'Device',
  'model': 'Model',
  'docxo_premium': 'Int+ (DOCXO)',
  'rta_fast': 'Fast',
  'rta_medium': 'Medium',
  'rta_slow': 'Slow',
  'rta_vslow': 'Very Slow',
  'tip_sweep_time': 'For ×N: multiplier; for Manual: seconds',
});
Object.assign(dict.zh, {
  'tip_select-refclk': '参考时钟源：内部 / 外部 / 外部强制（输出开关在同一浮层内）',
  'tip_btn-refclk-out': '开关 10 MHz 参考时钟输出',
  'tip_btn-connect': '通过 USB 连接频谱仪',
  'tip_btn-waterfall': '瀑布图视图（最新行在顶部）',
  'tip_btn-mode-rta': '在普通扫频(SWP)与实时频谱(RTA)之间切换',
  'tip_input-rta-center': '实时频谱中心频率；输入数值后点单位即生效',
  'tip_select-rta-span': '实时频谱分析带宽：50.78 MHz / 2^n',
  'tip_select-rta-bins': '概率密度背景的幅度分档数',
  'tip_input-center': '中心频率；输入数值后点单位即生效',
  'tip_input-span': '扫宽；输入数值后点单位即生效',
  'tip_input-start': '扫频起始频率',
  'tip_input-stop': '扫频终止频率',
  'tip_input-span-step': '▼/▲ 按钮使用的扫宽步进；Auto 开启时随扫宽联动',
  'tip_btn-span-step-auto': '让步进自动跟随当前扫宽',
  'tip_input-ref': '参考电平：显示刻度顶值（Manual 同时下发设备）',
  'tip_btn-ref-set': '将参考电平下发设备',
  'tip_btn-ref-auto': '自动参考电平（需要衰减为自动）',
  'tip_select-preamp': '前置放大器：自动或强制关闭',
  'tip_select-ifgain': '中频增益档位（0-3）；档位越高噪底越高',
  'tip_select-gainstrategy': '增益策略：低噪声或高线性',
  'tip_select-trace-mode': '迹线模式：清除写入 / 最大保持 / 最小保持 / 平均 / 关闭',
  'tip_btn-view-freeze': '冻结当前迹线（查看），再次点击恢复原模式',
  'tip_select-trace-avg': '当前迹线的平均深度：2-256 或 ∞（指数平均，不会冻结）',
  'tip_select-smooth': '显示平滑：Savitzky-Golay，保峰',
  'tip_btn-markers-all': '四个游标全部开启 / 关闭',
  'tip_btn-marker-tracking': '让当前游标追踪最近的信号峰',
  'tip_btn-peaklist': '显示峰值列表',
  'tip_btn-meas-onoff': '启用测量模式（幅度 / 谐波 / 相噪）',
  'tip_tab-amp': '峰值的 n-dB 带宽与中心频率',
  'tip_tab-harm': '谐波测量：设备自动调谐到 H1..Hn',
  'tip_tab-pnm': '相位噪声测量，频偏 100 Hz - 10 MHz',
  'tip_btn-amp-meas': '按左侧列出的 n-dB 值进行带宽测量',
  'tip_btn-amp-clear': '清除幅度测量结果',
  'tip_btn-harm-set': '应用基频、谐波次数与每次谐波扫宽',
  'tip_btn-pnm-set': '应用相噪测量参数',
  'tip_btn-pnm-thr': '应用载波检测门限',
  'tip_btn-pnm-avg': '应用相噪迹线平均次数',
  'tip_input-rbw': 'RBW 模式为手动时使用的分辨率带宽',
  'tip_select-rbw-mode': 'RBW 模式：手动或自动（扫宽 / 2000）',
  'tip_input-vbw': 'VBW 模式为手动时使用的视频带宽',
  'tip_select-vbw-mode': 'VBW 模式：旁路(10×RBW)、=RBW、0.1×RBW 或手动',
  'tip_btn-vbw-set': '应用视频带宽设置',
  'tip_input-points': '向设备请求的迹线点数（51-4000；设备实际可能更少）',
  'tip_btn-points': '应用请求的点数',
  'tip_select-sweep-mode': '扫描时间：minSWT 系列、Manual 秒或 minSMP×N',
  'tip_select-spur': '杂散抑制等级（仅 SWP；抑制越强扫描越慢）',
  'tip_btn-gapfill': '对迹线中的无效频率间隙做插值填充',
  'device': '设备',
  'model': '型号',
  'docxo_premium': 'Int+ (DOCXO)',
  'rta_fast': '快',
  'rta_medium': '中',
  'rta_slow': '慢',
  'rta_vslow': '很慢',
  'tip_sweep_time': '×N 为倍率；Manual 为绝对秒数',
});

// Tooltips for normalize / theme and the scale unit
Object.assign(dict.en, {
  'tip_normalize': 'Through calibration: store the active trace as reference and turn normalization on',
  'tip_theme': 'Switch between the dark and light theme',
  'db_per_div': 'dB/div',
});
Object.assign(dict.zh, {
  'tip_normalize': '直通校准：把当前迹线存为参考并开启归一化',
  'tip_theme': '在深色与浅色主题之间切换',
  'db_per_div': 'dB/格',
});

// Language / theme labels now show the current state, so the tooltips explain the click
Object.assign(dict.en, {
  'tip_lang': 'Language: click to switch between English and Chinese',
  'tip_theme': 'Theme: click to switch between dark and light',
});
Object.assign(dict.zh, {
  'tip_lang': '当前语言；点击在中文和 English 之间切换',
  'tip_theme': '当前主题；点击在深色和浅色之间切换',
});

Object.assign(dict.en, { 'tip_version': 'Frontend version' });
Object.assign(dict.zh, { 'tip_version': '前端版本' });
Object.assign(dict.en, { 'tip_export_csv': 'Export the active trace (frequency, power) as CSV' });
Object.assign(dict.zh, { 'tip_export_csv': '把当前迹线导出为 CSV（频率、功率）' });

// Amplitude unit + external gain/loss offset
Object.assign(dict.en, {
  'amp_unit': 'Unit',
  'tip_level_unit': 'Amplitude unit for trace readouts (marker, peaks, channel power, limits); differences stay in dB',
  'tip_gain': 'External gain/loss offset in dB, added to the plot and to every trace readout (cable loss positive, amplifier gain negative)',
});
Object.assign(dict.zh, {
  'amp_unit': '单位',
  'tip_level_unit': '迹线读数（游标、峰值、信道功率、限制线）的幅度单位；差值仍以 dB 表示',
  'tip_gain': '外部增益/线损补偿（dB），作用于图形与所有迹线读数（线损取正、放大器增益取负）',
});

// Export (PNG snapshot, peak list CSV)
Object.assign(dict.en, {
  'export_png': 'PNG',
  'tip_export_png': 'Export the spectrum plot as a PNG with the acquisition settings in the header',
  'tip_export_peaks_csv': 'Export the peak list (frequency, level, delta to strongest) as CSV',
});
Object.assign(dict.zh, {
  'export_png': 'PNG',
  'tip_export_png': '把频谱图导出为 PNG，抬头带采集参数',
  'tip_export_peaks_csv': '把峰值列表导出为 CSV（频率、电平、相对最强峰的差值）',
});

// Channel measurements: channel power / OBW / ACPR
Object.assign(dict.en, {
  'channel': 'Channel', 'chan_center': 'Center', 'chan_bw': 'CH BW', 'chan_obw': 'OBW', 'chan_acp': 'ACP',
  'chan_power': 'Channel power', 'chan_acp_l': 'ACP lower', 'chan_acp_u': 'ACP upper',
  'tip_tab-chan': 'Channel power, occupied bandwidth and adjacent-channel power of the displayed trace',
  'tip_chan_center': 'Channel centre in MHz; leave empty to use marker 1 or the sweep centre',
  'tip_chan_bw': 'Channel bandwidth used for the channel-power and ACPR integrals (MHz)',
  'tip_chan_obw': 'Power percentage used for the occupied-bandwidth band',
  'tip_chan_offset': 'Adjacent channel centre offset from the channel centre (MHz)',
  'tip_chan_adjbw': 'Adjacent channel bandwidth (MHz)',
  'tip_btn-chan-meas': 'Measure channel power, OBW and ACPR from the displayed trace',
  'tip_btn-chan-clear': 'Clear the channel measurement result',
});
Object.assign(dict.zh, {
  'channel': '信道', 'chan_center': '中心', 'chan_bw': '信道带宽', 'chan_obw': 'OBW', 'chan_acp': '邻道',
  'chan_power': '信道功率', 'chan_acp_l': '下邻道', 'chan_acp_u': '上邻道',
  'tip_tab-chan': '对当前显示迹线计算信道功率、占用带宽与邻道功率比',
  'tip_chan_center': '信道中心频率（MHz）；留空则使用游标 1 或扫频中心',
  'tip_chan_bw': '用于信道功率与 ACPR 积分的信道带宽（MHz）',
  'tip_chan_obw': '占用带宽所用的功率百分比',
  'tip_chan_offset': '邻道中心相对信道中心的偏移（MHz）',
  'tip_chan_adjbw': '邻道带宽（MHz）',
  'tip_btn-chan-meas': '基于当前显示迹线测量信道功率、OBW 与 ACPR',
  'tip_btn-chan-clear': '清除信道测量结果',
});

// Limit lines + pass/fail
Object.assign(dict.en, {
  'limits': 'Limits', 'limit_state': 'Limit', 'limit_edit': 'Edit', 'limit_add': 'Add',
  'limit_reset': 'Span', 'limit_csv': 'CSV', 'limit_tol': 'Tol', 'limit_del': '×',
  'limit_pass': 'PASS', 'limit_na': 'no data',
  'limit_fail': 'FAIL {n} bins, worst +{db} dB @ {f} MHz',
  'tip_limit_onoff': 'Toggle the limit line and the pass/fail check',
  'tip_limit_add': 'Insert a limit point in the middle of the widest gap',
  'tip_limit_reset': 'Reset to a flat two-point limit across the current span',
  'tip_limit_csv': 'Export the bins that exceed the limit as CSV',
  'tip_limit_tol': 'Tolerance in dB; a bin counts as exceeding only when it is this much above the limit',
  'tip_limit_freq': 'Limit point frequency in MHz',
  'tip_limit_level': 'Limit point level in dBm',
  'tip_limit_del': 'Remove this limit point (at least two are kept)',
});
Object.assign(dict.zh, {
  'limits': '限制线', 'limit_state': '限制', 'limit_edit': '编辑', 'limit_add': '加点',
  'limit_reset': '全扫宽', 'limit_csv': 'CSV', 'limit_tol': '容差', 'limit_del': '×',
  'limit_pass': '通过', 'limit_na': '无数据',
  'limit_fail': '超限 {n} 点，最差 +{db} dB @ {f} MHz',
  'tip_limit_onoff': '开启 / 关闭限制线与通过判定',
  'tip_limit_add': '在最宽的一段区间中点插入限制点',
  'tip_limit_reset': '重置为覆盖当前扫宽的两点平直限制线',
  'tip_limit_csv': '把超出限制线的频点导出为 CSV',
  'tip_limit_tol': 'dB 容差；超出限制线超过该值才判为超限',
  'tip_limit_freq': '限制点频率（MHz）',
  'tip_limit_level': '限制点电平（dBm）',
  'tip_limit_del': '删除该限制点（至少保留两点）',
});
