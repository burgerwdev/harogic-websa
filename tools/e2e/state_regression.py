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

Exit status is non-zero when any check fails, so it can be used in CI (the service has to
be running; the acquisition loop requires a WebSocket client, which this script is).
"""
from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.request

from playwright.sync_api import Page, sync_playwright

FAILURES: list[str] = []


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


def check(name: str, ok: bool, detail: str = "") -> None:
    print(f"  {'PASS' if ok else 'FAIL'}  {name}{('  <- ' + detail) if detail else ''}")
    if not ok:
        FAILURES.append(name)


def sdr_panel_visible(page: Page) -> bool:
    return page.is_visible("#sdr-settings")


def enter_sdr(page: Page) -> None:
    """#btn-mode-sdr is a toggle: only click when the SDR panel is not already shown."""
    if not sdr_panel_visible(page):
        page.click("#btn-mode-sdr")
        page.wait_for_timeout(2500)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:8080")
    args = ap.parse_args()
    url = args.url.rstrip("/")

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

        # 7 - SWP/RTA reference level (the refPending -> slot migration)
        print("7) SWP reference stepping")
        post(url, {"cmd": "SET_MODE", "mode": "std"})
        page.wait_for_timeout(2500)
        post(url, {"cmd": "SET_REF", "mode": "manual", "ref": -20})
        page.wait_for_timeout(2000)
        before = state(url)["ref"]
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
