import { getLang } from '../core/i18n';

/** Keep action help consistent across the overview and independent channel windows. */
export function workbenchTip(action: string): string {
  const tips: Record<string, [string, string]> = {
    select: ['Open this channel in the workspace; unapplied drafts are kept', '在工作区打开此信道，保留未应用的草稿'],
    recordings: ['Open recording controls and saved files for this channel', '打开此信道的录制控制与文件记录'],
    detail: ['Open a separate settings window fixed to this channel', '打开固定到此信道的独立设置窗口'],
    results: ['View live signal information and decoded results; processing already runs while receiving', '查看实时信号信息和解码结果；接收时已自动进行处理'],
    'follow-results': ['Open a results window that follows the selected channel', '打开随选中信道切换的结果窗口'],
    listen: ['Turn this channel on or off in this browser’s speakers', '开启或关闭此信道在当前浏览器的声音'],
    solo: ['Temporarily hear only Solo channels; turn off to restore the mix', '暂时只听标为独听的信道；关闭后恢复原混听组合'],
    'toggle-receiver': ['Stop reception and finish its recordings, or resume reception', '停止该路接收并结束其录制，或重新开始接收'],
    'tab-receive': ['Set the frequency and optional demodulation or decoding mode', '设置频率，选择是否解调或解码'],
    'tab-recording': ['Save this channel on the server and inspect recording tasks', '将此信道保存到服务器，并查看录制任务'],
    'tab-output': ['Connect external software to live IQ, audio or decoder streams', '将实时 IQ、音频或解码数据连接到外部软件'],
    cancel: ['Discard unapplied settings and restore confirmed values', '撤销未应用的设置，恢复已确认值'],
    retry: ['Retry the rejected settings for this channel', '重新提交此信道刚才失败的设置'],
    'retry-stream': ['Reconnect local audio and results for this channel', '重新连接此信道的本地音频和结果'],
    'master-mute': ['Mute all speakers in this browser; reception and recording continue', '静音当前浏览器的全部声音；接收和录制继续'],
    'resume-audio': ['Allow the browser to play channel audio', '允许浏览器播放信道声音'],
    create: ['Choose a frequency and mode, then create and start receiving in one step', '选择频率和模式，一步创建并开始接收'],
    duplicate: ['Create another channel with the selected channel’s settings', '按当前信道的设置新建另一条信道'],
    retire: ['Delete the selected channel definition and end its reception and recordings', '删除选中信道，并结束该路接收和录制'],
    'dismiss-fault': ['Dismiss this error message', '关闭此错误提示'],
    workbench: ['Open or focus the channel workbench', '打开信道工作台或将其置于前台'],
    recall: ['Bring all channel windows back into this page', '将全部信道窗口收回当前页面'],
    reset: ['Restore the default channel window positions and sizes', '恢复信道窗口的默认位置和大小'],
    'quick-listen': ['Enter a frequency and add it to the listening mix', '输入频率，并将其加入监听'],
    'preset-save': ['Save the selected configuration in this fixture session', '在此模拟会话中保存选中信道的设置'],
    'preset-load': ['Preview the saved configuration for the selected channel', '预览将已保存设置载入选中信道的影响'],
    scene: ['Preview a simulated change to all channels', '预览全部信道的模拟场景变更'],
    'scene-apply': ['Apply the previewed scene to all channels', '应用已预览的全部信道场景'],
    'scene-cancel': ['Dismiss the scene preview', '取消场景预览'],
  };
  const tip = tips[action];
  return tip ? tip[getLang() === 'zh' ? 1 : 0] : '';
}
