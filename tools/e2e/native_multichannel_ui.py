#!/usr/bin/env python3
"""Real browser control retention and receiver Decode UX on synthetic/replay sources."""
from __future__ import annotations

import argparse
import base64
import copy
import csv
import json
import os
from pathlib import Path

from native_multichannel import api
from native_multichannel_processing import assert_receivers_continue, setup
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from viewport_baseline import wait_async_condition, wait_ui_ready


def watch_controls(surface):
    surface.evaluate('''root => {
      window.uxWatch?.observer.disconnect();
      const refs = [...root.querySelectorAll('button,input,select,form,.mc-content,.mc-list')];
      const removed = [];
      const observer = new MutationObserver(records => {
        for (const record of records) for (const node of record.removedNodes)
          for (const ref of refs) if (node === ref || node.contains?.(ref)) removed.push(ref.outerHTML.slice(0,180));
      });
      observer.observe(root,{subtree:true,childList:true});
      window.uxWatch = {refs,removed,observer};
    }''')


def assert_controls(surface):
    report = surface.evaluate('''() => ({removed:window.uxWatch.removed,
      disconnected:window.uxWatch.refs.filter(ref=>!ref.isConnected).map(ref=>ref.outerHTML.slice(0,180))})''')
    assert not report['removed'] and not report['disconnected'], report
    return report


