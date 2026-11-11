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
const page = await browser.newPage({ viewport: { width: 1440, height: 980 } }); const errors = [], commands = [], gets = [];
page.on('pageerror', error => errors.push(error.message));
page.on('request', request => { if (request.method() === 'POST') commands.push({ url: request.url(), body: request.postDataJSON() }); if (new URL(request.url()).pathname === '/api/sdr/scan' && request.method() === 'GET') gets.push(Date.now()); });
const report = {};
try {
  await scan('pause'); await scan('cancel_region'); await scan('configure',{range:null,channels:[],auto_identify:false});
  await page.goto(origin); await page.waitForFunction(() => document.querySelector('#sdr-scan-toggle') && document.querySelector('#spectrum')?.dataset.sdrWindow !== '');
  await page.waitForTimeout(1500); await page.locator('#sdr-scan-toggle').click();
  const root = page.locator('#sdr-scan-window'); await root.waitFor({ state: 'visible' });
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
  await eventually(async () => (await api('/api/sdr/scan')).signals.some(s => s.source === 'region' && s.identify_state === 'complete' && s.activity !== 'stale'), 'One-shot identification did not finish');
  let current = await api('/api/sdr/scan'); assert.equal(current.running, false); assert.equal(current.range, null); report.oneShot = current;
  assert.equal(commands.slice(baselineCommands).some(c => c.url.endsWith('/api/config')), false, 'Selection emitted a tuning command');
  let receivers = await api('/api/sdr/receivers?detail=live'); assert.equal(receivers.receivers.find(r => r.receiver_uid === primary.receiver_uid).stream.epoch, primary.stream.epoch);
  // A second selection replaces the temporary group and leaves reception alone.
  await page.locator('#waterfall').click({ button: 'right', position: { x: 160, y: 50 } });
  assert.equal(await page.locator('.sdr-identify-menu button').count(), 2);
  assert.match(await page.locator('.sdr-identify-menu').innerText(), /current|当前/);
  await page.keyboard.press('Escape'); assert.equal(await page.locator('.sdr-identify-menu').count(), 0);
  await root.locator('[data-scan-select]').click(); await page.keyboard.press('Enter');
  await eventually(async () => { const s = await api('/api/sdr/scan'); return s.signals.some(r => r.source === 'region' && r.identify_state === 'complete' && r.activity !== 'stale'); }, 'Keyboard selection did not finish');
  await page.waitForTimeout(600);
  await root.locator('.sdr-scan-footer select').selectOption('iq');
  const add = root.locator('.sdr-scan-footer button').last(); await add.click();
  receivers = await eventually(async () => { const x = await api('/api/sdr/receivers?detail=live'); return x.receivers.some(r => !initial.has(r.receiver_uid)) ? x : false; }, 'Add channel did not create a receiver');
  const afterAdd = receivers.receivers.length; await page.waitForTimeout(800);
  await root.locator('.sdr-scan-footer button').last().click(); await page.waitForTimeout(500);
  assert.equal((await api('/api/sdr/receivers?detail=live')).receivers.length, afterAdd, 'Open existing channel created a duplicate');
  report.reuse = true;
  await root.locator('.sdr-scan-footer select').selectOption('am');
  await root.locator('.sdr-scan-footer button').nth(1).click();
  await eventually(async () => (await api('/api/sdr/receivers?detail=live')).receivers.some(r => !initial.has(r.receiver_uid) && r.confirmed.purpose === 'listen'), 'Listen did not create the explicit audio receiver');
  report.listen = true;
  await root.locator('.sdr-scan-evidence button').click(); await page.waitForTimeout(400);
  await root.locator('.sdr-scan-evidence button').click();
  // Hide only changes visibility and must stop the large snapshot polling.
  await root.locator('[data-scan-run]').click(); await page.waitForTimeout(1100); await root.locator('[data-close]').click();
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
    await page.screenshot({path:`${artifacts}/扫描-窄窗-${scale}.png`});
    const footer = page.locator('#sdr-scan-window .sdr-scan-footer'); assert.equal(await footer.isVisible(),true); const box = await footer.boundingBox(); assert(box.x >= 0 && box.x + box.width <= 801 && box.y + box.height <= 781,JSON.stringify(box));
  }
  report.finalPrimary = (await api('/api/sdr/receivers?detail=live')).receivers.find(r => r.receiver_uid === primary.receiver_uid);
  assert.equal(report.finalPrimary.reception?.interruptions ?? 0,primary.reception?.interruptions ?? 0,'Scan interrupted formal audio processing');
  report.browserErrors = errors; report.hiddenPolling = true; assert.deepEqual(errors, []);
} finally {
  report.lastSnapshot = await api('/api/sdr/scan'); report.commands = commands.filter(c => c.url.endsWith('/api/sdr/scan') || c.url.endsWith('/api/config'));
  await browser.close();
  const snapshot = await api('/api/sdr/receivers?detail=live');
  for (const receiver of snapshot.receivers.filter(r => !initial.has(r.receiver_uid))) {
    const result = await api('/api/sdr/receivers', {type:'retire',service_id:snapshot.service_id,request_id:crypto.randomUUID(),receiver_uid:receiver.receiver_uid}); assert.equal(result.fault,null);
  }
  await scan('pause'); await scan('cancel_region');
  await fs.writeFile(`${artifacts}/browser-report.json`,JSON.stringify(report,null,2));
}
console.log('Signal scan live browser gate passed');
