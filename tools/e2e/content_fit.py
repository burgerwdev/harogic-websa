#!/usr/bin/env python3
"""Check dense controls with real browser minimum-font preferences, plus S toggling.

Uses an isolated native fixture and read-only synthetic scan rows. Run:
  .venv/bin/python tools/e2e/content_fit.py
"""

from __future__ import annotations

import argparse
import json
import urllib.request
from pathlib import Path

from playwright.sync_api import Error as PlaywrightError
from playwright.sync_api import expect, sync_playwright
from rust_iq_browser import free_port, start_service, stop
from themes import THEMES
from viewport_baseline import wait_ui_ready

AUDIT = r"""() => {
 const visible=e=>e.getBoundingClientRect().width>0 && e.getBoundingClientRect().height>0 && getComputedStyle(e).visibility!=='hidden';
 const issues=[]; const id=e=>e.tagName.toLowerCase()+(e.id?'#'+e.id:'.'+[...e.classList].join('.'))+(e.dataset.action?'['+e.dataset.action+']':'');
 for(const e of document.querySelectorAll('button,.unit-btn,.gh-toggle,.mc-channel-avatar,output,.group-title,.info-label,.info-value,.param-row>label,.mc-card-number,.mc-card-current,.mc-file-type,.mc-file-state')) {
  if(!visible(e)||e.closest('svg'))continue; const b=e.getBoundingClientRect();
  const walk=document.createTreeWalker(e,NodeFilter.SHOW_TEXT); let n;
  while(n=walk.nextNode()){
   if(!n.textContent.trim()||!visible(n.parentElement)||n.parentElement.closest('svg,option'))continue;
   const range=document.createRange();range.selectNodeContents(n);
   for(const r of range.getClientRects())if(r.width>0&&(r.left<b.left-.8||r.right>b.right+.8||r.top<b.top-.8||r.bottom>b.bottom+.8))issues.push({kind:'text',target:id(e),text:n.textContent.trim().slice(0,42),box:[b.width,b.height],glyph:[r.width,r.height],overflow:[b.left-r.left,r.right-b.right,b.top-r.top,r.bottom-b.bottom]});
  }
 }
 for(const icon of document.querySelectorAll('.mc-icon')){
  if(!visible(icon))continue;const parent=icon.closest('.mc-channel-avatar,button')||icon.parentElement;if(!visible(parent))continue;
  const r=icon.getBoundingClientRect(),b=parent.getBoundingClientRect();
  if(r.left<b.left-.8||r.right>b.right+.8||r.top<b.top-.8||r.bottom>b.bottom+.8)issues.push({kind:'icon',target:id(parent),box:[b.width,b.height],glyph:[r.width,r.height]});
 }
 return issues;
}"""


def scan_fixture(origin):
    with urllib.request.urlopen(origin + "/api/sdr/scan", timeout=10) as response:
        value = json.load(response)
    base = value["capture"]["center_hz"]
    value["signals"] = [
        dict(
            id=str(i),
            generation="1",
            center_hz=base + (i - 4) * 25000,
            low_hz=base + (i - 4) * 25000 - 4000,
            high_hz=base + (i - 4) * 25000 + 4000,
            bandwidth_hz=8000,
            excess_db=12.4,
            label="FM stereo pilot" if i % 2 else "unknown",
            verdict="credible",
            identify_state="complete",
            activity="active",
            suggested_mode="wfm" if i % 2 else None,
            protocol=None,
            receiver_available=True,
            decoder_available=False,
            receiver_uid=None,
            source="region",
            sampled_at_ms="1",
            flags=[],
            observed_hit_ratio=0.7,
        )
        for i in range(8)
    ]
    return value


def scan_route(snapshot, commands):
    def respond(route):
        if route.request.method == "GET":
            route.fulfill(json=snapshot)
        else:
            commands.append(route.request.post_data)
            route.fulfill(status=409, json={"error": "read-only layout fixture"})

    return respond