def browser_gate(origin, snapshot, artifacts):
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        context = browser.new_context(viewport={'width': 1600, 'height': 1100})
        page = context.new_page(); errors = []
        page.on('pageerror', lambda error: errors.append(str(error)))
        page.goto(origin); wait_ui_ready(page, 'sdr')
        page.locator('[data-action=sdr-workbench]').click()
        overview = page.locator('[data-view-id=overview-follow]')
        expect(overview).to_be_visible()
        uid, second, third = [row['receiver_uid'] for row in snapshot['receivers']]
        first_row = overview.locator(f'[data-uid="{uid}"]')
        before = api(origin)
        assert overview.locator('[name=purpose]').count() == 0
        assert overview.locator('[name=mode]').count() == 1
        assert overview.locator('[data-action=tab-processing],[data-action=tab-results]').count() == 0
        assert first_row.locator('[data-action=listen]').count() == 1
        monitor = overview.locator('.mc-monitor [data-action=listen]')
        expect(monitor).to_have_count(1)
        expect(monitor).to_have_attribute('aria-pressed', first_row.locator('[data-action=listen]').get_attribute('aria-pressed'))
        missing_tips = overview.locator('button:visible').evaluate_all('buttons=>buttons.filter(b=>!b.title.trim()).map(b=>b.outerHTML)')
        assert not missing_tips, missing_tips
        # AM uses actual native signal/audio telemetry, with no message-search controls.
        first_row.locator('.mc-row-more > summary').click()
        first_row.locator('[data-action=results]').click()
        signal = page.locator(f'[data-view-id="results-{uid}"]')
        expect(signal.locator('[data-audio-scope]')).to_be_visible()
        expect(signal.locator('[data-decode-search]')).not_to_be_visible()
        expect(signal.locator('[data-decode-action=clear]')).not_to_be_visible()
        # The centre grid line alone is painted even for silence. Require waveform
        # pixels outside it, so an empty or flat scope cannot satisfy this gate.
        page.wait_for_function('''uid => {
          const c = document.querySelector(`[data-view-id="results-${uid}"] [data-audio-scope]`);
          if (!c) return false;
          const pixels = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
          return pixels.some((value, index) => index % 4 === 3 && value > 0
            && Math.abs(Math.floor(index / 4 / c.width) - c.height / 2) > 2);
        }''', arg=uid)
        signal.screenshot(path=str(artifacts / 'am-signal-info.png'))
        signal.locator('[data-close]').click()
        # The selected-channel monitor and its list row control the same route.
        # Establish a deliberate stopped route before testing the held mouse click;
        # native startup may already have selected the main listening channel.
        if monitor.get_attribute('aria-pressed') == 'true':
            monitor.click()
        expect(monitor).to_have_attribute('aria-pressed', 'false')
        expect(first_row.locator('[data-action=listen]')).to_have_attribute('aria-pressed', 'false')
        watch_controls(overview)
        # Native mouse press crosses several status/decoder updates.
        control = first_row.locator('[data-action=listen]')
        control.scroll_into_view_if_needed(); box = control.bounding_box()
        page.mouse.move(box['x'] + box['width']/2, box['y'] + box['height']/2)
        page.mouse.down(); page.wait_for_timeout(1500); page.mouse.up()
        expect(control).to_have_attribute('aria-pressed', 'true')
        expect(monitor).to_have_attribute('aria-pressed', 'true')
        held_mouse = assert_controls(overview)
        # Space activates on keyup; no pointer guard protects this button.
        output_tab = overview.locator('[data-action=tab-output]')
        output_tab.focus(); output_tab.press('Space', delay=1300)
        expect(output_tab).to_have_attribute('aria-pressed', 'true')
        overview.locator('[data-section=iq-preview] summary').click()
        overview.locator('[data-action=output]').click()
        expect(overview).to_contain_text('active')
        watch_controls(overview)
        overview.locator('.mc-content').evaluate('element => { element.scrollTop=80; window.uxScroll=element.scrollTop; }')
        page.wait_for_timeout(1400)
        output_controls = assert_controls(overview)
        assert overview.locator('.mc-content').evaluate('element=>element.scrollTop===window.uxScroll')
        overview.locator('[data-action=tab-receive]').click()
        overview.locator('[data-frequency-unit=Hz]').click()
        frequency = overview.locator('[name=frequency]')
        frequency.fill('100001234'); frequency.press('ArrowLeft'); frequency.press('Shift+ArrowLeft')
        draft = frequency.input_value(); watch_controls(overview); page.wait_for_timeout(1200)
        expect(frequency).to_be_focused(); expect(frequency).to_have_value(draft)
        draft_controls = assert_controls(overview)
        # Search retains the native editor/selection, including incoming decode data.
        search = overview.locator('[data-search]'); search.fill('AM native')
        search.press('Home'); search.press('Shift+ArrowRight')
        search.evaluate('element=>{window.uxSearch=element;window.uxSelection=[element.selectionStart,element.selectionEnd];}')
        watch_controls(overview); page.wait_for_timeout(1200)
        assert search.evaluate('element=>element===window.uxSearch && document.activeElement===element && element.selectionStart===window.uxSelection[0] && element.selectionEnd===window.uxSelection[1]')
        search_controls = assert_controls(overview)
        search.fill(''); overview.locator('[data-action=cancel]').click()
        assert_receivers_continue(before['receivers'], api(origin)['receivers'])
        # Actual replay decodes use one familiar message table, not one card per event.
        overview.locator(f'[data-uid="{second}"] .mc-row-more > summary').click()
        overview.locator(f'[data-uid="{second}"] [data-action=results]').click()
        results = page.locator(f'[data-view-id="results-{second}"]')
        expect(results.locator('.mc-message-table tbody tr')).not_to_have_count(0, timeout=35000)
        expect(results.locator('.mc-message-table')).to_have_count(1)
        results.locator('[data-decode-search]').fill('CQ')
        expect(results.locator('.mc-message-table tbody')).to_contain_text('CQ')
        results.locator('[data-sort=snr]').click()
        utc = results.locator('[data-decode-action=utc]'); utc.focus()
        watch_controls(results); utc.press('Space', delay=1300)
        expect(utc).to_have_attribute('aria-pressed', 'true')
        expect(results.locator('.mc-message-table thead')).to_contain_text('UTC')
        decode_controls = assert_controls(results)
        # On this same FT8 receiver, speakers, backend IQ recording and result exports coexist.
        result_controls = results.locator('.mc-decode-binding')
        listen = result_controls.locator('[data-action=listen]')
        if listen.get_attribute('aria-pressed') != 'true':
            listen.click()
        expect(listen).to_have_attribute('aria-pressed', 'true')
        expect(overview.locator(f'[data-uid="{second}"] [data-action=listen]')).to_have_attribute('aria-pressed', 'true')
        result_controls.locator('[data-action=rec-iq]').click()
        wait_async_condition(page, 'uid=>fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.recordings.some(t=>t.receiver_uid===uid&&t.state==="recording"))', arg=second)
        page.wait_for_function('uid=>window.websaAudioStatus().some(r=>r.channel===uid&&r.playedFrames>1000&&!r.error)', arg=second)
        results.locator('[data-export-format]').select_option('csv')
        with page.expect_download() as download:
            results.locator('[data-decode-action=export]').click()
        download.value.save_as(artifacts / 'ft8-filtered.csv')
        with (artifacts / 'ft8-filtered.csv').open(encoding='utf-8-sig', newline='') as file:
            exported_csv = list(csv.DictReader(file))
        assert exported_csv and all('CQ' in row['message'] for row in exported_csv), exported_csv
        assert all(second in row['identity'] for row in exported_csv)
        results.locator('[data-export-format]').select_option('jsonl')
        with page.expect_download() as download:
            results.locator('[data-decode-action=export]').click()
        download.value.save_as(artifacts / 'ft8-filtered.jsonl')
        exported_jsonl = [json.loads(line) for line in (artifacts / 'ft8-filtered.jsonl').read_text().splitlines()]
        assert exported_jsonl and all(row['time_basis'] == 'browser_receipt' and second in row['identity'] for row in exported_jsonl)
        simultaneous = api(origin)
        assert any(task['receiver_uid'] == second and task['state'] == 'recording' for task in simultaneous['recordings'])
        assert_receivers_continue(before['receivers'], simultaneous['receivers'])
        result_controls.locator('[data-action=rec-iq]').click()
        wait_async_condition(page, 'uid=>fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.recordings.some(t=>t.receiver_uid===uid&&t.state==="completed"))', arg=second)
        page.screenshot(path=str(artifacts / 'decode-messages-desktop.png'))
        results.locator('[data-decode-search]').fill('')
        # Create a backend IQ task via the real list control. View operations must preserve it.
        page.locator('[data-action=sdr-workbench]').click()
        third_row = overview.locator(f'[data-uid="{third}"]')
        third_row.locator('.mc-row-more > summary').click()
        third_row.locator('[data-action=rec-iq]').click()
        wait_async_condition(page, 'uid=>fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.recordings.some(t=>t.receiver_uid===uid&&["preparing","recording"].includes(t.state)))', arg=third)
        target = api(origin)['receivers'][1]; candidate = copy.deepcopy(target['confirmed'])
        candidate['processors'][0].update(mode='sstv', bandwidth_hz=3000)
        candidate['baseband'].update(passband_hz=6000, transition_hz=500, minimum_rate_hz=8000)
        reply = api(origin, payload={'type': 'configure', 'request_id': 'ui-sstv', 'receiver_uid': second,
            'expected_revision': target['revision'], 'config': candidate})
        assert reply['fault'] is None, reply
        overview.locator('[data-close]').click()
        expect(results.locator('canvas[role=img]')).to_be_visible(timeout=20000)
        canvas = results.locator('canvas[role=img]')
        canvas.evaluate('canvas=>{window.uxCanvas=canvas;window.uxFirstPixels=canvas.getContext("2d").getImageData(0,0,canvas.width,canvas.height).data.reduce((n,v,i)=>n+(i%4!==3&&v>0),0);}')
        page.wait_for_function('uid=>{const c=document.querySelector(`[data-view-id="results-${uid}"] canvas[role=img]`);return c===window.uxCanvas&&c.getContext("2d").getImageData(0,0,c.width,c.height).data.reduce((n,v,i)=>n+(i%4!==3&&v>0),0)>window.uxFirstPixels+1000;}', arg=second, timeout=20000)
        live_pixels = canvas.evaluate('canvas=>canvas.getContext("2d").getImageData(0,0,canvas.width,canvas.height).data.reduce((n,v,i)=>n+(i%4!==3&&v>0),0)')
        with page.expect_popup() as popup:
            results.locator('[data-pop]').click()
        child = popup.value; child.on('pageerror', lambda error: errors.append(str(error)))
        child.set_viewport_size({'width': 420, 'height': 760})
        child_results = child.locator(f'[data-view-id="results-{second}"]')
        # The same canvas survives adoption into the popup document.
        assert page.evaluate('window.uxCanvas.isConnected && window.uxCanvas.ownerDocument!==document')
        watch_controls(child_results)
        child_results.locator('[data-decode-action=utc]').focus()
        child_results.locator('[data-decode-action=utc]').press('Space', delay=1300)
        image_controls = assert_controls(child_results)
        assert page.evaluate('window.uxCanvas.isConnected')
        overflow = child_results.evaluate('element=>element.scrollWidth-element.clientWidth')
        assert overflow <= 1, overflow
        child.wait_for_function('''() => { const c=document.querySelector('canvas[role=img]'); const d=c.getContext('2d').getImageData(0,0,c.width,c.height).data;let n=0;for(let i=0;i<d.length;i+=4)if(d[i]+d[i+1]+d[i+2]>0)n++;return n>16000; }''', timeout=20000)
        expect(child_results.locator('[data-decode-search]')).not_to_be_visible()
        with child.expect_download() as download:
            child_results.locator('[data-decode-action=image-export]').click()
        download.value.save_as(artifacts / 'sstv-export.png')
        png = (artifacts / 'sstv-export.png').read_bytes()
        assert png.startswith(b'\x89PNG\r\n\x1a\n')
        exported_size = child.evaluate('''async encoded => {
          const img=new Image(); img.src='data:image/png;base64,'+encoded; await img.decode();
          const c=document.createElement('canvas'); c.width=img.width; c.height=img.height;
          const ctx=c.getContext('2d'); ctx.drawImage(img,0,0);
          if (!ctx.getImageData(0,0,c.width,c.height).data.some((v,i)=>i%4!==3&&v>0)) throw new Error('Downloaded PNG has no decoded pixels');
          return [img.width,img.height];
        }''', base64.b64encode(png).decode())
        assert exported_size[0] == child_results.locator('canvas[role=img]').evaluate('c=>c.width')
        child.screenshot(path=str(artifacts / 'decode-sstv-420.png'))
        page.locator('#btn-lang').click()
        expect(child.locator('html')).to_have_attribute('lang', 'zh-CN')
        child.screenshot(path=str(artifacts / 'decode-sstv-420-zh.png'))
        page.locator('#btn-theme').click()
        expect(child.locator('html')).to_have_attribute('data-theme', 'light')
        child.screenshot(path=str(artifacts / 'decode-sstv-420-zh-light.png'))
        assert page.evaluate('window.uxCanvas.isConnected')
        child_results.locator('[data-decode-action=clear]').click()
        expect(child_results.locator('.mc-decode-confirm')).to_be_visible()
        child_results.locator('[data-decode-action=clear-cancel]').click()
        expect(child_results.locator('canvas[role=img]')).to_be_visible()
        child_results.locator('[data-dock]').click()
        assert page.evaluate('window.uxCanvas.isConnected && window.uxCanvas.ownerDocument===document')
        page.locator('[data-action=sdr-workbench]').click()
        after = api(origin)
        assert any(task['receiver_uid'] == third and task['state'] == 'recording' for task in after['recordings'])
        assert_receivers_continue([before['receivers'][0], before['receivers'][2]], [after['receivers'][0], after['receivers'][2]])
        third_row.locator('[data-action=rec-iq]').click()
        wait_async_condition(page, 'uid=>fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.recordings.some(t=>t.receiver_uid===uid&&t.state==="completed"))', arg=third)
        page.screenshot(path=str(artifacts / 'workbench-product-zh-light.png'))
        report = {'source': 'isolated synthetic AM/IQ and committed FT8/SSTV replay; no RF hardware',
            'held_mouse': held_mouse, 'output_controls': output_controls, 'draft_controls': draft_controls,
            'search_controls': search_controls, 'decode_controls': decode_controls, 'image_controls': image_controls,
            'csv_rows': len(exported_csv), 'jsonl_entries': len(exported_jsonl), 'png_size': exported_size,
            'live_nonzero_components': live_pixels, 'compact_horizontal_overflow': overflow, 'errors': errors,
            'checks': ['row and selected monitor share one listening route', '1500ms real mouse press', '1300ms Space key release activation', 'no removed live controls during status updates',
                'draft/focus/search selection/scroll retention', 'real merged FT8 messages/filter/sort/UTC', 'same-UID listening + backend IQ recording + filtered CSV/JSONL downloads',
                'real AM audio waveform with no search/clear tools', 'actual SSTV PNG download in 420px popup', 'all visible buttons have hover tips',
                'live SSTV pixels update the same canvas', '420px Pop out/Dock preserves canvas and controls',
                'clear confirmation and cancel', 'IQ task continues across viewing operations', 'unaffected receivers retain identity']}
        assert not errors, errors
        browser.close(); return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, default=Path('/tmp/websa-multichannel-ui'))
    args = parser.parse_args(); args.artifacts.mkdir(parents=True, exist_ok=True)
    os.environ['WEBSA_RECORDING_DIR'] = str((args.artifacts / 'recordings').resolve())
    with (args.artifacts / 'service.log').open('w') as log:
        proc, origin = start_service(free_port(), log=log)
        try:
            snapshot = setup(origin)
            report = browser_gate(origin, snapshot, args.artifacts)
            (args.artifacts / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print('PASS: live DOM retention, keyboard/mouse controls and receiver Decode UX (synthetic/replay)')
        finally:
            stop(proc)


if __name__ == '__main__':
    main()
