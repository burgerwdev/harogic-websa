#!/usr/bin/env node
// Live-service browser gate. Artifacts belong outside the checkout. Explicit
// environment opt-in prevents an accidental hardware test in ordinary CI.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
if (!process.env.WEBSA_SCAN_LIVE) throw new Error('Set WEBSA_SCAN_LIVE=1 for the live SDR browser gate');
const { chromium } = await import(process.env.WEBSA_PLAYWRIGHT ?? 'playwright');
const origin = process.env.WEBSA_ORIGIN ?? 'http://127.0.0.1:8080';
const artifacts = process.env.WEBSA_ARTIFACTS;
assert(artifacts, 'WEBSA_ARTIFACTS must point outside the checkout'); await fs.mkdir(artifacts, { recursive: true });
const api = async (path, body) => { const r = await fetch(origin + path, body ? { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) } : {}); const v = await r.json(); assert(r.ok, JSON.stringify(v)); return v; };
const scan = async (type, fields = {}) => { const s = await api('/api/sdr/scan'); return api('/api/sdr/scan', { type, service_id: s.service_id, scan_id: s.scan_id, expected_revision: s.revision, capture_epoch: s.capture.epoch, request_id: crypto.randomUUID(), ...fields }); };
const eventually = async (condition, message, timeout = 12000) => { const deadline = Date.now() + timeout; do { const v = await condition(); if (v) return v; await new Promise(r => setTimeout(r, 150)); } while (Date.now() < deadline); throw new Error(message); };
const before = await api('/api/sdr/receivers?detail=live'); assert.equal(before.capture.state, 'running');
const initial = new Set(before.receivers.map(r => r.receiver_uid)); const primary = before.receivers.find(r => r.display_number === 1);
const browser = await chromium.launch({ executablePath: process.env.WEBSA_CHROMIUM ?? '/usr/bin/chromium', headless: true, args: ['--no-sandbox'] });
const page = await browser.newPage({ viewport: { width: 1440, height: 980 } }); const errors = [], commands = [], gets = [], notifications = [];
let delayRegionReply = false, delayedSignal = '', finishDelayedReply, failDelayedReply, blockedCandidate = null, emptyRegionFixture = false;
const delayedReply = new Promise((resolve, reject) => { finishDelayedReply = resolve; failDelayedReply = reject; });
await page.route('**/api/sdr/scan', async route => {
  const request = route.request();
  if (request.method() === 'GET' && (blockedCandidate || emptyRegionFixture)) {
    const response = await route.fetch(), value = await response.json(); value.signals = emptyRegionFixture ? [] : [blockedCandidate];
    if (emptyRegionFixture) value.reason = 'no_signal_detected';
    return route.fulfill({response,body:JSON.stringify(value),contentType:'application/json'});
  }
  if (!delayRegionReply || request.method() !== 'POST' || request.postDataJSON().type !== 'identify_region') return route.continue();
  delayRegionReply = false;
  try {
    const response = await route.fetch();
    const state = await eventually(async () => { const s = await api('/api/sdr/scan'); const r = s.signals.filter(r => r.source === 'region'); return r.length && r.every(r => r.identify_state === 'complete') ? s : false; }, 'Delayed command did not complete');
    delayedSignal = state.signals.find(r => r.source === 'region').id;
    await page.waitForTimeout(150);
    await route.fulfill({ response }); finishDelayedReply();
  } catch (error) { failDelayedReply(error); await route.abort().catch(() => {}); }
});
page.on('response', response => { if (response.url().endsWith('/api/sdr/scan/events')) notifications.push(response.status()); });
page.on('pageerror', error => errors.push(error.message));
page.on('request', request => { if (request.method() === 'POST') commands.push({ url: request.url(), body: request.postDataJSON() }); if (new URL(request.url()).pathname === '/api/sdr/scan' && request.method() === 'GET') gets.push(Date.now()); });
page.setDefaultTimeout(10000);
const benchTag='scan-e2e:'+crypto.randomUUID();
await page.route('**/api/sdr/receivers',async route=>{const request=route.request();if(request.method()!=='POST')return route.continue();const data=request.postDataJSON();if(data.type==='create'||data.type==='configure'&&data.config?.tags?.some(t=>t===benchTag||t.startsWith('scan-audition:'))){data.config.tags=[...new Set([...data.config.tags,benchTag])];return route.continue({postData:JSON.stringify(data)});}return route.continue();});
const report = {benchTag}, recordings=[];
const receiverCommand=async(type,fields={})=>{const state=await api('/api/sdr/receivers?detail=live');const result=await api('/api/sdr/receivers',{type,service_id:state.service_id,request_id:crypto.randomUUID(),...fields});assert.equal(result.fault,null,JSON.stringify(result.fault));return result.snapshot;};
try {
  for(const kind of ['iq','audio']){const known=new Set((await api('/api/sdr/receivers?detail=live')).recordings.map(r=>r.task_id));const state=await receiverCommand('record_start',{receiver_uid:primary.receiver_uid,kinds:[kind]});const task=state.recordings.find(r=>!known.has(r.task_id)&&r.receiver_uid===primary.receiver_uid);assert(task,'recording task absent');recordings.push({kind,id:task.task_id});}
  await scan('pause'); await scan('cancel_region'); await scan('configure',{range:null,channels:[],auto_identify:false});
  await page.goto(origin); await page.waitForFunction(() => document.querySelector('#sdr-scan-toggle') && document.querySelector('#spectrum')?.dataset.sdrWindow !== '');
  await page.waitForTimeout(1500); await page.locator('#sdr-scan-toggle').click();
  const root = page.locator('#sdr-scan-window'); await root.waitFor({ state: 'visible' });
  await eventually(async () => notifications.includes(200), 'Scan change notification connection did not open');
  await page.waitForFunction(() => document.querySelector('[data-scan-run]')?.disabled === false);
  assert.equal((await api('/api/sdr/scan')).running, false, 'Opening the window must not start scanning');
  const geometryBefore = await root.boundingBox();
  await root.locator('[data-scan-select]').focus(); await root.locator('[data-scan-select]').click();
  assert.equal(await root.locator('.mc-body').isVisible(), false);
  await page.keyboard.press('Escape'); assert.equal(await root.locator('.mc-body').isVisible(), true);
  assert.equal(await root.locator('[data-scan-select]').evaluate(e => e === document.activeElement), true);
  const geometryAfter = await root.boundingBox(); assert(Math.abs(geometryBefore.height - geometryAfter.height) < 2);
  const baselineCommands = commands.length;
  const c = await page.locator('#spectrum').boundingBox();
  await root.locator('[data-scan-select]').click();
  await page.mouse.move(c.x + c.width * .35, c.y + c.height * .5); await page.mouse.down(); await page.mouse.move(c.x + c.width * .37, c.y + c.height * .8, { steps: 10 });
  await page.screenshot({ path: `${artifacts}/选区-双图同步.png` }); await page.mouse.up();
  await eventually(async () => { const s=await api('/api/sdr/scan'); return s.reason==='no_signal_detected'||s.signals.some(r => r.source === 'region' && r.identify_state === 'complete' && r.activity !== 'stale'); }, 'One-shot identification did not finish');
  let current = await api('/api/sdr/scan'); assert.equal(current.running, false); assert.equal(current.range, null); report.oneShot = current;
  // Return the old command receipt after the completed-result notification.
  // It must not leave a paused one-shot view stuck on the old placeholder.
  delayRegionReply = true;
  await page.evaluate(({ hz, epoch }) => document.dispatchEvent(new CustomEvent('websa:identify-region', { detail: { low_hz: hz - 2000, high_hz: hz + 2000, origin: 'manual', capture_epoch: epoch } })), { hz: before.capture.center_hz + 900000, epoch: before.capture.epoch });
  await delayedReply;
  await root.locator(`[data-signal="${delayedSignal}"][data-identify-state="complete"]`).waitFor({state:'visible'});
  report.lateCommandReceipt = true;
  // A genuine over-wide candidate must not offer an invalid configuration,
  // including after choosing an explicit audio mode. An empty region is a
  // completed discovery with no signal, not a cancelled/failed classification.
  current=await api('/api/sdr/scan');
  blockedCandidate = {...current.signals.find(r=>r.source==='region'),id:'wide-analysis-fixture',activity:'active',identify_state:'unavailable',
    label:'unknown',suggested_mode:null,flags:['analysis_passband_limit'],bandwidth_hz:751495.0};
  await page.evaluate(()=>document.dispatchEvent(new Event('visibilitychange')));
  const wideRow=root.locator('[data-signal="wide-analysis-fixture"]');await wideRow.waitFor({state:'visible'});
  assert.match(await wideRow.innerText(),/analysis passband|单路分析通带/);
  await wideRow.click();await root.locator('.sdr-scan-footer select').selectOption('am');
  assert.equal(await wideRow.locator('[data-scan-add]').isDisabled(),true);
  assert.equal(await wideRow.locator('[data-scan-listen]').isDisabled(),true);
  blockedCandidate=null;emptyRegionFixture=true;await page.evaluate(()=>document.dispatchEvent(new Event('visibilitychange')));
  await wideRow.waitFor({state:'detached'});report.analysisPassbandLimit=true;
  await eventually(async()=>/No signal detected|选区内未发现信号/.test(await root.locator('.sdr-scan-empty').innerText()),'Empty region did not show a no-signal result');
  assert.equal(await root.locator('[data-scan-add]').count(),0);report.noSignalRegionUI=true;
  emptyRegionFixture=false;await page.evaluate(()=>document.dispatchEvent(new Event('visibilitychange')));
  await root.locator(`[data-signal="${delayedSignal}"]`).waitFor({state:'visible'});
  assert.equal(commands.slice(baselineCommands).some(c => c.url.endsWith('/api/config')), false, 'Selection emitted a tuning command');
  let receivers = await api('/api/sdr/receivers?detail=live'); assert.equal(receivers.receivers.find(r => r.receiver_uid === primary.receiver_uid).stream.epoch, primary.stream.epoch);
  // A second selection replaces the temporary group and leaves reception alone.
  await page.locator('#waterfall').click({ button: 'right', position: { x: 160, y: 50 } });
  assert.equal(await page.locator('.sdr-identify-menu button').count(), 2);
  assert.match(await page.locator('.sdr-identify-menu').innerText(), /current|当前/);
  await page.keyboard.press('Escape'); assert.equal(await page.locator('.sdr-identify-menu').count(), 0);
  await root.locator('[data-scan-select]').click(); await page.keyboard.press('Enter');
  await eventually(async () => { const s = await api('/api/sdr/scan'); return s.signals.some(r => r.source === 'region' && r.identify_state === 'complete' && r.activity !== 'stale'); }, 'Keyboard selection did not finish');
  // Keep Current and the preview at distinct frequencies for the mixer assertions.
  const previewHz = before.capture.center_hz + (primary.confirmed.baseband.frequency_hz < before.capture.center_hz ? 1 : -1) * Math.min(100000, before.capture.usable_bandwidth_hz / 4);
  await scan('identify_region', { low_hz: previewHz - 3000, high_hz: previewHz + 3000, origin: 'manual' });
  const separate = await eventually(async () => (await api('/api/sdr/scan')).signals.find(r => r.source === 'region' && r.identify_state === 'complete' && Math.abs(r.center_hz - primary.confirmed.baseband.frequency_hz) > 12000), 'Separate preview selection did not finish');
  await root.locator(`[data-signal="${separate.id}"]`).click();
  await page.waitForTimeout(600);
  const selectedId = await root.locator('tr[aria-selected="true"]').getAttribute('data-signal');
  const row = root.locator(`[data-signal="${selectedId}"]`);
  await root.locator('.sdr-scan-footer select').selectOption('iq');
  assert.equal(await row.locator('[data-scan-listen]').isDisabled(),true);
  await row.locator('[data-scan-add]').click();
  receivers = await eventually(async () => { const x = await api('/api/sdr/receivers?detail=live'); return x.receivers.some(r => !initial.has(r.receiver_uid)) ? x : false; }, 'Add IQ channel did not create a receiver');
  const afterAdd = receivers.receivers.length; await page.waitForTimeout(500);
  assert.match(await row.locator('[data-scan-add]').innerText(),/C[0-9]+/);
  await root.locator('.sdr-scan-footer select').selectOption('am');
  const currentAudio = await page.locator('#spectrum').getAttribute('data-sdr-audio') ?? '';
  await row.locator('[data-scan-listen]').click();
  const preview = await eventually(async () => (await api('/api/sdr/receivers?detail=live')).receivers.find(r => r.confirmed.tags.some(t=>t.startsWith('scan-audition:'))), 'Preview did not create an owned receiver');
  await root.locator('[data-scan-stop]').waitFor({state:'visible'});
  assert.equal(await root.locator('[data-scan-exclusive]').isChecked(),true);
  await eventually(async()=> /enabled=false/.test(await page.locator('#spectrum').getAttribute('data-sdr-audio')??''),'Exclusive preview did not silence Current');
  await root.locator('[data-scan-exclusive]').uncheck();
  if(/enabled=true/.test(currentAudio)) await eventually(async()=> /enabled=true/.test(await page.locator('#spectrum').getAttribute('data-sdr-audio')??''),'Mix was not restored');
  await root.locator('[data-scan-exclusive]').check();
  await root.locator('.sdr-scan-footer select').selectOption('usb');
  await eventually(async()=> (await api('/api/sdr/receivers?detail=live')).receivers.find(r=>r.receiver_uid===preview.receiver_uid&&r.confirmed.processors[0]?.mode==='usb'),'Preview mode did not switch in place');
  await eventually(async()=>!(await root.locator('.sdr-scan-footer select').isDisabled()),'Preview mode edit still pending');
  await root.locator('.sdr-scan-footer select').selectOption('am');
  const previewAgain=await eventually(async()=> (await api('/api/sdr/receivers?detail=live')).receivers.find(r=>r.receiver_uid===preview.receiver_uid&&r.confirmed.processors[0]?.mode==='am'),'Preview AM did not resume');
  await eventually(async()=>!(await root.locator('.sdr-scan-footer select').isDisabled()),'Preview still pending');report.previewModeSwitch=true;
  const previewStream=previewAgain.stream;
  await row.locator('[data-scan-add]').click();
  await eventually(async()=>{const r=(await api('/api/sdr/receivers?detail=live')).receivers.find(r=>r.receiver_uid===preview.receiver_uid);return r&&!r.confirmed.tags.some(t=>t.startsWith('scan-audition:'))&&r;},'Preview was not promoted');
  receivers=await api('/api/sdr/receivers?detail=live');assert.equal(receivers.receivers.length,afterAdd+1);assert.deepEqual(receivers.receivers.find(r=>r.receiver_uid===preview.receiver_uid).stream,previewStream);
  assert.equal(await root.locator('[data-scan-stop]').count(),0);
  await row.locator('[data-scan-listen]').click();await root.locator('[data-scan-stop]').waitFor({state:'visible'});
  assert.equal((await api('/api/sdr/receivers?detail=live')).receivers.length,afterAdd+1,'Borrowing a saved channel duplicated it');
  await root.locator('[data-scan-stop]').click();await page.waitForTimeout(400);
  assert((await api('/api/sdr/receivers?detail=live')).receivers.some(r=>r.receiver_uid===preview.receiver_uid),'Stopping preview retired a saved receiver');
  report.reuse=true;report.listen=true;report.auditionMixing=true;report.previewPromotion=true;
  await root.locator('[data-scan-evidence]').click(); await page.waitForTimeout(300);await root.locator('[data-scan-evidence]').click();
  await root.locator('[data-scan-search]').fill('no-such-frequency');assert.equal(await root.locator('tr[data-signal]').count(),0);
  await root.locator('[data-clear-filters]').click();assert((await root.locator('tr[data-signal]').count())>0);
  await root.locator('[data-sort="power"]').click();assert.equal(await root.locator('th[aria-sort="descending"]').count(),1);
  report.searchAndSort=true;
  await root.locator('[data-scan-clear]').click();await eventually(async()=>!(await api('/api/sdr/scan')).signals.length,'Clearing did not remove results');
  assert.equal((await api('/api/sdr/receivers?detail=live')).receivers.length,afterAdd+1,'Clear retired saved channels');report.clear=true;
  await root.locator('[data-measurement]').selectOption('table');
  const hz=primary.confirmed.baseband.frequency_hz;
  await root.locator('[data-table=start]').fill(String((hz-50000)/1e6));await root.locator('[data-table=stop]').fill(String((hz+50000)/1e6));await root.locator('[data-table=step]').fill('50');await root.locator('[data-table=width]').fill('12');
  await root.locator('fieldset button').first().click();
  await eventually(async()=> (await api('/api/sdr/scan')).signals.filter(s=>s.source==='channel').length===3,'Fixed frequency measurement did not produce three windows');
  assert.equal((await api('/api/sdr/receivers?detail=live')).receivers.length,afterAdd+1);report.fixedFrequencies=true;
  await root.locator('[data-measurement]').selectOption('discovery');await page.waitForTimeout(500);
  // Hide only changes visibility and must stop the large snapshot polling.
  if(!(await api('/api/sdr/scan')).running) await root.locator('[data-scan-run]').click(); await page.waitForTimeout(1100); await root.locator('[data-close]').click();
  const hiddenPolls = gets.length; await page.waitForTimeout(1300); assert(gets.length - hiddenPolls <= 1); assert.equal((await api('/api/sdr/scan')).running, true);
  await page.locator('#sdr-scan-toggle').click(); await root.waitFor({ state: 'visible' });
  const popupPromise = page.waitForEvent('popup'); await root.locator('[data-pop]').click(); const popup = await popupPromise;
  await popup.locator('#sdr-scan-window').waitFor({state:'visible'}); assert.equal(await page.locator('#sdr-scan-window').count(),0);
  await popup.locator('[data-dock]').click(); await page.locator('#sdr-scan-window').waitFor({state:'visible'}); report.popout = true;
  for (const theme of ['dark', 'light']) for (const lang of ['en', 'zh']) {
    await page.evaluate(({theme,lang}) => { localStorage.setItem('web-sa-theme',theme);localStorage.setItem('web-sa-lang',lang); }, {theme,lang});
    await page.reload(); await page.waitForTimeout(1200); await page.locator('#sdr-scan-toggle').click(); await page.waitForTimeout(700);
    await page.screenshot({ path: `${artifacts}/扫描-${theme}-${lang}.png` });
  }
  for (const scale of [1.25, 1.5]) {
    await page.setViewportSize({width:800,height:780});
    await page.evaluate(scale => localStorage.setItem('web-sa-ui-scale',String(scale)), scale);
    await page.reload(); await page.waitForTimeout(1200); await page.locator('#sdr-scan-toggle').click(); await page.waitForTimeout(500);
    assert.equal(await page.locator('html').getAttribute('data-ui-scale'),String(scale));
    await page.locator('#sdr-scan-window tr[data-signal]').first().click();
    await page.screenshot({path:`${artifacts}/扫描-窄窗-${scale}.png`});
    const footer = page.locator('#sdr-scan-window .sdr-scan-footer'); assert.equal(await footer.isVisible(),true); const box = await footer.boundingBox(); assert(box.x >= 0 && box.x + box.width <= 801 && box.y + box.height <= 781,JSON.stringify(box));
  }
  report.finalPrimary = (await api('/api/sdr/receivers?detail=live')).receivers.find(r => r.receiver_uid === primary.receiver_uid);
  assert.equal(report.finalPrimary.reception?.interruptions ?? 0,primary.reception?.interruptions ?? 0,'Scan interrupted formal audio processing');
  assert.equal(await page.locator('#sdr-scan-toggle svg').count(),1);
  report.browserErrors = errors; report.hiddenPolling = true; report.notificationResponses = notifications; assert(notifications.every(status => status === 200)); assert.deepEqual(errors, []);
} catch (error) {
  await page.screenshot({path:`${artifacts}/失败现场.png`});
  report.failure=String(error); report.actionGeometry=await page.locator('.sdr-scan-row-actions').first().evaluate(e=>({cell:e.getBoundingClientRect().toJSON(),style:getComputedStyle(e).cssText,buttons:[...e.querySelectorAll('button')].map(b=>({box:b.getBoundingClientRect().toJSON(),padding:getComputedStyle(b).padding,display:getComputedStyle(b).display,position:getComputedStyle(b).position}))})).catch(()=>null);
  throw error;
} finally {
  report.lastSnapshot = await api('/api/sdr/scan'); report.commands = commands.filter(c => c.url.endsWith('/api/sdr/scan') || c.url.endsWith('/api/config'));
  await browser.close();
  const continuityErrors=[];
  for(const {kind,id}of recordings){
    try{
      await receiverCommand('record_stop',{task_id:id});const task=await eventually(async()=> (await api('/api/sdr/receivers?detail=live')).recordings.find(r=>r.task_id===id&&['completed','failed'].includes(r.state)),'recording did not finish');
      assert.equal(task.state,'completed');const segments=task.tracks[0].segments;assert(segments.length);
      for(let i=1;i<segments.length;i++){assert.deepEqual(segments[i].stream,segments[i-1].stream);if(kind==='audio')assert.equal(segments[i].first_audio_index,segments[i-1].next_audio_index);}
      let index=null;if(kind==='iq'){index=await api('/api/sdr/recordings/'+id+'/index');for(let i=1;i<index.entries.length;i++){const a=index.entries[i-1].meta.clock,b=index.entries[i].meta.clock;assert.equal(BigInt(b.first_output_index),BigInt(a.first_output_index)+BigInt(a.samples));}}
      await fs.writeFile(`${artifacts}/试听-${kind}-连续性.json`,JSON.stringify({task,index},null,2));report[kind+'Continuity']=true;
    }catch(error){continuityErrors.push(String(error));}finally{try{await receiverCommand('record_delete',{task_id:id});}catch(error){continuityErrors.push(String(error));}}
  }
  const snapshot = await api('/api/sdr/receivers?detail=live');
  for (const receiver of snapshot.receivers.filter(r => r.confirmed.tags.includes(benchTag))) {
    const result = await api('/api/sdr/receivers', {type:'retire',service_id:snapshot.service_id,request_id:crypto.randomUUID(),receiver_uid:receiver.receiver_uid}); assert.equal(result.fault,null);
  }
  await scan('pause'); await scan('cancel_region');
  report.recordingErrors=continuityErrors;await fs.writeFile(`${artifacts}/browser-report.json`,JSON.stringify(report,null,2));assert.deepEqual(continuityErrors,[]);
}
console.log('Signal scan live browser gate passed');
