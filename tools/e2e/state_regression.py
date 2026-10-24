#!/usr/bin/env python3
"""Exercise native-service tuning, mode, reload, overlay and reference-level contracts.

Usage: python3 tools/e2e/state_regression.py --url http://127.0.0.1:8080
Run against a running websad (fixture or hardware); failures produce a non-zero exit.
Signal-dependent hardware scenarios report SKIP when the required signal is absent.
"""
from __future__ import annotations

import argparse
import json
import math
import struct
import sys
import time
import urllib.request

from playwright.sync_api import Page, sync_playwright
from viewport_baseline import wait_ui_ready

FAILURES: list[str] = []

#: True when websad runs fixture input; set in main().
FAKE_BACKEND = False


def skip(name: str, reason: str) -> None:
    """Report a scenario whose required signal is absent on this bench."""
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
    """Count painted pixels to detect a blank spectrum canvas."""
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


#: The highlighted channel band and the listen marker, in canvas columns. Green excess (g - r) per
#: column, summed over three rows: the marker is the opaque `#00ffa0` line (by far the largest
#: excess), and the band is the 12%-alpha fill - the longest run that is tinted but not opaque.
OVERLAY_SCAN = """() => {
  const c = document.getElementById('spectrum');
  const g = c.getContext('2d');
  const ex = new Float64Array(c.width);
  for (const frac of [0.3, 0.5, 0.7]) {
    const d = g.getImageData(0, Math.round(c.height * frac), c.width, 1).data;
    for (let x = 0; x < c.width; x++) ex[x] += d[x*4+1] - d[x*4];
  }
  let marker = 0;
  for (let x = 1; x < c.width; x++) if (ex[x] > ex[marker]) marker = x;
  let best = null, run = null, gap = 0;
  for (let x = 0; x < c.width; x++) {
    if (ex[x] >= 6) { run = run ? [run[0], x] : [x, x]; gap = 0; }
    else if (run) { gap++; if (gap > 2) { if (!best || run[1]-run[0] > best[1]-best[0]) best = run; run = null; } }
  }
  if (run && (!best || run[1]-run[0] > best[1]-best[0])) best = run;
  return { marker, band: best };
}"""


def enter_sdr(page: Page) -> None:
    """#btn-mode-sdr is a toggle: only click when the SDR panel is not already shown."""
    if not sdr_panel_visible(page):
        page.click("#btn-mode-sdr")
    wait_ui_ready(page, "sdr")