def check_window_focus(page):
    page.bring_to_front()
    scan = page.locator("#sdr-scan-window")
    expect(scan).to_be_focused()
    expect(scan).to_have_css("outline-style", "none")
    # Focusing the container must not suppress visible focus on its children.
    page.keyboard.press("Tab")
    slider = scan.locator(".window-opacity")
    expect(slider).to_be_focused()
    expect(slider).to_have_css("outline-style", "solid")
    page.keyboard.press("Tab")
    button = scan.locator("[data-collapse]")
    expect(button).to_be_focused()
    expect(button).to_have_css("outline-style", "solid")
    scan.focus()


def check_shortcut(page, commands):
    scan = page.locator("#sdr-scan-window")
    page.evaluate("document.activeElement?.blur()")
    page.keyboard.press("s")
    expect(scan).to_be_visible()
    check_window_focus(page)
    page.keyboard.down("s")
    page.keyboard.down("s")
    page.keyboard.up("s")
    expect(scan).not_to_be_visible()
    page.keyboard.press("s")
    expect(scan).to_be_visible()
    search = scan.locator("[data-scan-search]")
    search.fill("s")
    search.press("s")
    expect(scan).to_be_visible()
    search.fill("")
    scan.focus()
    for combo in ["Control+s", "Alt+s", "Shift+s"]:
        page.keyboard.press(combo)
        expect(scan).to_be_visible()
    page.keyboard.press("s")
    expect(scan).not_to_be_visible()
    page.keyboard.press("s")
    expect(scan).to_be_visible()
    with page.expect_popup() as opened:
        scan.locator("[data-pop]").click()
    popup = opened.value
    expect(popup.locator("#sdr-scan-window")).to_be_visible()
    popup.locator("#sdr-scan-window").focus()
    check_window_focus(popup)
    try:
        popup.keyboard.press("s")
    except PlaywrightError:
        # S closes this document on keydown, before Firefox sends keyup.
        if not popup.is_closed():
            raise
    expect(scan).not_to_be_visible()
    assert popup.is_closed()
    page.keyboard.press("s")
    expect(scan).to_be_visible()
    assert not commands, commands
    print(
        "PASS S shows/hides Scan, ignores editing/repeats/modifiers, and closes its popout without scan commands",
        flush=True,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--browser", choices=["firefox", "chromium"], default="firefox")
    parser.add_argument("--minimum-font", type=int, nargs="+", default=[0, 16, 20])
    parser.add_argument("--languages", nargs="+", default=["en", "zh"], choices=["en", "zh"])
    parser.add_argument("--artifacts", type=Path, default=Path("/tmp/websa-content-fit"))
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    server, origin = start_service(free_port())
    failures = []
    try:
        with sync_playwright() as p:
            for minimum in args.minimum_font:
                prefs = {
                    f"font.minimum-size.{x}": minimum
                    for x in ["x-western", "x-unicode", "zh-CN", "zh-TW"]
                }
                browser = (
                    p.firefox.launch(firefox_user_prefs=prefs)
                    if args.browser == "firefox"
                    else p.chromium.launch()
                )
                try:
                    for language in args.languages:
                        context = browser.new_context(viewport={"width": 1366, "height": 900})
                        try:
                            page = context.new_page()
                            errors = []
                            commands = []
                            page.on("pageerror", lambda error, errors=errors: errors.append(str(error)))
                            page.on(
                                "request",
                                lambda request, commands=commands: commands.append(request.post_data)
                                if request.method == "POST" and request.url.endswith("/api/sdr/scan")
                                else None,
                            )
                            page.add_init_script(
                                "localStorage.setItem('web-sa-ui-scale','1');localStorage.setItem('web-sa-lang',"
                                + json.dumps(language)
                                + ");"
                            )
                            page.goto(origin, wait_until="domcontentloaded")
                            wait_ui_ready(page)
                            if not page.locator("#btn-mode-sdr").evaluate(
                                "e => e.classList.contains('active')"
                            ):
                                page.locator("#btn-mode-sdr").click()
                            wait_ui_ready(page, "sdr")
                            snapshot = scan_fixture(origin)
                            # Check the real popout before installing the dense-row fixture:
                            # request interception stalls styles on Chromium's about:blank popup.
                            if minimum == args.minimum_font[0] and language == args.languages[0]:
                                check_shortcut(page, commands)
                                page.locator("#sdr-scan-window [data-close]").click()
                            page.route("**/api/sdr/scan", scan_route(snapshot, commands))
                            page.locator("#sdr-scan-toggle").click()
                            expect(page.locator("#sdr-scan-window tr[data-signal]")).to_have_count(
                                8
                            )
                            page.locator("[data-action=sdr-workbench]").click()
                            expect(page.locator(".mc-channel-avatar").first).to_be_visible()
                            page.locator(".mc-window [data-action=recording-files]").first.evaluate(
                                "e => e.click()"
                            )
                            expect(page.locator("[data-view-id=recording-library]")).to_be_visible()
                            page.locator("#btn-sdr-eq").evaluate("e => e.click()")
                            expect(page.locator(".sdr-eq-window")).to_be_visible()
                            page.evaluate("document.activeElement?.blur()")
                            page.keyboard.press("?")
                            expect(page.locator(".sdr-help-window")).to_be_visible()
                            for theme in THEMES:
                                # Floating windows can legitimately cover the footer; open the real
                                # modal through the trigger, then select through its native button.
                                page.locator("#btn-theme").evaluate("e => e.click()")
                                page.locator(f'[data-theme-choice="{theme}"]').click()
                                page.locator("#sdr-scan-window").focus()
                                check_window_focus(page)
                                assert page.locator(
                                    ".mc-body th button,.mc-body .mc-channel-select"
                                ).evaluate_all(
                                    "nodes => nodes.every(e => getComputedStyle(e,'::before').content === 'none')"
                                ), (theme, "plain controls inherited decorative pills")
                                issues = page.evaluate(AUDIT)
                                if issues:
                                    failures.append(
                                        dict(
                                            browser=args.browser,
                                            minimum=minimum,
                                            language=language,
                                            theme=theme,
                                            issues=issues,
                                        )
                                    )
                                # Narrow tables may scroll horizontally; headings must stay in
                                # their own cells when minimum fonts widen the labels.
                                page.locator("#sdr-scan-window").evaluate(
                                    "e => e.style.width = '520px'"
                                )
                                assert page.locator(".sdr-scan-table th button").evaluate_all("""nodes => nodes.every(e => {
                                  const a=e.getBoundingClientRect(), b=e.closest('th').getBoundingClientRect();
                                  return a.left>=b.left-.5 && a.right<=b.right+.5 && a.top>=b.top-.5 && a.bottom<=b.bottom+.5;
                                })"""), (minimum, language, theme, "header cell overflow")
                                issues = page.evaluate(AUDIT)
                                if issues:
                                    failures.append(
                                        dict(
                                            browser=args.browser,
                                            minimum=minimum,
                                            language=language,
                                            theme=theme,
                                            narrow=True,
                                            issues=issues,
                                        )
                                    )
                                page.locator("#sdr-scan-window").evaluate(
                                    "e => e.style.width = '720px'"
                                )
                            page.screenshot(
                                path=str(
                                    args.artifacts / f"{args.browser}-{minimum}-{language}.png"
                                )
                            )
                            page.locator("#btn-mode-rta").evaluate("e => e.click()")
                            wait_ui_ready(page, "rta")
                            for theme in THEMES:
                                page.locator("#btn-theme").evaluate("e => e.click()")
                                page.locator(f'[data-theme-choice="{theme}"]').click()
                                issues = page.evaluate(AUDIT)
                                if issues:
                                    failures.append(
                                        dict(
                                            browser=args.browser,
                                            minimum=minimum,
                                            language=language,
                                            theme=theme,
                                            mode="rta",
                                            issues=issues,
                                        )
                                    )
                            assert not errors, errors
                            assert not commands, commands
                            print(
                                f"CHECK {args.browser}: minimum font {minimum}px, {language}, all {len(THEMES)} themes, scan headers/avatars/buttons/icons",
                                flush=True,
                            )
                        finally:
                            context.unroute_all(behavior="wait")
                            context.close()
                finally:
                    browser.close()
    finally:
        stop(server)
        (args.artifacts / "overflow.json").write_text(json.dumps(failures, indent=2))
    assert not failures, f"{len(failures)} failing layouts; see {args.artifacts / 'overflow.json'}"
    print("PASS no text/icon overflow in the checked controls", flush=True)


if __name__ == "__main__":
    main()
