#!/usr/bin/env python3
"""Main SDR quick-add, explicit replacement, channel selection and shared hover on a synthetic source."""

from __future__ import annotations

import argparse
import json
import urllib.request
from pathlib import Path

from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, post, start_service, stop
from viewport_baseline import wait_ui_ready


def api(origin, payload=None):
    request = urllib.request.Request(
        origin + "/api/sdr/receivers",
        data=None if payload is None else json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.load(response)


def browser_gate(origin, artifacts):
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=True)
        page = browser.new_page(viewport={"width": 1600, "height": 1000})
        # Hold only spectrum arrivals to exercise an unsettled retune preview.
        page.add_init_script("""(() => {
          const Native = WebSocket; window.__heldFft = [];
          window.WebSocket = class extends Native { constructor(...args) {
            super(...args); this.addEventListener('message', e => {
              if (!window.__holdFft || !(e.data instanceof ArrayBuffer)) return;
              const magic = String.fromCharCode(...new Uint8Array(e.data, 0, 4));
              if (magic === 'RTAF' || magic === 'RTA2') { window.__heldFft = [this,e.data]; e.stopImmediatePropagation(); }
            });
          }};
        })()""")
        errors = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.goto(origin)
        wait_ui_ready(page, "sdr")
        expect(page.locator("#waterfall")).to_be_visible()
        page.wait_for_function(
            'Math.abs(document.querySelector(".spectrum-box").getBoundingClientRect().height-document.querySelector("#waterfall-container").getBoundingClientRect().height)<2'
        )
        expect(page.locator("[data-action=sdr-channel-add]")).to_have_attribute(
            "title", "Add the current frequency and demodulation settings to an independent channel"
        )
        page.locator("#btn-lang").click()
        add = page.locator("[data-action=sdr-channel-add]")
        expect(add).to_have_attribute("title", "将当前频点和解调设置快速加入独立信道")
        listen = page.locator("#input-sdr-listen")
        waterfall = page.locator("#waterfall")

        # A canvas gesture cancels native focus transfer. It must explicitly leave
        # both frequency editors so arrows resume tuning without a blank-area click.
        def confirmed_frequency():
            return api(origin)["receivers"][0]["confirmed"]["baseband"]["frequency_hz"]

        def wait_frequency(value):
            page.wait_for_function(
                'value => fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>Math.abs(s.receivers[0].confirmed.baseband.frequency_hz-value)<0.01)',
                arg=value,
            )

        for editor in ["#input-sdr-center", "#input-sdr-listen"]:
            for surface in ["#spectrum", "#waterfall"]:
                field = page.locator(editor)
                field.fill("100")
                field.press("Enter")
                wait_frequency(100e6)
                page.locator(surface).click(
                    position={
                        "x": page.locator(surface).bounding_box()["width"] * 0.65,
                        "y": page.locator(surface).bounding_box()["height"] * 0.6,
                    }
                )
                expect(page.locator(surface)).to_be_focused()
                # Wait for the click's tune request before measuring the key step.
                page.wait_for_timeout(200)
                before_key = confirmed_frequency()
                page.keyboard.press("ArrowRight")
                wait_frequency(before_key + 1000)
                page.keyboard.press("ArrowLeft")
                wait_frequency(before_key)
                # Focusing an editor still gives its cursor the native arrow keys.
                field.focus()
                page.keyboard.press("ArrowRight")
                page.wait_for_timeout(100)
                assert confirmed_frequency() == before_key
        page.locator("#btn-keypad").click()
        keypad = page.locator("#keypad")
        for editor in ["#input-sdr-center", "#input-sdr-listen"]:
            field = page.locator(editor)
            field.click()
            expect(keypad).to_be_visible()
            before_pad = confirmed_frequency()
            # Opening the pad blurs its source input. Arrows must still belong to
            # the pad rather than tune the receiver behind an unfinished draft.
            page.keyboard.press("ArrowRight")
            page.wait_for_timeout(100)
            assert confirmed_frequency() == before_pad
            for digit in "100.02":
                keypad.get_by_text(digit, exact=True).click()
            if editor == "#input-sdr-center":
                keypad.locator(".ok").click()
            else:
                page.keyboard.press("Enter")
            expect(keypad).not_to_be_visible()
            wait_frequency(100020000)
            field.click()
            expect(keypad).to_be_visible()
            for digit in "100.09":
                keypad.get_by_text(digit, exact=True).click()
            page.keyboard.press("Escape")
            expect(keypad).not_to_be_visible()
            page.wait_for_timeout(100)
            assert confirmed_frequency() == 100020000, (
                "Cancelling a virtual keypad draft must not tune"
            )
            field.click()
            expect(keypad).to_be_visible()
            keypad.get_by_text("9", exact=True).click()
            # Clicking the plot dismisses the draft and restores normal tuning.
            plot = page.locator("#spectrum")
            box = plot.bounding_box()
            plot.click(position={"x": box["width"] * 0.65, "y": box["height"] * 0.6})
            expect(keypad).not_to_be_visible()
            expect(plot).to_be_focused()
            page.wait_for_timeout(200)
            before_key = confirmed_frequency()
            page.keyboard.press("ArrowRight")
            wait_frequency(before_key + 1000)
        page.locator("#btn-keypad").click()
        center = page.locator("#input-sdr-center")
        center.fill("100")
        center.press("Enter")
        wait_frequency(100e6)
        page.wait_for_timeout(200)
        # Retuning a paused waterfall must preserve and reproject original RF
        # observations. Returning to the old capture restores the exact pixels.
        page.locator("#btn-wf-pause").click()
        page.mouse.move(5, 5)
        page.wait_for_timeout(100)
        history_pixels = """() => {
            const canvas=document.getElementById('waterfall');
            return [...canvas.getContext('2d').getImageData(0,0,canvas.width,Math.min(canvas.height,80)).data];
        }"""
        paused_pixels = page.evaluate(history_pixels)
        center.fill("100.05")
        center.press("Enter")
        wait_frequency(100050000)
        page.wait_for_timeout(200)
        assert page.evaluate(history_pixels) != paused_pixels
        center.fill("100")
        center.press("Enter")
        wait_frequency(100e6)
        page.wait_for_timeout(200)
        assert page.evaluate(history_pixels) == paused_pixels, (
            "Retuning back must restore paused history without resampling drift or clearing"
        )
        page.screenshot(path=str(artifacts / "waterfall-retune-return.png"))
        page.locator("#btn-wf-pause").click()
        box = waterfall.bounding_box()
        page.mouse.click(box["x"] + box["width"] * 0.75, box["y"] + box["height"] * 0.5)
        page.wait_for_function(
            'fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>Math.abs(s.receivers[0].confirmed.baseband.frequency_hz-100048828.125)<500)'
        )
        listen.fill("100")
        listen.press("Enter")
        page.wait_for_function(
            'fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers[0].confirmed.baseband.frequency_hz===100000000)'
        )
        page.locator("#btn-spectrum-zoom").click()
        box = waterfall.bounding_box()
        before_zoom = api(origin)["receivers"][0]["confirmed"]
        page.mouse.move(box["x"] + box["width"] * 0.25, box["y"] + box["height"] * 0.5)
        page.mouse.down()
        page.mouse.move(box["x"] + box["width"] * 0.65, box["y"] + box["height"] * 0.5, steps=8)
        page.screenshot(path=str(artifacts / "waterfall-marquee.png"))
        page.mouse.up()
        expect(page.locator("#btn-zoom-reset")).to_be_enabled()
        assert api(origin)["receivers"][0]["confirmed"] == before_zoom
        page.locator("#btn-zoom-reset").click()
        page.locator("#btn-spectrum-zoom").click()
        add.click()
        page.wait_for_function(
            'fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.length===2)'
        )
        snapshot = api(origin)
        assert len({r["audio_signature"] for r in snapshot["receivers"]}) == 1
        page.wait_for_function(
            'window.websaAudioStatus().filter(r=>typeof r.channel==="string"&&r.enabled).length===1'
        )
        # The shortcut does not also open the workbench.
        assert page.locator("[data-view-id=overview-follow]:visible").count() == 0
        listen.fill("100.02")
        listen.press("Enter")
        page.wait_for_function(
            'fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.find(r=>r.display_number===1).confirmed.baseband.frequency_hz===100020000)'
        )
        page.wait_for_function(
            'window.websaAudioStatus().filter(r=>typeof r.channel==="string"&&r.enabled).length===2'
        )
        add.click()
        page.wait_for_function(
            'fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.length===3)'
        )
        listen.fill("100.04")
        listen.press("Enter")
        page.wait_for_function(
            'fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.find(r=>r.display_number===1).confirmed.baseband.frequency_hz===100040000)'
        )
        add.click()
        dialog = page.locator("[data-replace-channel]")
        expect(dialog).to_be_visible()
        expect(dialog.locator("[name=replace]")).to_have_count(3)
        expect(dialog.locator("[type=submit]")).to_be_disabled()
        before = api(origin)["receivers"]
        second = next(r for r in before if r["display_number"] == 2)
        dialog.screenshot(path=str(artifacts / "replace-channel.png"))
        dialog.locator(f'[value="{second["receiver_uid"]}"]').check()
        dialog.locator("[type=submit]").click()
        expect(dialog).not_to_be_visible()
        after = api(origin)["receivers"]
        assert all(
            next(r for r in after if r["receiver_uid"] == old["receiver_uid"])["confirmed"]
            == old["confirmed"]
            for old in before
            if old["display_number"] != 2
        )
        assert (
            next(r for r in after if r["display_number"] == 2)["confirmed"]["baseband"][
                "frequency_hz"
            ]
            == 100040000
        )
        page.locator("[data-action=sdr-channel-next]").click()
        expect(page.locator("[data-action=sdr-channel-prev]")).to_have_attribute(
            "title", "上一个接收信道"
        )
        expect(page.locator("[data-action=sdr-channel-next]")).to_have_attribute(
            "title", "下一个接收信道"
        )
        expect(page.locator("#sdr-current-channel")).to_have_text("C2")
        listen.fill("100.05")
        listen.press("Enter")
        page.wait_for_function(
            'fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.find(r=>r.display_number===2).confirmed.baseband.frequency_hz===100050000)'
        )
        assert (
            next(r for r in api(origin)["receivers"] if r["display_number"] == 1)["confirmed"][
                "baseband"
            ]["frequency_hz"]
            == 100040000
        )
        # All channel buttons start at the same value-column edge as the tuning buttons.
        workbench = page.locator("[data-action=sdr-workbench]").bounding_box()
        tune = page.locator("[data-action=sdr-tune-down]").bounding_box()
        assert abs(workbench["x"] - tune["x"]) < 1, (workbench, tune)
        if not page.locator("#waterfall").is_visible():
            page.locator("#btn-waterfall").click()
        page.locator("#btn-wf-pause").click()
        page.locator("#btn-view-freeze").evaluate("button=>button.click()")
        page.mouse.move(5, 5)
        page.wait_for_timeout(300)
        # Compare actual canvas pixels with history paused. A horizontal stripe avoids OSD text.
        stripe = """id => {
          const c=document.getElementById(id),g=c.getContext('2d');
          return [...g.getImageData(0,Math.floor(c.height*0.7),c.width,1).data];
        }"""
        baseline = {id: page.evaluate(stripe, id) for id in ["spectrum", "waterfall"]}
        box = page.locator("#waterfall").bounding_box()
        page.mouse.move(box["x"] + box["width"] * 0.25, box["y"] + box["height"] * 0.5)
        page.wait_for_timeout(150)
        hovered = {id: page.evaluate(stripe, id) for id in baseline}
        assert all(hovered[id] != baseline[id] for id in baseline), (
            "Waterfall hover must paint both plots"
        )
        expect(page.locator("#spectrum")).not_to_have_attribute("data-cursor", "")
        page.screenshot(path=str(artifacts / "shared-hover.png"))
        page.mouse.move(5, 5)
        page.wait_for_timeout(150)
        expect(page.locator("#spectrum")).to_have_attribute("data-cursor", "")
        assert page.evaluate(stripe, "waterfall") == baseline["waterfall"], (
            "Hover must erase from paused history"
        )
        page.locator("#btn-theme").click()
        page.mouse.move(5, 5)
        page.wait_for_timeout(150)
        light_baseline = page.evaluate(stripe, "waterfall")
        page.mouse.move(box["x"] + box["width"] * 0.25, box["y"] + box["height"] * 0.5)
        page.wait_for_timeout(150)
        expect(page.locator("#spectrum")).not_to_have_attribute("data-cursor", "")
        assert page.evaluate(stripe, "waterfall") != light_baseline
        page.screenshot(path=str(artifacts / "shared-hover-light.png"))
        page.mouse.move(5, 5)
        page.wait_for_timeout(150)
        assert page.evaluate(stripe, "waterfall") == light_baseline
        page.locator("#btn-theme").click()
        spectrum = page.locator("#spectrum").bounding_box()
        page.mouse.move(box["x"] + box["width"] * 0.35, spectrum["y"] + spectrum["height"] * 0.5)
        page.wait_for_timeout(150)
        assert page.evaluate(stripe, "waterfall") != baseline["waterfall"], (
            "Spectrum hover must paint waterfall"
        )
        page.mouse.move(5, 5)
        page.wait_for_timeout(150)
        assert page.evaluate(stripe, "waterfall") == baseline["waterfall"]
        # Both frozen plots must respond to the same horizontal zoom slider.
        # Amplitude sliders change the spectrum scale while waterfall colours
        # keep their independently controlled range and retained history.
        before_sliders = [r["confirmed"] for r in api(origin)["receivers"]]
        page.locator("#btn-spectrum-zoom").click()
        page.wait_for_timeout(150)
        slider_baseline = {id: page.evaluate(stripe, id) for id in ["spectrum", "waterfall"]}

        def move_slider(key, value):
            page.locator("#zoom-rail-" + key).evaluate(
                '(input,value)=>{input.value=String(value);input.dispatchEvent(new Event("input",{bubbles:true}));}',
                value,
            )

        for amount in [350, 750, 1000, 500]:
            move_slider("horizontal", amount)
            changed = {id: page.evaluate(stripe, id) for id in slider_baseline}
            assert all(changed[id] != slider_baseline[id] for id in changed), (
                "Horizontal zoom must redraw spectrum and paused waterfall together"
            )
        move_slider("horizontal", 0)
        assert page.evaluate(stripe, "waterfall") == slider_baseline["waterfall"], (
            "Zooming out must restore exact historical pixels"
        )
        move_slider("top", -45)
        move_slider("bottom", -125)
        assert page.evaluate(stripe, "spectrum") != slider_baseline["spectrum"]
        assert page.evaluate(stripe, "waterfall") == slider_baseline["waterfall"], (
            "Spectrum amplitude sliders must not flatten waterfall colours"
        )
        page.screenshot(path=str(artifacts / "waterfall-zoom-sliders.png"))
        assert [r["confirmed"] for r in api(origin)["receivers"]] == before_sliders, (
            "Display sliders must not retune the radio"
        )
        # Native history supplies the local floor even while acquisition continues
        # elsewhere. All rows use one current-view colour scale.
        page.locator("#select-wf-floor-source").evaluate(
            'e=>{e.value="view";e.dispatchEvent(new Event("change",{bubbles:true}));}'
        )

        def history():
            return page.locator("#waterfall").evaluate("e=>JSON.parse(e.dataset.history)")

        frozen_rows = history()["rows"]

        def shared_window():
            wf, sp = page.evaluate(
                """()=>[JSON.parse(document.querySelector('#waterfall').dataset.history),JSON.parse(document.querySelector('#spectrum').dataset.frequencyView)]"""
            )
            assert abs(wf["lo"] - sp["lo"]) < 0.001 and abs(wf["hi"] - sp["hi"]) < 0.001, (wf, sp)
            assert wf["rows"] == frozen_rows
            return wf

        timings = page.locator("#zoom-rail-horizontal").evaluate("""async e=>{
          const timings=[];
          for(let value=0;value<=1000;value+=25) {
            e.value=String(value);const at=performance.now();
            e.dispatchEvent(new Event('input',{bubbles:true}));timings.push(performance.now()-at);
            await new Promise(requestAnimationFrame);
          } return timings;
        }""")
        shared_window()
        move_slider("horizontal", 500)
        ov = page.locator("#zoom-overview")
        ov.click(position={"x": ov.bounding_box()["width"] * 0.75, "y": 20})
        page.mouse.move(5, 5)
        shared_window()
        move_slider("horizontal", 0)
        shared_window()
        page.wait_for_function(
            'JSON.parse(document.querySelector("#waterfall").dataset.history).settled',
            timeout=15000,
        )
        local_pixels = page.evaluate(stripe, "waterfall")
        page.wait_for_timeout(500)
        assert page.evaluate(stripe, "waterfall") == local_pixels, (
            "Live data must not recolour frozen view history"
        )
        for value in ["fixed", "auto"]:
            page.locator("#select-wf-range").evaluate(
                '(e,value)=>{e.value=value;e.dispatchEvent(new Event("change",{bubbles:true}));}',
                value,
            )
        assert page.evaluate(stripe, "waterfall") == local_pixels, (
            "Auto/Fixed round trip restores native history"
        )
        center = page.locator("#input-sdr-center")
        at_center = float(center.input_value())
        at_listen = listen.input_value()
        for offset in [0.01, 0]:
            center.fill(str(at_center + offset))
            center.press("Enter")
            page.wait_for_timeout(250)
            shared_window()
        listen.fill(at_listen)
        listen.press("Enter")
        page.wait_for_timeout(300)
        page.mouse.move(5, 5)
        page.wait_for_timeout(100)
        page.wait_for_function(
            'JSON.parse(document.querySelector("#waterfall").dataset.history).settled',
            timeout=15000,
        )
        assert page.evaluate(stripe, "waterfall") == local_pixels, (
            "Paused local-floor retune return restores exact history"
        )
        # Hold new-axis FFTs while the axis gesture has committed. A slider must
        # rebase that preview immediately and keep both plots on the same Hz.
        page.evaluate("window.__holdFft=true")
        box = page.locator("#spectrum").bounding_box()
        page.mouse.move(box["x"] + box["width"] * 0.6, box["y"] + box["height"] - 8)
        page.mouse.down()
        page.mouse.move(box["x"] + box["width"] * 0.55, box["y"] + box["height"] - 8, steps=5)
        page.mouse.up()
        for amount in [250, 700, 1000, 0]:
            move_slider("horizontal", amount)
            shared_window()
        page.evaluate(
            """()=>{window.__holdFft=false;const [s,b]=window.__heldFft;if(s&&b)s.dispatchEvent(new MessageEvent('message',{data:b}));window.__heldFft=[];}"""
        )
        page.wait_for_timeout(300)
        shared_window()
        expect(page.locator("#spectrum")).to_have_attribute("data-density", "on")
        (artifacts / "view-floor-browser.json").write_text(
            json.dumps(
                {"frozen_rows": frozen_rows, "slider_redraw_ms": timings, "history": history()},
                indent=2,
            )
            + "\n"
        )
        page.screenshot(path=str(artifacts / "view-floor-paused.png"))
        page.locator("#select-wf-floor-source").evaluate(
            'e=>{e.value="capture";e.dispatchEvent(new Event("change",{bubbles:true}));}'
        )
        page.locator("#btn-spectrum-zoom").click()
        page.screenshot(path=str(artifacts / "channels-desktop.png"))
        page.set_viewport_size({"width": 480, "height": 800})
        add.scroll_into_view_if_needed()
        workbench = page.locator("[data-action=sdr-workbench]").bounding_box()
        tune = page.locator("[data-action=sdr-tune-down]").bounding_box()
        assert abs(workbench["x"] - tune["x"]) < 1
        page.screenshot(path=str(artifacts / "channels-narrow.png"))
        page.set_viewport_size({"width": 1600, "height": 1000})
        page.locator("[data-action=sdr-workbench]").click()
        overview = page.locator("[data-view-id=overview-follow]")
        toggle = overview.locator(".mc-channel-actions [role=switch]")
        expect(toggle).to_have_attribute("aria-checked", "true")
        positions = overview.locator(".mc-channel-actions button").evaluate_all(
            "buttons=>buttons.map(b=>b.getBoundingClientRect().y)"
        )
        assert max(positions) - min(positions) < 1
        toggle.click()
        expect(toggle).to_have_attribute("aria-checked", "false")
        toggle.click()
        expect(toggle).to_have_attribute("aria-checked", "true")
        snapshot = api(origin)
        third = next(r for r in snapshot["receivers"] if r["display_number"] == 3)
        third_row = overview.locator(f'[data-uid="{third["receiver_uid"]}"]')
        third_row.locator(".mc-row-more > summary").click()
        expect(third_row.locator("[data-action=duplicate]")).to_be_disabled()
        third_row.locator("[data-action=retire]").click()
        expect(third_row).to_have_count(0)
        assert next(r for r in api(origin)["receivers"] if r["display_number"] == 2)["confirmed"][
            "enabled"
        ]
        first_uid = next(r for r in snapshot["receivers"] if r["display_number"] == 1)[
            "receiver_uid"
        ]
        first_row = overview.locator(f'[data-uid="{first_uid}"]')
        first_row.locator(".mc-row-more > summary").click()
        first_row.locator("[data-action=duplicate]").click()
        page.wait_for_function(
            'fetch("/api/sdr/receivers").then(r=>r.json()).then(s=>s.receivers.length===3)'
        )
        page.screenshot(path=str(artifacts / "workbench-actions.png"))
        page.set_viewport_size({"width": 420, "height": 800})
        overview.locator(".mc-channel-actions").scroll_into_view_if_needed()
        positions = overview.locator(".mc-channel-actions button").evaluate_all(
            "buttons=>buttons.map(b=>b.getBoundingClientRect().y)"
        )
        assert max(positions) - min(positions) < 1
        page.screenshot(path=str(artifacts / "workbench-actions-narrow.png"))
        overview.locator("[data-close]").click()
        page.set_viewport_size({"width": 1600, "height": 1000})
        page.locator("#btn-waterfall").click()
        expect(waterfall).not_to_be_visible()
        page.locator("[data-action=preset]").click()
        expect(waterfall).to_be_visible()
        expect(page.locator("#sdr-current-channel")).to_have_text("C1")
        page.wait_for_function(
            'Math.abs(document.querySelector(".spectrum-box").getBoundingClientRect().height-document.querySelector("#waterfall-container").getBoundingClientRect().height)<2'
        )
        page.screenshot(path=str(artifacts / "sdr-preset-default.png"))
        assert not errors, errors
        browser.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, default=Path("/tmp/websa-channel-shortcuts"))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    process = None
    try:
        process, origin = start_service(free_port())
        post(origin, {"cmd": "SET_MODE", "mode": "sdr"})
        post(origin, {"cmd": "SET_SDR", "center": 100e6, "listen": 100e6, "decimate": 256})
        post(
            origin,
            {
                "cmd": "SET_SDR_CHANNEL",
                "channel": 1,
                "enabled": True,
                "freq": 100e6,
                "mode": "am",
                "ifbw": 6000,
            },
        )
        for receiver in api(origin)["receivers"]:
            if receiver["display_number"] != 1:
                reply = api(
                    origin,
                    {
                        "type": "retire",
                        "request_id": "clear-" + receiver["receiver_uid"],
                        "receiver_uid": receiver["receiver_uid"],
                    },
                )
                assert reply["fault"] is None, reply
        browser_gate(origin, args.artifacts)
        print(
            f"PASS: SDR frequency focus, virtual keypad, retune history, zoom sliders, shared hover, preset split, duplicate audio and channel actions; screenshots: {args.artifacts}"
        )
    finally:
        stop(process)


if __name__ == "__main__":
    main()
