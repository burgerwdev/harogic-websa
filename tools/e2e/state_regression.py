#!/usr/bin/env python3
"""Front-end state-machine regression (Playwright, no RF signal required).

Guards the single-owner parameter model introduced for the SDR group:
  frontend/modern/src/core/params.ts   (confirmed / desired / epoch slots)
  frontend/modern/src/ui/sdrState.ts   (the SDR group's owner + its only renderer)

It drives the real UI against a running service and checks the properties that used to
break - not the implementation:

  1. tune     - a value set in the UI reaches the backend and both agree
  2. demod    - demod / IF bandwidth / de-emphasis reach the backend and highlight follows
  3. preset   - Preset inside SDR must not reappear with the pre-Preset frequency
                (the old `pendingSdrFreq` hand-off survived a reset and did exactly that)
  4. reload   - after a page reload the controls show the backend's values, not whatever
                the browser restored into the form
  5. handoff  - entering SDR from the swept view follows the swept centre
  6. modes    - mode round trip leaves no stuck pending state (buttons usable again)

Usage:  python3 tools/e2e/state_regression.py [--url http://127.0.0.1:8080]

This script is device-agnostic by construction (every expectation is read from /api/state or
the DOM) and runs unchanged against the fake backend, which is how CI covers the parameter
state machine:
    WEBSA_FAKE=1 WEBSA_PORT=8099 python3 -m web_sa.supervisor &
    python3 tools/e2e/state_regression.py --url http://127.0.0.1:8099

Discipline: never relax or branch an assertion to make it pass on the fake - that would
weaken the bench run too. A genuinely device-only check (e.g. one that needs a real trace
length or the IF-overflow warning) passes `require_device=True`, which skips it with a visible
SKIP line when the backend is the fake. No check needs that today.

Exit status is non-zero when any check fails, so it can be used in CI (the service has to
be running; the acquisition loop requires a WebSocket client, which this script is).
"""
from __future__ import annotations

import argparse
import json
import math
import sys
import time
import urllib.request

from playwright.sync_api import Page, sync_playwright

FAILURES: list[str] = []

#: True when the service runs the fake backend (see hardware/fake_device.py); set in main().
FAKE_BACKEND = False


def skip(name: str, reason: str) -> None:
    """Report a check that cannot be exercised in this environment.

    Used where a scenario depends on the signal in front of the antenna (a weak trace cannot be
    pushed out of a 100 dB window within the allowed Ref range). The same scenario runs whenever a
    source is present, and it is asserted unconditionally on the fake backend in CI.
    """
    print(f"  SKIP  {name}  <- {reason}")


def check(name: str, ok: bool, detail: str = "", require_device: bool = False) -> None:
    """Record one check. `require_device` skips it (visibly) on the fake backend."""
    if require_device and FAKE_BACKEND:
        print(f"  SKIP  {name}  <- needs the analyzer")
        return
    print(f"  {'PASS' if ok else 'FAIL'}  {name}{('  <- ' + detail) if detail else ''}")
    if not ok:
        FAILURES.append(name)


