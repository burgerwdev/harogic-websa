import { getLang, onLangChange } from '../core/i18n';
import { getUiScale } from '../core/uiScale';
import { createFloatingWindow, type FloatingWindow } from './floatingWindow';
import './workbench.css';
import './sdrHelp.css';

const l = (en: string, zh: string): string => getLang() === 'zh' ? zh : en;
let panel: FloatingWindow | undefined;
let previousFocus: HTMLElement | null = null;

export function toggleSdrHelp(): void {
  if (panel && !panel.root.hidden) { panel.hide(); previousFocus?.focus({ preventScroll: true }); return; }
  previousFocus = document.activeElement as HTMLElement | null;
  if (!panel) {
    const scale = getUiScale();
    panel = createFloatingWindow('sdr-help', () => l('SDR shortcuts & gestures', 'SDR 快捷键与鼠标手势'),
      { x: 100, y: 80, w: Math.min(660 * scale, window.innerWidth - 32), h: Math.min(760 * scale, window.innerHeight - 100) });
    panel.root.classList.add('sdr-help-window');
    const render = () => {
      if (!panel) return;
      const group = (title: string, rows: string[][]) => `<h2>${title}</h2><dl>${rows.map(([key, description]) => `<dt>${key}</dt><dd>${description}</dd>`).join('')}</dl>`;
      panel.body.innerHTML = `<p>${l('Main shortcuts apply in SDR outside text fields and dialogs. Holding A/L/R/I/V/? does not repeat the action.', '主界面快捷键在 SDR 模式下生效；编辑输入框和对话框时不触发。长按 A/L/R/I/V/? 不会重复执行。')}</p>`
        + group(l('Channels', '信道管理'), [
          ['A', l('Save this frequency without listening. Already saved: keep its state.', '加入当前频点，不开启监听；已加入时保持现有状态。')],
          ['L', l('Save and listen; already saved: toggle listening.', '加入并监听；已加入时切换监听。')],
          ['R', l('Save and record audio; already saved: toggle audio recording.', '加入并录制音频；已加入时切换音频录制。')],
          ['I', l('Save and record IQ; already saved: toggle IQ recording.', '加入并录制 IQ；已加入时切换 IQ 录制。')],
          ['V', l('Show/hide the channel overview, including a popped-out overview. Hiding docks and closes the popup.', '显示／隐藏信道概览；已弹出时也有效，隐藏会收回并关闭独立窗口。')],
          ['●L / ●R / ●IQ', l('Label indicators: listening / audio recording / IQ recording. These actions are independent.', '标签状态：监听／音频录制／IQ 录制；三个动作互相独立。')],
        ]) + group(l('Tuning & audio', '调谐与音频'), [
          ['← / →', l('Tune down/up by one step.', '按当前步进向下／向上调谐。')],
          ['Shift + ← / →', l('Coarse tuning step.', '粗步进调谐。')],
          ['Ctrl + ← / →', l('Fine tuning step.', '细步进调谐。')],
          ['↑ / ↓', l('Raise/lower channel audio gain.', '增加／减小信道音频增益。')],
          ['PgUp / PgDn', l('Next larger/smaller filter bandwidth preset.', '切换到更宽／更窄的滤波带宽预设。')],
          ['M', l('Cycle available demodulation modes. Inside channel windows: toggle master mute.', '循环切换可用解调模式；在信道窗口内则切换总静音。')],
          ['H', l('Show/hide passbands and labels on the plots.', '显示／隐藏频谱与瀑布图上的通带和标签。')],
          ['Space', l('Toggle listening audio / master mute.', '开关监听声音／总静音。')],
          ['? / Esc', l('Show/hide this help / close a window or cancel a gesture or edit.', '显示／隐藏本帮助；Esc 关闭浮窗或取消手势、编辑。')],
        ]) + group(l('Mouse & trackpad', '鼠标与触控板'), [
          [l('Plot click', '单击图区'), l('Tune using Free, Channel grid or Signal locate. Click a channel label to tune exactly to its saved frequency.', '按自由、频道网格或信号定位调谐；点击信道标签精确跳转到该频点。')],
          [l('Top frequency handle', '顶部频率滑块'), l('Drag to tune even when display zoom is enabled.', '拖动调谐；开启显示缩放时也有效。')],
          [l('Passband edge', '通带边缘'), l('Drag an edge grip on the spectrum or waterfall to change filter bandwidth.', '拖动频谱或瀑布图的通带边缘抓手调整滤波带宽。')],
          [l('Plot drag', '拖动图区'), l('Zoom on: select a zoom range. Zoom off: tune continuously.', '开启缩放时框选放大范围；关闭缩放时连续调谐。')],
          ['Ctrl + ' + l('click / drag', '单击／拖动'), l('Place/move the active measurement marker; drag an existing marker diamond to move it.', '放置／移动当前测量标记；也可直接拖动已有标记菱形。')],
          [l('Plot wheel', '图区滚轮'), l('Zoom the display when Zoom is on; otherwise scroll the page. Ctrl/Cmd + wheel keeps browser zoom.', '开启缩放时调整显示范围，否则滚动页面；Ctrl/Cmd＋滚轮保留浏览器缩放。')],
          [l('Axis drag / wheel', '坐标轴拖动／滚轮'), l('Frequency axis: pan/change capture span; level axis: shift level/change dB per division.', '频率轴：平移／改变采集跨度；幅度轴：平移电平／调整每格 dB。')],
          [l('Channel card', '信道卡片'), l('Drag to arrange; double-click to open a separate channel window. Drag a window header to move it; drag its edges to resize.', '拖动排列，双击打开信道独立窗口；拖动窗口标题栏移动，拖动边缘调整大小。')],
        ]) + group(l('Focused controls', '控件内操作'), [
          [l('Zoom overview', '缩放导航框'), l('←/→ pan (Shift: smaller step); +/− zoom; Home resets the display view.', '←/→ 平移（Shift 为小步进）；＋/− 缩放；Home 重置显示范围。')],
          ['Alt + ← / → / ↑ / ↓', l('Move the focused overview card.', '移动已聚焦的概览卡片。')],
          ['Enter / Esc', l('Confirm/cancel frequency, filter and name edits. Numeric keypad: digits, decimal point, minus, comma, Backspace, Enter and Esc.', '确认／取消频率、滤波和名称编辑；数字键盘支持数字、小数点、负号、逗号、退格、Enter 和 Esc。')],
        ]);
    };
    panel.root.addEventListener('keydown', event => {
      if ((event.key === '?' || event.key === 'Escape') && !event.ctrlKey && !event.metaKey && !event.altKey) {
        event.preventDefault(); event.stopPropagation();
        if (!event.repeat) { panel?.hide(); previousFocus?.focus({ preventScroll: true }); }
      }
    }, true);
    const stopLanguage = onLangChange(() => { panel?.sync(); render(); });
    window.addEventListener('pagehide', () => { stopLanguage(); panel?.dispose(); panel = undefined; }, { once: true });
    render();
  }
  panel.setCollapsed(false); panel.show();
}
