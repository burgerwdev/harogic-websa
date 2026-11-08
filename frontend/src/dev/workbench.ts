import '../style.css';
import '../ui/workbench.css';
import '../ui/decodeResults.css';
import { setLang } from '../core/i18n';
import { setTheme } from '../core/theme';
import { setUiScale } from '../core/uiScale';
import { FixtureReceiverAdapter } from './fixtureReceiverAdapter';
import { WorkbenchSession } from '../ui/workbenchSession';
import { mountWorkbench } from '../ui/workbenchApp';

// This standalone HTML is served by Vite dev and excluded from the production build.
const adapter = new FixtureReceiverAdapter();
const session = new WorkbenchSession(adapter);
setLang('zh'); setTheme('dark'); setUiScale(1);
const controls = document.querySelector<HTMLElement>('#fixture-controls')!;
controls.innerHTML = `<b>DEV / FIXTURE</b><label>Language <select data-language><option value="zh">中文</option><option value="en">English</option></select></label>
<label>Theme <select data-theme><option value="dark">Dark</option><option value="light">Light</option></select></label>
<label>UI <select data-scale><option>1</option><option>0.8</option><option>1.25</option><option>1.5</option></select></label>
<label>Inject → selected <select data-fault><option value="reject">Request rejection / 拒绝</option><option value="late">Delayed reply / 迟到</option><option value="gap">Stream gap / 断流</option><option value="recover">New stream / 恢复</option><option value="disk">Disk full / 磁盘满</option><option value="consumer">Slow consumer / 慢订阅</option><option value="capture">Capture failure / 采集失败</option></select></label><button data-inject>Inject</button><output data-injected></output>`;
const app = mountWorkbench(session, document.querySelector<HTMLElement>('#workbench-app')!);
controls.querySelector<HTMLSelectElement>('[data-language]')!.addEventListener('change', event => setLang((event.target as HTMLSelectElement).value as 'zh' | 'en'));
controls.querySelector<HTMLSelectElement>('[data-theme]')!.addEventListener('change', event => setTheme((event.target as HTMLSelectElement).value as 'dark' | 'light'));
controls.querySelector<HTMLSelectElement>('[data-scale]')!.addEventListener('change', event => setUiScale(Number((event.target as HTMLSelectElement).value)));
controls.querySelector('[data-inject]')!.addEventListener('click', () => {
  const kind = controls.querySelector<HTMLSelectElement>('[data-fault]')!.value as Parameters<typeof adapter.inject>[0];
  adapter.inject(kind, session.selected); controls.querySelector('output')!.textContent = `${kind} → ${session.selected}`;
});
window.addEventListener('pagehide', () => { app.dispose(); session.dispose(); adapter.dispose(); });