def main() -> int:
    global FAKE_BACKEND
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="http://127.0.0.1:8080")
    args = ap.parse_args()
    url = args.url.rstrip("/")
    FAKE_BACKEND = str(state(url)["device"]).endswith("(FAKE)")
    print(f"backend: {'fake' if FAKE_BACKEND else 'device'} ({url})")

    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page()
        native_pan: dict = {}

        def receive_frame(payload) -> None:
            if not isinstance(payload, bytes) or payload[:4] not in (b"RTAF", b"RTA2"):
                return
            if len(payload) < 32:
                errors.append("truncated native RTAF header")
                return
            _, points, wf_len = struct.unpack_from("<IIH", payload, 4)
            base_size = 32 + 12 * points + 2 * wf_len
            if points < 2 or wf_len < 1 or len(payload) < base_size:
                errors.append("invalid native RTA spectrum length")
                return
            if payload[:4] == b"RTA2":
                if len(payload) < base_size + 8 or payload[base_size:base_size + 4] != b"TRIG":
                    errors.append("invalid RTA2 trigger trailer")
                    return
                metadata_size = struct.unpack_from("<I", payload, base_size + 4)[0]
                if len(payload) != base_size + 8 + metadata_size:
                    errors.append("invalid RTA2 trigger metadata length")
                    return
                try:
                    json.loads(payload[base_size + 8:])
                except (ValueError, UnicodeDecodeError):
                    errors.append("invalid RTA2 trigger metadata JSON")
                    return
            elif len(payload) != base_size:
                errors.append("invalid native RTAF length")
                return
            native_pan.update(
                lo=struct.unpack_from("<d", payload, 16)[0],
                hi=struct.unpack_from("<d", payload, base_size - 8)[0],
                freq=struct.unpack_from(f"<{points}d", payload, 24),
            )

        page.on("websocket", lambda ws: ws.on("framereceived", receive_frame)
                if "noaudio=1" in ws.url and "noiq=1" in ws.url else None)
        errors: list[str] = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.on(
            "console",
            lambda m: errors.append(f"console.{m.type}: {m.text}") if m.type == "error" else None,
        )

        page.goto(url, wait_until="domcontentloaded")
        wait_ui_ready(page)
        page.evaluate("localStorage.setItem('web-sa-mode','std')")
        page.reload(wait_until="domcontentloaded")
        wait_ui_ready(page, "std")

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

        # 1b - Display, hardware capture and native sample coverage are distinct.
        print("1b) SDR display / capture / native sample geometry")
        before_frames = page.get_attribute("#spectrum", "data-rta-frames")
        page.wait_for_function("""(before) => {
            const c = document.getElementById('spectrum');
            const w = JSON.parse(c.dataset.sdrWindow || '{}');
            return c.dataset.rtaFrames !== before && Math.abs((w.lo + w.hi) / 2 - 90.5e6) < 1;
        }""", arg=before_frames, timeout=30000)
        win = page.evaluate("JSON.parse(document.getElementById('spectrum').dataset.sdrWindow)")
        d = state(url)
        actual = d["sdr"].get("actual") or {}
        required = ("start", "stop", "center", "bandwidth", "capture_start", "capture_stop",
                    "capture_center", "capture_bandwidth", "capture_decimate", "iq_rate", "decimate")
        geometry_valid = all(isinstance(actual.get(k), (int, float))
                             and math.isfinite(actual[k]) for k in required)
        check("native SDR publishes display and capture geometry", geometry_valid, str(actual))
        if geometry_valid:
            lo, hi = actual["start"], actual["stop"]
            cap_lo, cap_hi = actual["capture_start"], actual["capture_stop"]
            check("display centre and width match the requested SDR view",
                  hi > lo and abs((lo + hi) / 2 - d["sdr"]["center"]) < 1
                  and abs(actual["center"] - d["sdr"]["center"]) < 1
                  and abs(hi - lo - actual["bandwidth"]) < 1
                  and actual["decimate"] > 0
                  and abs(actual["bandwidth"] - min(50e6 / actual["decimate"],
                                                   actual["capture_bandwidth"])) < 1,
                  f"display {lo}..{hi}, bandwidth {actual['bandwidth']}")
            check("capture centre and width match native IQ geometry",
                  cap_hi > cap_lo and abs((cap_lo + cap_hi) / 2 - actual["capture_center"]) < 1
                  and abs(cap_hi - cap_lo - actual["capture_bandwidth"]) < 1
                  and abs(actual["capture_bandwidth"] - 0.8 * actual["iq_rate"]) < 1
                  and actual["capture_decimate"] > 0
                  and abs(actual["iq_rate"] - 62.5e6 / actual["capture_decimate"]) < 1,
                  f"capture {cap_lo}..{cap_hi}, IQ rate {actual['iq_rate']}")
            check("the hardware capture contains the entire display",
                  cap_lo <= lo + 1 and cap_hi >= hi - 1,
                  f"capture {cap_lo}..{cap_hi}, display {lo}..{hi}")
            check("the canvas uses the native display window",
                  abs(win["lo"] - lo) < 1 and abs(win["hi"] - hi) < 1, str(win))
            freq = native_pan.get("freq", ())
            native_valid = (len(freq) >= 2 and all(math.isfinite(f) for f in freq)
                            and all(a < b for a, b in zip(freq, freq[1:], strict=False)))
            check("the native frame has finite increasing sample frequencies", native_valid)
            if native_valid:
                first_bin, last_bin = freq[1] - freq[0], freq[-1] - freq[-2]
                check("native samples cover the display to within one bin without invented edges",
                      abs(native_pan["lo"] - lo) < 1 and abs(native_pan["hi"] - hi) < 1
                      and -1e-6 <= freq[0] - lo < first_bin + 1e-6
                      and -1e-6 <= hi - freq[-1] < last_bin + 1e-6
                      and cap_lo <= freq[0] <= freq[-1] <= cap_hi,
                      f"samples {freq[0]}..{freq[-1]}, edge bins {first_bin}/{last_bin}")
                check("the renderer preserves the native sample endpoints",
                      abs(win["capLo"] - freq[0]) < 1e-6
                      and abs(win["capHi"] - freq[-1]) < 1e-6, str(win))

        # User zoom changes only the local view; the native windows stay intact.
        zoom_before = state(url)
        page.click('#btn-spectrum-zoom')
        page.locator('#zoom-overview').press('+')
        page.wait_for_function("!document.getElementById('btn-zoom-reset').disabled")
        check("user zoom narrows the displayed view",
              page.get_attribute('#btn-spectrum-zoom', 'aria-pressed') == 'true'
              and not page.is_disabled('#btn-zoom-reset'))
        zoom_after = state(url)
        check("user zoom preserves native tuning and both windows",
              zoom_before['sdr']['center'] == zoom_after['sdr']['center']
              and zoom_before['sdr']['decimate'] == zoom_after['sdr']['decimate']
              and all(zoom_before['sdr']['actual'][k] == zoom_after['sdr']['actual'][k]
                      for k in required), str(zoom_after['sdr']['actual']))
        page.click('#btn-zoom-reset')
        page.wait_for_function("document.getElementById('btn-zoom-reset').disabled")
        check("reset returns to the full window with zoom still enabled",
              page.get_attribute('#btn-spectrum-zoom', 'aria-pressed') == 'true')
        page.click('#btn-zoom-close')
        check("closing zoom returns to the ordinary spectrum",
              page.get_attribute('#btn-spectrum-zoom', 'aria-pressed') == 'false')

        # 1c - A saved SDR hardware level deliberately skips the automatic entry fit.
        # Exercise the user's explicit Auto action and require its own fresh decision.
        print("1c) SDR explicit automatic scale is applied")
        previous_seq = page.evaluate(
            "JSON.parse(document.getElementById('spectrum').dataset.sdrRefDbg || '{}').seq ?? -1"
        )
        page.click('[data-action="set-ref-auto"]')
        page.wait_for_function("""(previous) => {
          const d = JSON.parse(document.getElementById('spectrum').dataset.sdrRefDbg || '{}');
          return d.seq > previous && Number.isFinite(d.peak) && !document.getElementById('btn-ref-auto').classList.contains('busy');
        }""", arg=previous_seq, timeout=30000)
        dbg = page.evaluate(
            "JSON.parse(document.getElementById('spectrum').dataset.sdrRefDbg || '{}')"
        )
        if dbg and "peak" in dbg:
            check("explicit Auto completed a placement decision", dbg.get("result") in ("applied", "ok"), str(dbg))
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
        # The confirmed de-emphasis must survive the pending-slot TTL.
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

        # 2b - Published overlay edges follow each demodulator's receive band.
        print("2b) the channel overlay follows the demodulator's own band")
        page.select_option("#select-sdr-decimate", "2048")
        page.click('[data-action="apply-sdr-bw"]')
        # Hold IF bandwidth constant while changing demodulators.
        page.click('[data-sdr-ifbw="3000"]')
        page.click('[data-sdr-demod="usb"]')
        page.wait_for_timeout(2500)
        marker_move = []
        for listen_mhz in ("90.5", "90.501"):
            page.fill("#input-sdr-listen", listen_mhz)
            page.click('[data-action="apply-sdr-tune"]')
            page.wait_for_timeout(2200)
            marker_move.append(page.evaluate(OVERLAY_SCAN))
        check("the channel overlay is drawn", all(frame["band"] is not None for frame in marker_move))
        for mode in ("usb", "lsb", "cw", "ft8"):
            page.click(f'[data-sdr-demod="{mode}"]')
            page.wait_for_timeout(2500)
            sdr = state(url)["sdr"]
            primary = next((ch for ch in sdr.get("channels", []) if ch.get("channel") == 1), sdr)
            ifbw = float(primary["if_bw"])
            guard = min(300.0, max(100.0, ifbw * 0.1))
            pitch = max(100.0, float(primary["pitch"])) if mode == "cw" else 0
            expected = {"usb": (guard, ifbw), "lsb": (-ifbw, -guard),
                        "cw": (pitch - max(50.0, ifbw) / 2, pitch + max(50.0, ifbw) / 2),
                        "ft8": (100, 3000)}[mode]
            published = page.eval_on_selector("#spectrum", "e => e.dataset.demodBand || ''")
            edges = tuple(float(edge) for edge in published.split("..")) if published else ()
            check(f"{mode} overlay publishes its receive band",
                  len(edges) == 2 and all(abs(a - b) < 1 for a, b in zip(edges, expected, strict=True)),
                  f"dataset={published!r}, expected={expected}")
            drawn = page.evaluate(OVERLAY_SCAN)
            check(f"{mode} overlay remains visible", drawn["band"] is not None)
        page.click('[data-sdr-demod="nfm"]')
        page.click('[data-sdr-ifbw="12000"]')
        page.wait_for_timeout(1500)

        # 3 - Preset must discard the previous tuning request.
        print("3) Preset inside SDR")
        page.fill("#input-sdr-center", "101.7")
        page.click('[data-action="apply-sdr"]')
        page.wait_for_timeout(2000)
        page.click('[data-action="preset"]')
        wait_ui_ready(page, "sdr")
        page.wait_for_function("""async () => {
          const s = await fetch('/api/state').then(response => response.json());
          return s.mode === 'sdr' && Math.abs(s.sdr.center - 101.7e6) > 1e6
            && Math.abs(Number(document.getElementById('input-sdr-center').value) * 1e6 - s.sdr.center) < 1e4;
        }""")
        d = state(url)
        after = d["sdr"]["center"] / 1e6
        check(
            "no pre-Preset frequency is reapplied",
            abs(after - 101.7) > 1,
            f"centre after Preset {after:.4f} MHz",
        )
        check("Preset inside SDR keeps SDR active", sdr_panel_visible(page) and d["mode"] == "sdr", d["mode"])
        check("Preset preserves the confirmed SDR mode for reload",
              page.evaluate("localStorage.getItem('web-sa-mode')") == "sdr")

        # 4 - reload: controls must follow the backend, not the browser-restored form value
        print("4) reload")
        enter_sdr(page)
        page.fill("#input-sdr-center", "433")
        page.click('[data-action="apply-sdr"]')
        page.wait_for_function("""async () => {
          const s = await fetch('/api/state').then(response => response.json());
          return s.mode === 'sdr' && Math.abs(s.sdr.center - 433e6) < 1e4
            && Math.abs(Number(document.getElementById('input-sdr-center').value) - 433) < 0.0001;
        }""")
        page.reload(wait_until="domcontentloaded")
        wait_ui_ready(page, "sdr")
        d = state(url)
        shown = float(page.input_value("#input-sdr-center"))
        check(
            "controls match the backend after reload",
            abs(shown * 1e6 - d["sdr"]["center"]) < 1e4,
            f"input {shown:.4f} vs backend {d['sdr']['center'] / 1e6:.4f} MHz",
        )
        check("reload preserves the active SDR backend mode", d["mode"] == "sdr", d["mode"])

        # 5 - Plain mode return preserves SDR setup; Shift+click explicitly retunes.
        print("5) the user's SDR setup survives a mode round trip")
        if not sdr_panel_visible(page):
            enter_sdr(page)
            page.wait_for_timeout(1500)

        # The setup the user leaves in SDR: a frequency, a demod, an IF bandwidth and a volume.
        page.fill("#input-sdr-center", "100")
        page.click('[data-action="apply-sdr"]')
        page.wait_for_timeout(2200)
        page.click('[data-sdr-demod="usb"]')
        page.wait_for_timeout(900)
        page.click('[data-sdr-ifbw="2400"]')
        page.wait_for_timeout(900)
        page.evaluate("() => { const v = document.getElementById('input-sdr-volume'); v.value = '0.5';"
                      " v.dispatchEvent(new Event('change', {bubbles: true})); }")
        page.wait_for_timeout(1500)
        left = state(url)["sdr"]
        check("the user's SDR setup is applied",
              abs(left["center"] - 100e6) < 1e6 and left["demod"] == "usb"
              and abs(float(left["if_bw"]) - 2400) < 1 and abs(float(left["volume"]) - 0.5) < 0.01,
              f'centre {left["center"] / 1e6:.3f} MHz demod {left["demod"]} '
              f'if_bw {left["if_bw"]} volume {left["volume"]}')

        # Leave SDR, move the sweep somewhere else, come back with the plain mode button.
        page.click("#btn-mode-rta")
        page.wait_for_timeout(2500)
        post(url, {"cmd": "SET_FREQ", "center": 433e6, "span": 5e6})
        page.wait_for_timeout(2000)
        page.click("#btn-mode-sdr")
        page.wait_for_timeout(3000)
        back = state(url)
        check("a plain return keeps the user's frequency",
              abs(back["sdr"]["center"] - 100e6) < 1e6,
              f'centre {back["sdr"]["center"] / 1e6:.3f} MHz (swept centre '
              f'{back["req"]["swp"]["center"] / 1e6:.3f} MHz)')
        check("...and the rest of the listening setup",
              back["sdr"]["demod"] == "usb" and abs(float(back["sdr"]["if_bw"]) - 2400) < 1
              and abs(float(back["sdr"]["volume"]) - 0.5) < 0.01,
              f'demod {back["sdr"]["demod"]} if_bw {back["sdr"]["if_bw"]} '
              f'volume {back["sdr"]["volume"]}')

        # The explicit gesture still hands off: Shift+click on the swept view enters SDR there.
        page.click("#btn-mode-rta")
        page.wait_for_timeout(2500)
        post(url, {"cmd": "SET_FREQ", "center": 225e6, "span": 20e6})
        page.wait_for_timeout(2000)
        box = page.eval_on_selector("#spectrum", """el => {
            const r = el.getBoundingClientRect();
            return {x: r.left + r.width * 0.25, y: r.top + r.height * 0.5};
        }""")
        page.keyboard.down("Shift")
        page.mouse.click(box["x"], box["y"])
        page.keyboard.up("Shift")
        page.wait_for_timeout(3000)
        gesture = state(url)
        # Shift+click maps the RTA axis, independently of the SWP snapshot.
        axis = gesture["rta_actual"] or gesture["swp_actual"]
        expected = axis["start"] + 0.25 * (axis["stop"] - axis["start"])
        check("Shift+click hands that frequency to SDR",
              gesture["mode"] == "sdr" and abs(gesture["sdr"]["center"] - expected) < 2e6
              and abs(gesture["sdr"]["center"] - 100e6) > 5e6,
              f'centre {gesture["sdr"]["center"] / 1e6:.3f} MHz vs clicked '
              f'{expected / 1e6:.3f} MHz (axis {axis["start"] / 1e6:.1f}'
              f'..{axis["stop"] / 1e6:.1f} MHz)')

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
        # Leaving SDR must preserve the stored audio preference.
        page.click("#btn-mode-rta")
        page.wait_for_timeout(2500)
        page.click("#btn-mode-sdr")
        page.wait_for_timeout(3000)
        gate_back = page.evaluate("document.getElementById('spectrum')?.dataset.sdrAudio || ''")
        check(
            "audio on survives a mode round trip",
            page.inner_text("#btn-sdr-audio") == "On" and "enabled=true" in gate_back,
            f"label={page.inner_text('#btn-sdr-audio')} gate={gate_back}",
        )

        page.click("#btn-sdr-audio")
        page.wait_for_timeout(600)
        check(
            "toggle turns audio off again",
            page.inner_text("#btn-sdr-audio") == "Off"
            and "enabled=false" in page.evaluate("document.getElementById('spectrum')?.dataset.sdrAudio || ''"),
            f"label={page.inner_text('#btn-sdr-audio')}",
        )
        # ...and "off" is a preference too: it comes back as off.
        page.click("#btn-mode-rta")
        page.wait_for_timeout(2500)
        page.click("#btn-mode-sdr")
        page.wait_for_timeout(3000)
        check(
            "audio off survives a mode round trip",
            page.inner_text("#btn-sdr-audio") == "Off"
            and "enabled=false" in page.evaluate(
                "document.getElementById('spectrum')?.dataset.sdrAudio || ''"),
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

        # 6c - Both unit buttons and keypad apply the RTA centre.
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

        # 7 - SWP reference stepping.
        print("7) SWP reference stepping")
        post(url, {"cmd": "SET_MODE", "mode": "std"})
        page.wait_for_timeout(2500)
        post(url, {"cmd": "SET_REF", "mode": "manual", "ref": -5})
        page.wait_for_timeout(2000)
        before = state(url)["ref"]
        # Step down from a level that still contains the fixture carrier.
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

        # 8 - Mode requests must settle promptly and leave controls usable.
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

        # 8b - A newer request supersedes a pending mode change.
        print("8b) a newer mode request supersedes the old one")
        post(url, {"cmd": "SET_MODE", "mode": "std"})
        page.wait_for_timeout(2000)
        page.click("#btn-mode-rta")
        page.wait_for_timeout(50)          # deliberately do not wait for the confirmation
        page.click("#btn-mode-sdr")        # supersede before the first request settles
        page.wait_for_timeout(2500)
        check("the newest mode request wins", state(url)["mode"] == "sdr",
              state(url)["mode"])

        # 9 - The user's manual SDR reference must hold.
        print("9) SDR manual reference holds")
        if sdr_panel_visible(page) is False:
            page.click("#btn-mode-sdr")
            page.wait_for_timeout(2500)
        else:
            post(url, {"cmd": "SET_MODE", "mode": "sdr"})
            page.wait_for_timeout(2500)
        page.wait_for_timeout(1200)
        # Derive a usable reference from the measured signal and device limits.
        sdr_now = state(url)["auto_ref"]
        window = 100.0
        floor_now = sdr_now.get("last_noise_floor")
        peak_now = sdr_now.get("last_peak")
        caps = state(url)["caps"]
        good_ref = round(max((floor_now or -120.0) + window / 2, (peak_now or -60.0) + 10))
        good_ref = max(float(caps["ref_min"]), min(float(caps["ref_max"]), good_ref))
        page.fill("#input-ref", str(good_ref))
        page.click("#btn-ref-set")
        # A manual request must hold even if hardware reports IF overflow.
        page.wait_for_timeout(1500)
        settled = state(url)
        after = page.input_value("#input-ref")
        check("the Ref box shows the level the device reports",
              abs(float(after) - float(settled["ref"])) < 1.5,
              f"input {after} vs device {settled['ref']} (asked {good_ref})")
        check("manual Ref remains exactly the requested level without Auto",
              abs(float(settled["req"]["sdr"]["ref"]) - good_ref) < 0.01,
              f'asked {good_ref}, requested {settled["req"]["sdr"]["ref"]}, '
              f'result {settled["auto_ref"].get("result")}')
        page.wait_for_timeout(3500)              # well past any auto-refresh settle window
        held_state = state(url)
        held = held_state["ref"]
        check("manual requested Ref still holds after fresh acquisitions",
              abs(float(held_state["req"]["sdr"]["ref"]) - good_ref) < 0.01,
              f'asked {good_ref}, requested {held_state["req"]["sdr"]["ref"]} after 3.5 s')
        check("the hardware readback remains stable", abs(held - settled["ref"]) < 0.01,
              f'{settled["ref"]} -> {held} after 3.5 s')
        page.click("#btn-ref-down")
        page.wait_for_timeout(600)
        down = page.input_value("#input-ref")
        check("Ref down arrow works without pressing up first", float(down) < float(held),
              f"{held} -> {down}")

        # 9a2 - Manual clipping is feedback; only Auto requests a correction.
        print("9a2) SDR: a clipped level is left alone until asked, and the correction is visible")
        if sdr_panel_visible(page) is False:
            page.click("#btn-mode-sdr")
            page.wait_for_timeout(2500)
        sdr_now = state(url)
        sdr_peak = sdr_now["auto_ref"].get("last_peak")
        clip_ref = None
        if sdr_peak is not None and sdr_peak - 15.0 >= -50.0:
            # Put the peak 15 dB above the display, within the allowed Ref range.
            clip_ref = math.ceil((sdr_peak - 15.0) / 5.0) * 5.0
        if clip_ref is None:
            skip("a clipped level is left alone and Auto corrects it",
                 f"SDR peak {sdr_peak} cannot be clipped within the device Ref range on this bench")
        else:
            page.fill("#input-ref", str(clip_ref))
            page.click("#btn-ref-set")
            page.wait_for_timeout(3500)
            held_sdr = state(url)
            box_held = float(page.input_value("#input-ref"))
            check("a clipped manual level is left alone (it is feedback, not a fault)",
                  abs(held_sdr["ref"] - clip_ref) < 1.5 and abs(box_held - clip_ref) < 1.5,
                  f'asked {clip_ref}, device {held_sdr["ref"]} box {box_held} '
                  f'result {held_sdr["auto_ref"].get("result")}')
            # The Auto button corrects the level. The correction must reach the canvas and the Ref
            # box, and not the device only.
            page.click("#btn-ref-auto")
            page.wait_for_timeout(3500)
            sdr_after = state(url)
            dbg_after = page.evaluate(
                "JSON.parse(document.getElementById('spectrum').dataset.sdrRefDbg || '{}')")
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

        # 9b - Auto Scale acts once, exposes progress and leaves Ref editable.
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

        # 9c - Manual SWP Ref holds even when the trace leaves the display.
        print("9c) SWP: raising Ref is the user's choice, gross clipping is not")
        post(url, {"cmd": "SET_MODE", "mode": "std"})
        page.wait_for_timeout(3000)
        # (a) Raising Ref is a display choice, derived from the measured floor.
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

        # (b) A clipped manual trace holds until the user presses Auto.
        clip = state(url)
        peak = clip["auto_ref"].get("last_peak")
        if peak is not None and peak - 15.0 >= -50.0:
            clip_ref = math.ceil((peak - 15.0) / 5.0) * 5.0
            post(url, {"cmd": "SET_REF", "mode": "manual", "ref": clip_ref, "range_db": 100})
            page.wait_for_timeout(2500)
            held = state(url)["ref"]          # what the device accepted for that request
            page.wait_for_timeout(4000)       # observe the manual level over time
            fixed = state(url)
            check("a grossly clipped trace is not raised automatically",
                  fixed["ref"] == held,
                  f'peak {peak:.0f}, asked {clip_ref}, device {held} -> {fixed["ref"]} '
                  f'result {fixed["auto_ref"].get("result")}')
            page.click("#btn-ref-auto")
            page.wait_for_timeout(6000)
            fitted = state(url)
            check("Auto Scale still re-fits the clipped trace",
                  fitted["ref"] > held,
                  f'peak {peak:.0f}, clipped at {held}, after Auto {fitted["ref"]} '
                  f'result {fitted["auto_ref"].get("result")}')
        else:
            skip("a grossly clipped trace is not raised automatically",
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

        # A second fit either holds or moves by at least the device's 5 dB quantum.
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

        # 9e - Settings preserve manual Ref; Auto must still fit noise-only traces.
        print("9e) a settings change keeps the user's level, and Auto still fixes the trace")
        dev = state(url)
        window = float(dev.get("ref_range_db") or 100.0)
        floor = dev["auto_ref"].get("last_noise_floor")
        if floor is None:
            skip("a settings change keeps the user's level", "no noise-floor estimate yet")
        else:
            caps = dev["caps"]
            low = math.ceil((float(floor) + window + 5.0) / 5.0) * 5.0   # floor 5 dB under the edge
            low = max(float(caps["ref_min"]), min(float(caps["ref_max"]), low))
            post(url, {"cmd": "SET_REF", "mode": "manual", "ref": low, "range_db": window})
            page.wait_for_timeout(3000)
            # Change the displayed frequency range while preserving manual Ref.
            post(url, {"cmd": "SET_FREQ", "center": 20e6, "span": 1e6})
            page.wait_for_timeout(4000)
            kept = state(url)
            check("a settings change does not undo a level the user set",
                  abs(float(kept["ref"]) - low) < 1.5, f'asked {low}, device {kept["ref"]}')
            page.click("#btn-ref-auto")
            # Gain changes can move the trace between Ref steps; await the bounded fit.
            fixed = {}
            for _ in range(24):                     # up to 12 s
                fixed = state(url)
                if not fixed.get("auto_ref", {}).get("adjusting"):
                    break
                page.wait_for_timeout(500)
            fixed_floor = fixed["auto_ref"].get("last_noise_floor")
            fixed_peak = fixed["auto_ref"].get("last_peak")
            bottom, top = float(fixed["ref"]) - window, float(fixed["ref"])
            check("the fit never refuses: it reports applied or ok",
                  fixed["auto_ref"].get("result") in ("applied", "ok"),
                  str(fixed["auto_ref"]))
            check("Auto Scale puts the whole trace inside the window after the change",
                  (fixed_floor is None or fixed_floor >= bottom)
                  and (fixed_peak is None or fixed_peak <= top),
                  f'ref {fixed["ref"]} window [{bottom}, {top}] '
                  f'floor {fixed_floor} peak {fixed_peak}')

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