def post(url: str, cmd: dict) -> dict:
    req = urllib.request.Request(
        f"{url}/api/config",
        data=json.dumps(cmd).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.load(r)
    except Exception as exc:  # a rejected command must fail the check, not the run
        return {"error": repr(exc)}


def state(url: str) -> dict:
    with urllib.request.urlopen(f"{url}/api/state", timeout=10) as r:
        return json.load(r)



def painted_pixels(page) -> int:
    """Number of non-transparent pixels on the spectrum canvas (0 = nothing drawn).

    The regression used to assert frames/datasets only, so a renderer that was never
    registered (the hub module lost its import) passed every check while the user saw a
    blank canvas. Counting painted pixels is the direct check.
    """
    return page.evaluate(
        """() => {
          const c = document.getElementById('spectrum');
          const g = c.getContext('2d');
          const d = g.getImageData(0, 0, c.width, c.height).data;
          let n = 0;
          for (let i = 3; i < d.length; i += 4) if (d[i] > 0) n++;
          return n;
        }"""
    )


def sdr_panel_visible(page: Page) -> bool:
    return page.is_visible("#sdr-settings")


def enter_sdr(page: Page) -> None:
    """#btn-mode-sdr is a toggle: only click when the SDR panel is not already shown."""
    if not sdr_panel_visible(page):
        page.click("#btn-mode-sdr")
        page.wait_for_timeout(2500)


def main() -> int:
    global FAKE_BACKEND
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:8080")
    args = ap.parse_args()
    url = args.url.rstrip("/")
    try:
        FAKE_BACKEND = str(state(url).get("device", "")).startswith("FAKE")
    except Exception:
        FAKE_BACKEND = False
    print(f"backend: {'fake' if FAKE_BACKEND else 'device'} ({url})")

    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page()
        errors: list[str] = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.on(
            "console",
            lambda m: errors.append(f"console.{m.type}: {m.text}") if m.type == "error" else None,
        )

        page.goto(url, wait_until="networkidle")
        page.evaluate("localStorage.setItem('web-sa-mode','std')")
        page.reload(wait_until="networkidle")
        page.wait_for_timeout(2500)

        # 1 - tune
        print("1) tune")
        painted = painted_pixels(page)
        check("spectrum is actually drawn (renderer registered)",
              painted > 5000, f"{painted} painted pixels")
        enter_sdr(page)
        page.fill("#input-sdr-center", "90.5")
        page.click('[data-action="apply-sdr"]')
        page.wait_for_timeout(2200)
        page.fill("#input-sdr-listen", "90.5")
        page.click('[data-action="apply-sdr-tune"]')
        page.wait_for_timeout(2200)
        d = state(url)
        centre, listen = d["sdr"]["center"] / 1e6, d["sdr"]["listen"] / 1e6
        check("centre reaches the backend", abs(centre - 90.5) < 0.01, f"backend {centre:.4f} MHz")
        check("listen reaches the backend", abs(listen - 90.5) < 0.01, f"backend {listen:.4f} MHz")
        check(
            "controls show the backend values",
            abs(float(page.input_value("#input-sdr-center")) - centre) < 0.01,
            f"input {page.input_value('#input-sdr-center')}",
        )

        # 1b - SDR centre alignment: the canvas maps the frame's display window across its
        # width, so the canvas centre is (start+stop)/2. It must equal the requested centre
        # within one division, whatever the hardware capture offset did.
        print("1b) SDR centre alignment")
        win = page.evaluate(
            "JSON.parse(document.getElementById('spectrum').dataset.sdrWindow || '{}')"
        )
        actual = d["sdr"].get("actual") or {}
        if win and win.get("hi", 0) > win.get("lo", 0):
            division = (win["hi"] - win["lo"]) / 10
            check(
                "the display window is centred on the requested frequency",
                abs((win["lo"] + win["hi"]) / 2 - 90.5e6) < division,
                f"window {(win['lo'] + win['hi']) / 2 / 1e6:.4f} MHz (1 div = {division / 1e3:.1f} kHz)",
            )
            check(
                "the capture window is published separately",
                "capture_start" in actual and "capture_stop" in actual
                and actual["capture_stop"] > actual["capture_start"],
                f"capture {actual.get('capture_start')}..{actual.get('capture_stop')}",
            )
            check(
                "the display and capture windows have the same width",
                abs((win["hi"] - win["lo"])
                    - (actual["capture_stop"] - actual["capture_start"])) < 2 * division,
                f"display {win['hi'] - win['lo']:.0f} vs capture "
                f"{actual['capture_stop'] - actual['capture_start']:.0f} Hz",
            )
            # The capture must not be tuned away from the requested centre: the old
            # +200 kHz "avoid the zero-IF DC centre" shift left the lowest 200 kHz of the
            # display uncovered (a blank strip at the left edge of the panadapter).
            check(
                "the capture is not offset from the display window",
                abs(actual["capture_start"] - win["lo"]) < division
                and abs(actual["capture_stop"] - win["hi"]) < division,
                f"offset {actual['capture_start'] - win['lo']:.0f} Hz",
            )
        else:
            check("the display window is published on the canvas", bool(win), str(win))

        # 1c - entering SDR must show a usable trace: the automatic scale was armed on
        # entry, and the value it decided is the one the canvas renders (the reported
        # "Preset -> SDR spectrum overflows the canvas").
        print("1c) SDR automatic scale is applied")
        dbg = page.evaluate(
            "JSON.parse(document.getElementById('spectrum').dataset.sdrRefDbg || '{}')"
        )
        if dbg and "peak" in dbg:
            check(
                "the auto-scale decision is what the canvas shows",
                abs(dbg.get("shown", 1e9) - dbg.get("ref", -1e9)) < 3,
                str(dbg),
            )
            check(
                "the trace fits the window (Ref - peak <= 100 dB)",
                dbg.get("shown", 1e9) - dbg.get("peak", -1e9) <= 100,
                str(dbg),
            )
        else:
            check("the SDR auto-scale published a decision", False, str(dbg))

        # 2 - demod group
        print("2) demod / IF bandwidth / de-emphasis")
        page.click('[data-sdr-demod="nfm"]')
        page.wait_for_timeout(1500)
        page.click('[data-sdr-ifbw="12000"]')
        page.wait_for_timeout(1500)
        page.click('[data-sdr-deemph="75"]')
        page.wait_for_timeout(1800)
        d = state(url)
        check("demod reaches the backend", d["sdr"]["demod"] == "nfm", d["sdr"]["demod"])
        check("IF bandwidth reaches the backend", abs(d["sdr"]["if_bw"] - 12000) < 1, str(d["sdr"]["if_bw"]))
        actual = (d["sdr"].get("actual") or {}).get("deemph_us")
        check("de-emphasis is applied", actual is not None and abs(actual - 75) < 1, str(actual))
        # ...and the selection must survive the slot TTL. The backend did not report the
        # requested de-emphasis, so the button fell back to Auto after ~3 s.
        page.wait_for_timeout(3500)
        check(
            "de-emphasis selection does not fall back to Auto",
            page.eval_on_selector('[data-sdr-deemph="75"]', "e => e.classList.contains('active')")
            and abs(float(state(url)["sdr"].get("deemph_us", -1)) - 75) < 1,
            f"active={page.eval_on_selector_all('[data-sdr-deemph].active', 'e => e.map(x => x.dataset.sdrDeemph)')}",
        )
        check(
            "button highlight follows the state",
            page.eval_on_selector_all(
                "[data-sdr-demod].active", "els => els.map(e => e.dataset.sdrDemod)"
            )
            == ["nfm"],
        )

        # 3 - Preset inside SDR: the old hand-off came back with the pre-Preset frequency
        print("3) Preset inside SDR")
        page.fill("#input-sdr-center", "101.7")
        page.click('[data-action="apply-sdr"]')
        page.wait_for_timeout(2000)
        page.click('[data-action="preset"]')
        page.wait_for_timeout(2500)
        d = state(url)
        after = d["sdr"]["center"] / 1e6
        check(
            "no pre-Preset frequency is reapplied",
            abs(after - 101.7) > 1,
            f"centre after Preset {after:.4f} MHz",
        )
        check("Preset inside SDR keeps SDR active", sdr_panel_visible(page), d["mode"])

        # 4 - reload: controls must follow the backend, not the browser-restored form value
        print("4) reload")
        enter_sdr(page)
        page.fill("#input-sdr-center", "433")
        page.click('[data-action="apply-sdr"]')
        page.wait_for_timeout(2200)
        page.reload(wait_until="networkidle")
        page.wait_for_timeout(3000)
        d = state(url)
        shown = float(page.input_value("#input-sdr-center"))
        check(
            "controls match the backend after reload",
            abs(shown * 1e6 - d["sdr"]["center"]) < 1e4,
            f"input {shown:.4f} vs backend {d['sdr']['center'] / 1e6:.4f} MHz",
        )

        # 5 - hand-off from the swept view
        print("5) swept view -> SDR hand-off")
        if sdr_panel_visible(page):
            page.click("#btn-mode-sdr")  # toggle out
            page.wait_for_timeout(2000)
        post(url, {"cmd": "SET_FREQ", "center": 225e6, "span": 10e6})
        page.wait_for_timeout(2000)
        enter_sdr(page)
        page.wait_for_timeout(1500)
        d = state(url)
        check(
            "entering SDR follows the swept centre",
            abs(d["sdr"]["center"] - 225e6) < 5e6,
            f"centre {d['sdr']['center'] / 1e6:.3f} MHz",
        )

        # 5b - the audio toggle must follow the user's choice even with a stored value
        print("5b) audio toggle with a stored preference")
        store = page.evaluate("localStorage.getItem('web-sa-sdr-audio')")
        page.click("#btn-sdr-audio")
        page.wait_for_timeout(600)
        label = page.inner_text("#btn-sdr-audio")
        gate = page.evaluate("document.getElementById('spectrum')?.dataset.sdrAudio || ''")
        check(
            "toggle turns audio on",
            label == "On" and "enabled=true" in gate,
            f"label={label} gate={gate} (stored value before: {store})",
        )
        page.click("#btn-sdr-audio")
        page.wait_for_timeout(600)
        check(
            "toggle turns audio off again",
            page.inner_text("#btn-sdr-audio") == "Off"
            and "enabled=false" in page.evaluate("document.getElementById('spectrum')?.dataset.sdrAudio || ''"),
            f"label={page.inner_text('#btn-sdr-audio')}",
        )

        # 6 - mode round trip leaves no stuck pending state
        print("6) mode round trip")
        for target, expected in (("#btn-mode-rta", "rta"), ("#btn-mode-sdr", "sdr")):
            page.click(target)
            page.wait_for_timeout(2500)
            d = state(url)
            check(f"switched to {expected}", d["mode"] == expected, d["mode"])
        check(
            "mode buttons are usable again (no stuck pending)",
            not page.eval_on_selector("#btn-mode-sdr", "e => e.disabled"),
        )

        # 6b - RTA entry must actually render frames, not only flip the mode flag (the frame
        # counter is bumped when a decoded frame is handed to the renderer).
        print("6b) RTA entry renders frames")
        before_frames = page.evaluate(
            "Number(document.getElementById('spectrum').dataset.rtaFrames || 0)"
        )
        post(url, {"cmd": "SET_MODE", "mode": "rta"})
        page.wait_for_timeout(2000)
        after_frames = page.evaluate(
            "Number(document.getElementById('spectrum').dataset.rtaFrames || 0)"
        )
        check("RTA frames reach the renderer after entry", after_frames > before_frames,
              f"{before_frames} -> {after_frames}",)
        painted_rta = painted_pixels(page)
        check("the RTA view is actually drawn", painted_rta > 5000,
              f"{painted_rta} painted pixels")

        # 6c - the RTA centre unit buttons and the virtual keypad must reach the device
        # (the unit group is keyed `rta_center` while the input id is `input-rta-center`;
        # the mismatch made both silently do nothing)
        print("6c) RTA centre unit buttons and keypad")
        post(url, {"cmd": "SET_MODE", "mode": "rta"})
        page.wait_for_timeout(2000)
        post(url, {"cmd": "SET_RTA", "center": 1.0e9, "span": 10e6})
        page.wait_for_timeout(1500)
        before = state(url)["req"]["rta"]["center"]
        page.fill("#input-rta-center", "450")
        page.click("#unit-rta_center-group button:nth-child(1)")   # MHz
        page.wait_for_timeout(1800)
        after = state(url)["req"]["rta"]["center"]
        check("a unit click applies the RTA centre (no Set needed)",
              abs(after - 450e6) < 1e3, f"{before/1e6} -> {after/1e6} MHz")

        page.click("#btn-keypad")
        page.wait_for_timeout(300)
        page.click("#input-rta-center")
        page.wait_for_timeout(400)
        unit_keys = page.evaluate(
            """() => [...document.querySelectorAll('#keypad button')]
                     .map(b => b.textContent).filter(x => /MHz|GHz/.test(x))""")
        check("the keypad offers units for the RTA centre", unit_keys == ["MHz", "GHz"],
              f"unit keys: {unit_keys}")
        for digit in "460":
            page.click(f'#keypad button:text-is("{digit}")')
        page.click('#keypad button:text-is("OK")')
        page.wait_for_timeout(1800)
        after_ok = state(url)["req"]["rta"]["center"]
        check("the keypad OK applies the RTA centre", abs(after_ok - 460e6) < 1e3,
              f"{after_ok/1e6} MHz")
        page.click("#btn-keypad")
        page.wait_for_timeout(200)

        # 7 - SWP/RTA reference level (the refPending -> slot migration)
        print("7) SWP reference stepping")
        post(url, {"cmd": "SET_MODE", "mode": "std"})
        page.wait_for_timeout(2500)
        post(url, {"cmd": "SET_REF", "mode": "manual", "ref": -5})
        page.wait_for_timeout(2000)
        before = state(url)["ref"]
        # Step DOWN from a level that already shows the whole trace: raising Ref pushes the noise
        # floor below the bottom edge, where the safety ranger would (correctly) pull it back, and
        # lowering it below about -15 dBm would clip the fake backend's -25 dBm carrier. This check
        # is about the step arriving at the device, not about the placement rules (9c/9d).
        page.click("#btn-ref-down")
        page.wait_for_timeout(2000)
        after = state(url)["ref"]
        check("ref step reaches the backend", abs(after - before) >= 5,
              f"{before} -> {after} dBm")
        check(
            "the stepped value is rendered while pending",
            abs(float(page.input_value("#input-ref")) - after) < 1.5
            or page.evaluate("document.getElementById('input-ref').disabled") is True,
            f"input {page.input_value('#input-ref')} vs backend {after}",
        )

        # 7b - a measurement owns the display: the waterfall is turned off and disabled
        print("7b) waterfall is disabled while measuring")
        post(url, {"cmd": "SET_MODE", "mode": "std"})
        page.wait_for_timeout(2000)
        if not page.evaluate("document.getElementById('btn-waterfall').classList.contains('active')"):
            page.click("[data-action='toggle-waterfall']")
            page.wait_for_timeout(800)
        page.click("#btn-meas-onoff")
        page.wait_for_timeout(1200)
        check("the waterfall is turned off while measuring",
              not page.evaluate("document.getElementById('btn-waterfall').classList.contains('active')"),
              page.evaluate("document.getElementById('btn-waterfall').className"))
        check("the waterfall button is disabled while measuring",
              page.eval_on_selector("#btn-waterfall", "e => e.disabled") is True, "button enabled")
        page.click("#btn-meas-onoff")
        page.wait_for_timeout(1200)
        check("the user's waterfall choice comes back after the measurement",
              page.eval_on_selector("#btn-waterfall", "e => e.disabled") is False
              and page.evaluate("document.getElementById('btn-waterfall').classList.contains('active')"),
              page.evaluate("document.getElementById('btn-waterfall').className"))
        page.click("[data-action='toggle-waterfall']")   # leave it off for the next step
        page.wait_for_timeout(600)

        # 8 - rapid mode switching: each switch must take effect promptly and leave no stuck
        # pending state. This is the measurable form of "switching got slow / needs a second
        # click" - a request that blocks the next one until its timeout would fail here.
        print("8) rapid mode switching")
        for want, sel in (("rta", "#btn-mode-rta"), ("sdr", "#btn-mode-sdr"),
                          ("rta", "#btn-mode-rta"), ("sdr", "#btn-mode-sdr"),
                          ("rta", "#btn-mode-rta")):
            t0 = time.monotonic()
            page.click(sel)
            settled = False
            while time.monotonic() - t0 < 3.0:
                if state(url)["mode"] == want:
                    settled = True
                    break
                time.sleep(0.15)
            check(f"switch to {want} within 3 s", settled,
                  f"mode={state(url)['mode']} after {time.monotonic() - t0:.1f}s")
        check("mode buttons usable after the burst",
              not page.eval_on_selector("#btn-mode-sdr", "e => e.disabled"))

        # 8b - a newer mode request supersedes an in-flight one (discipline 2). The buttons
        # used to be disabled while a request was pending, so the second click was swallowed
        # and the first target won - the "switching is slow / needs a retry" report.
        print("8b) a newer mode request supersedes the old one")
        post(url, {"cmd": "SET_MODE", "mode": "std"})
        page.wait_for_timeout(2000)
        page.click("#btn-mode-rta")
        page.wait_for_timeout(50)          # deliberately do not wait for the confirmation
        page.click("#btn-mode-sdr")        # supersede before the first request settles
        page.wait_for_timeout(2500)
        check("the newest mode request wins", state(url)["mode"] == "sdr",
              state(url)["mode"])

        # 9 - SDR manual reference must stay put. It used to be undone within a second or two
        # (the display-mode defaults kept writing 0) while a tracking auto-scale fought it.
        print("9) SDR manual reference holds")
        if sdr_panel_visible(page) is False:
            page.click("#btn-mode-sdr")
            page.wait_for_timeout(2500)
        else:
            post(url, {"cmd": "SET_MODE", "mode": "sdr"})
            page.wait_for_timeout(2500)
        page.wait_for_timeout(1200)
        # A level the trace in front of us actually fits in: mid-window and clear of the peak. It
        # has to be derived from the measurement, because the fake backend's carrier is ~45 dB
        # stronger than what the bench shows without a source, and a level that leaves the trace
        # clipped or below the window is (correctly) corrected by the safety ranger.
        sdr_now = state(url)["auto_ref"]
        window = 100.0
        floor_now = sdr_now.get("last_noise_floor")
        peak_now = sdr_now.get("last_peak")
        good_ref = round(max((floor_now or -120.0) + window / 2, (peak_now or -60.0) + 10))
        good_ref = max(-50, min(30, good_ref))
        page.fill("#input-ref", str(good_ref))
        page.click("#btn-ref-set")
        page.wait_for_timeout(800)
        first = page.input_value("#input-ref")
        check("manual Ref is applied", abs(float(first) - good_ref) < 1.5,
              f"input {first} (asked {good_ref})")
        page.wait_for_timeout(3500)              # well past any auto-refresh window
        after = page.input_value("#input-ref")
        check("manual Ref survives (not reset to 0)", abs(float(after) - good_ref) < 1.5,
              f"input {after} after 3.5 s (asked {good_ref})")
        page.click("#btn-ref-down")
        page.wait_for_timeout(600)
        down = page.input_value("#input-ref")
        check("Ref down arrow works without pressing up first", float(down) < float(after),
              f"{after} -> {down}")

        # 9a2 - When the ranger does act (the protective direction), the correction must be
        # visible on screen. Reported: the hint named a new level while the canvas and the Ref box
        # kept the manual value, because the correction only reached the device.
        print("9a2) SDR: an automatic correction is visible, not just announced")
        if sdr_panel_visible(page) is False:
            page.click("#btn-mode-sdr")
            page.wait_for_timeout(2500)
        sdr_now = state(url)
        sdr_peak = sdr_now["auto_ref"].get("last_peak")
        clip_ref = None
        if sdr_peak is not None and sdr_peak - 15.0 >= -50.0:
            # 15 dB over the top edge: past the gross-clipping margin (10 dB), so raising Ref is
            # the protective action the ranger takes on its own.
            clip_ref = math.ceil((sdr_peak - 15.0) / 5.0) * 5.0
        if clip_ref is None:
            skip("an automatic correction is visible",
                 f"SDR peak {sdr_peak} cannot be clipped within the device Ref range on this bench")
        else:
            page.fill("#input-ref", str(clip_ref))
            page.click("#btn-ref-set")
            page.wait_for_timeout(3500)
            sdr_after = state(url)
            dbg_after = page.evaluate(
                "JSON.parse(document.getElementById('spectrum').dataset.sdrRefDbg || '{}')")
            check("the clipped manual level is corrected",
                  sdr_after["auto_ref"].get("result") in ("applied", "clipped", "overflow"),
                  f'asked {clip_ref}: {sdr_after["auto_ref"]}')
            check("the display follows the correction (the trace moves)",
                  dbg_after.get("applied") is True
                  and dbg_after.get("shown") != dbg_after.get("before"),
                  str(dbg_after))
            # `target` may legitimately be 0.0, which is falsy: compare only against a real number.
            target = sdr_after["auto_ref"].get("target")
            box = float(page.input_value("#input-ref"))
            check("the Ref box shows the corrected level, not the typed one",
                  isinstance(target, (int, float)) and abs(box - float(target)) < 1.5,
                  f'box {box} target {target}')

        # 9b - Auto Scale is an ACTION, not a mode: it must show that it is working, act once,
        # and never lock the reference. The old tracking mode gave no feedback for the ~1.9 s
        # the device needs and disabled the Ref controls while it was "on".
        print("9b) Auto Scale is a one-shot action with feedback")
        check("the reference is never locked (no tracking mode)",
              not page.eval_on_selector("#input-ref", "e => e.disabled")
              and not page.eval_on_selector("#btn-ref-down", "e => e.disabled"),
              "input or arrows disabled")
        # Read the glow in the same tick as the click: the backend can answer within milliseconds,
        # so a separate read would race the reply instead of testing the feedback.
        glowing = page.evaluate(
            """() => {
                 const b = document.getElementById('btn-ref-auto');
                 b.click();
                 return b.classList.contains('busy');
               }""")
        check("pressing Auto Scale shows that it is working", glowing, "no busy state")
        page.wait_for_timeout(2500)
        sdr_ref = state(url)["auto_ref"]
        dbg = page.evaluate(
            "JSON.parse(document.getElementById('spectrum').dataset.sdrRefDbg || '{}')")
        check("the SDR fit is a backend decision, like the other modes",
              sdr_ref.get("result") in ("applied", "ok", "no_signal", "no_data", "clipped"),
              str(sdr_ref))
        check("the display shows the level the fit decided",
              not dbg or abs(float(dbg.get("shown", 1e9)) - float(dbg.get("ref", -1e9))) < 3,
              str(dbg))
        check("the glow is gone once the fit landed",
              not page.eval_on_selector("#btn-ref-auto", "e => e.classList.contains('busy')"),
              str(sdr_ref))
        check("the reference is still usable afterwards",
              not page.eval_on_selector("#input-ref", "e => e.disabled"),
              "input disabled after a fit")

        # 9c - SWP: a trace that has left the window is fitted WITHOUT being asked. Measured
        # regression: Ref 0 dBm with an 80 dB window and everything at -108 dBm, the old loop
        # refused to move (its guard wanted a peak 15 dB above the noise floor) and left the
        # display empty for as long as the signal stayed away. A 20 dB window makes the whole
        # trace sit below the bottom edge, whatever the signal level is today.
        print("9c) SWP: raising Ref is the user's choice, gross clipping is not")
        post(url, {"cmd": "SET_MODE", "mode": "std"})
        page.wait_for_timeout(3000)
        # (a) Raising Ref pushes the noise floor below the bottom edge. Reported: pressing the up
        # arrow made Auto pull the trace back down. That is a display choice, so nothing may happen
        # on its own. The level is derived from the measurement so it holds on any signal level.
        measure = state(url)
        floor = measure["auto_ref"].get("last_noise_floor")
        window = 100.0
        raised_ref = max(-50.0, min(30.0, math.ceil((floor if floor is not None else -120.0)
                                                   + window + 10.0)))
        post(url, {"cmd": "SET_REF", "mode": "manual", "ref": raised_ref, "range_db": window})
        page.wait_for_timeout(4500)
        kept = state(url)
        check("a level the user raised is left alone",
              kept["ref"] == raised_ref,
              f'asked {raised_ref}, device {kept["ref"]} result {kept["auto_ref"].get("result")}')

        # (b) The protective direction still works: a grossly clipped peak (far above the top
        # edge) loses information, so the ranger raises Ref. -15 dB below the peak clips by 15 dB,
        # which is past the gross-clipping margin on both backends.
        clip = state(url)
        peak = clip["auto_ref"].get("last_peak")
        if peak is not None and peak - 15.0 >= -50.0:
            clip_ref = math.ceil((peak - 15.0) / 5.0) * 5.0
            post(url, {"cmd": "SET_REF", "mode": "manual", "ref": clip_ref, "range_db": 100})
            page.wait_for_timeout(4500)
            fixed = state(url)
            check("a grossly clipped trace is raised automatically",
                  fixed["ref"] > clip_ref and fixed["auto_ref"].get("result") == "clipped",
                  f'peak {peak:.0f}, asked {clip_ref}, device {fixed["ref"]} '
                  f'result {fixed["auto_ref"].get("result")}')
            floor_now = fixed["auto_ref"].get("last_noise_floor")
            check("the raised level keeps the trace inside the window",
                  floor_now is None or fixed["ref"] - 100.0 <= floor_now,
                  f'floor {floor_now} ref {fixed["ref"]}')
        else:
            skip("a grossly clipped trace is raised automatically",
                 f"peak {peak} cannot be clipped within the device Ref range on this bench")

        # 9d - Auto Scale itself: one decision per press, no pointless reconfiguration.
        print("9d) Auto Scale in SWP: a decision per press, and no needless reconfiguration")
        post(url, {"cmd": "SET_REF", "mode": "manual", "ref": 10, "range_db": 100})
        page.wait_for_timeout(3000)
        before = state(url)
        page.click("#btn-ref-auto")
        page.wait_for_timeout(3500)
        after = state(url)
        result = after["auto_ref"].get("result")
        check("pressing Auto Scale produces a decision (never silence)",
              result in ("applied", "ok", "no_signal", "no_data"), str(after["auto_ref"]))
        if result == "applied":
            check("the applied target is the level the device reports",
                  abs(float(after["ref"]) - float(after["auto_ref"]["target"])) < 0.01,
                  f'ref {after["ref"]} target {after["auto_ref"]["target"]}')
            check("the fit moved the reference once", after["ref"] != before["ref"],
                  f'{before["ref"]} -> {after["ref"]}')
        else:
            # 'ok' (already placed well), 'no_signal' (nothing to anchor to) and 'no_data' all
            # mean the level must be left alone.
            check(f"a '{result}' decision leaves the reference where it was",
                  after["ref"] == before["ref"], f'{before["ref"]} -> {after["ref"]}')
        check("the fitted value is the one that is rendered",
              abs(float(page.input_value("#input-ref")) - float(after["ref"])) < 1.5,
              f'input {page.input_value("#input-ref")} vs ref {after["ref"]}')
        check("the glow is gone once the fit landed",
              not page.eval_on_selector("#btn-ref-auto", "e => e.classList.contains('busy')"),
              str(after["auto_ref"]))

        # The second press must also produce a decision, and it must not churn: either the
        # placement is reported as already good (no reconfiguration at all), or the estimate
        # really moved and the new target differs from the previous one by at least the 5 dB
        # quantum (the front-end gain chain follows Ref on the bench, so the noise floor estimate
        # can legitimately move between two presses).
        version = after["config_version"]
        previous_target = after["auto_ref"].get("target")
        page.click("#btn-ref-auto")
        page.wait_for_timeout(3500)
        again = state(url)
        result2 = again["auto_ref"].get("result")
        if result2 == "ok":
            check("a settled placement is not reconfigured again",
                  again["config_version"] == version,
                  f'config_version {version} -> {again["config_version"]}')
        elif result2 in ("applied", "clipped", "below_window"):
            check("a moved estimate is followed with a real step, not churn",
                  previous_target is None
                  or abs(float(again["auto_ref"].get("target")) - float(previous_target)) >= 5.0,
                  f'target {previous_target} -> {again["auto_ref"].get("target")} '
                  f'config_version {version} -> {again["config_version"]}')
        else:
            check("a second press reports how the placement stands, not silence",
                  result2 in ("no_signal", "no_data"), str(again["auto_ref"]))

        check("no page errors", not errors, "; ".join(errors[:3]))
        browser.close()

    print()
    if FAILURES:
        print(f"FAILED ({len(FAILURES)}): " + ", ".join(FAILURES))
        return 1
    print("all state-machine checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
