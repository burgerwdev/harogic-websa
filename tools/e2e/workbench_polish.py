#!/usr/bin/env python3
"""Start `npm run dev:workbench` before running this browser gate. No hardware is used."""

import argparse
import base64
from pathlib import Path

from playwright.sync_api import expect, sync_playwright

parser = argparse.ArgumentParser(
    description="Workbench browser checks using synthetic native snapshots on the Vite development server."
)
parser.add_argument("--origin", default="http://127.0.0.1:5186")
parser.add_argument("--artifacts", type=Path, default=Path("/tmp/websa-workbench-polish"))
args = parser.parse_args()
out = args.artifacts
out.mkdir(parents=True, exist_ok=True)
with sync_playwright() as p:
    browser = p.chromium.launch(headless=True)
    errors = []
    page = browser.new_page(viewport={"width": 1440, "height": 1000})
    page.on("pageerror", lambda error: errors.append(str(error)))
    page.route(
        "**/api/sdr/receiver-presets",
        lambda route: route.fulfill(json={"version": 1, "revision": "1", "presets": []}),
    )
    page.goto(args.origin + "/static/dist/workbench.html")
    page.locator("[data-view-id=overview-follow]").wait_for()
    page.evaluate("""async () => {
      window.dispatchEvent(new Event('pagehide')); document.body.replaceChildren();
      const [{FixtureReceiverAdapter}, {WorkbenchSession}, {mountWorkbench}, {setLang}, {setTheme}] = await Promise.all([
        import('/static/dist/src/dev/fixtureReceiverAdapter.ts'), import('/static/dist/src/ui/workbenchSession.ts'), import('/static/dist/src/ui/workbenchApp.ts'), import('/static/dist/src/core/i18n.ts'), import('/static/dist/src/core/theme.ts')]);
      setLang('en'); setTheme('dark');
      const adapter = new FixtureReceiverAdapter(), snapshot = adapter.snapshot();
      snapshot.transport='native'; snapshot.service_id='ui-review'; snapshot.capabilities.commands=['configure','record_start','record_stop','record_delete','record_rename','create','retire'];
      Object.assign(snapshot.capabilities, {monitor_mixing:true, receiver_presets:true, audio_archive_export:true, audio_archive_read:true, audio_flac_export:true, audio_opus_export:true, iq_archive_read:true, results_archive_export:true, recording_kinds:['audio','iq','results']});
      snapshot.capabilities.mode_descriptors=['am','wfm','usb','ft8','radiofax'].map(id=>({id,kind:['am','wfm','usb'].includes(id)?'analog':'digital',label:id.toUpperCase(),available:true,outputs:['audio'],defaultBandwidthHz:6000,minimumRateHz:12000,minBandwidthHz:50,maxBandwidthHz:192000}));
      snapshot.capture.center_hz=14384067.836569; snapshot.capture.usable_bandwidth_hz=6250000;
      snapshot.receivers[0].confirmed.name='Harbour radio';snapshot.receivers[0].confirmed.baseband.frequency_hz=14200198.803894;
      snapshot.receivers[0].confirmed.processors[0].mode='am';snapshot.receivers[0].confirmed.processors[0].pcm_gain=0.7;
      snapshot.recordings=Array.from({length:7},(_,i)=>({task_id:'saved-'+i,receiver_uid:'retired-'+i,receiver_name:i<4?'Harbour radio':'Mountain station',name:i===0?'Evening broadcast':i<4?'Harbour radio':'Mountain station',created_at_ms:String(Date.now()-i*3600000),state:i===2?'failed':'completed',audio_format:'opus',fault:i===2?{code:'SOURCE_FAILED',message:'The signal source was interrupted.'}:null,tracks:[{kind:'audio',track_id:'track-'+i,container:'websa-audio-files-v1',format:'opus',source:'published_audio',compression:'lossy',state:'completed',stored_bytes:'1570000',raw_bytes:'25000000',segments:[{frequency_hz:i<4?14200199.1:7100000,samples:'18000000',actual_rate_hz:48000,stream:{epoch:'1',channel_id:'1'},first_input_index:'0',end_reason:'stop'}]}]}));
      const fake={snapshot:()=>snapshot,subscribe:()=>()=>{},preflight:()=>({accepted:true,faults:[]}),dispatch:async command=>({request_id:command.request_id,snapshot,fault:null})};
      window.polish = new WorkbenchSession(fake); mountWorkbench(window.polish, document.body);
      window.polishTheme=setTheme;window.polishLang=setLang;
    }""")
    view = page.locator("[data-view-id=overview-follow]")
    expect(view.locator("[data-frequency-value]")).to_have_value("14.200199")
    expect(view.locator(".mc-capture-bar strong")).to_have_text("14.384068 MHz")
    expect(view.locator("[data-recording-capacity]").first).to_contain_text("0 / 16")
    page.screenshot(path=str(out / "receive-en-dark.png"))
    for mode in ["wfm", "radiofax", "am"]:
        view.locator("[name=mode]").select_option(mode)
        assert view.evaluate("e=>e.scrollWidth<=e.clientWidth+2")
    view.locator("[data-action=advanced-settings]").click()
    view.locator("[data-section=iq] summary").click()
    view.locator("[data-receiver-presets] summary").click()
    expect(view.locator("[data-preset-name]")).to_be_visible()
    # Rendering the panel alone also succeeds when the response is rejected.
    # A save is available only after the preset document was validated and loaded.
    expect(view.locator("[data-preset-save]")).to_be_enabled()
    expect(view.locator("[data-preset-status]")).to_have_text("")
    expect(view.locator("[data-preset-list] option")).to_have_count(1)
    page.screenshot(path=str(out / "settings-en-dark.png"))
    view.locator("[data-action=cancel]").click()

    view.locator("[data-action=tab-recording]").click()
    expect(view.locator(".mc-task")).to_have_count(4)
    page.screenshot(path=str(out / "history-en-dark.png"))
    view.locator(".mc-workspace-actions [data-action=recording-files]").click()
    library = page.locator("[data-view-id=recording-library]")
    library.locator("[data-file-search]").fill("14M")
    expect(library.locator(".mc-file")).to_have_count(4)
    page.screenshot(path=str(out / "library-en-dark.png"))
    library.locator("[data-file-search]").fill("Mountain")
    expect(library.locator(".mc-file")).to_have_count(3)
    library.locator("[data-close]").click()
    page.evaluate("polishLang('zh');polishTheme('light')")
    view.locator("[data-action=tab-receive]").click()
    page.screenshot(path=str(out / "receive-zh-light.png"))
    page.set_viewport_size({"width": 420, "height": 900})
    page.wait_for_timeout(250)
    view.locator("[data-editor]").scroll_into_view_if_needed()
    page.screenshot(path=str(out / "receive-zh-light-420.png"))
    assert view.evaluate("e=>e.scrollWidth<=e.clientWidth+2")
    view.locator("[data-action=tab-recording]").click()
    page.screenshot(path=str(out / "history-zh-light-420.png"))
    assert view.evaluate("e=>e.scrollWidth<=e.clientWidth+2")
    view.locator(".mc-workspace-actions [data-action=recording-files]").click()
    page.screenshot(path=str(out / "library-zh-light-420.png"))
    assert library.evaluate("e=>e.scrollWidth<=e.clientWidth+2")
    library.locator("[data-close]").click()
    page.set_viewport_size({"width": 1440, "height": 1000})
    with page.expect_popup() as popup:
        view.locator("[data-pop]").click()
    child = popup.value
    child.wait_for_timeout(250)
    expect(child.locator("[data-default-audio-format]")).to_be_visible()
    assert child.locator("[data-view-id=overview-follow]").evaluate(
        "e=>e.scrollWidth<=e.clientWidth+2"
    )
    # Capture the current rendered popup without waiting for cloned @font-face loads.
    cdp = child.context.new_cdp_session(child)
    (out / "popout-zh-light.png").write_bytes(
        base64.b64decode(cdp.send("Page.captureScreenshot")["data"])
    )
    cdp.detach()
    child.locator("[data-dock]").click()
    expect(view).to_be_visible()
    page.evaluate('polishTheme("dark");polishLang("en")')
    view.locator("[data-action=create]").first.click()
    expect(page.locator(".mc-create-dialog[open]")).to_be_visible()
    page.screenshot(path=str(out / "create-en-dark.png"))
    assert not errors, errors
    print("frequency, matching history, unit/name search, themes, narrow layout, and popout passed")
    browser.close()
