#!/usr/bin/env python3
"""Owned SAN-90 + TinySA CW validation; output is always switched off on exit.
Requires an up-to-date SDK shim and frontend/backend builds. No other service may own the radio.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

import serial
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/e2e"))
from native_multichannel import api  # noqa: E402
from native_multichannel_recording import finish as finish_recording  # noqa: E402
from native_multichannel_recording import read_archive  # noqa: E402
from native_multichannel_recording import start as start_recording  # noqa: E402
from rust_iq_browser import free_port, post, start_service, state, stop  # noqa: E402
from viewport_baseline import wait_ui_ready  # noqa: E402

HOOK = r"""() => {
 window.__fft={rows:[],paint:[],messages:0,bytes:0,version:0,lastAt:0,lastPaint:0};
 const Native=WebSocket;
 window.WebSocket=class extends Native { constructor(...args){super(...args);this.addEventListener('message',event=>{
  const b=event.data;if(!(b instanceof ArrayBuffer)||b.byteLength<24)return;
  const v=new DataView(b),magic=String.fromCharCode(...new Uint8Array(b,0,4));
  if(magic!=='RTAF'&&magic!=='RTA2'&&magic!=='RTA3')return;
  const a=window.__fft,now=performance.now(),n=v.getUint32(8,true),power=new Float32Array(b,24+n*8,n),f=new Float64Array(b,24,n);
  let peak=-Infinity,hz=0;const noise=[];
  for(let i=0;i<n;i++){if(power[i]>peak){peak=power[i];hz=f[i];} if(i%8===0&&Math.abs(f[i]-100050000)>2000&&Number.isFinite(power[i]))noise.push(power[i]);}
  noise.sort((a,b)=>a-b);a.messages++;a.bytes+=b.byteLength;a.lastAt=now;
  a.rows.push({t:now,n,peak,hz,floor:noise[Math.floor(noise.length*.3)]});
 });}};
 const put=CanvasRenderingContext2D.prototype.putImageData;
 CanvasRenderingContext2D.prototype.putImageData=function(...args){const a=window.__fft;
  if(this.canvas.id==='waterfall'&&a.messages>a.lastPaint){a.paint.push(performance.now()-a.lastAt);a.lastPaint=a.messages;}
  return put.apply(this,args);
 };
}"""


def cpu(pid):
    ticks = os.sysconf("SC_CLK_TCK")
    parts = Path(f"/proc/{pid}/stat").read_text().split()
    rss = int(Path(f"/proc/{pid}/status").read_text().split("VmRSS:")[1].split()[0]) / 1024
    return (int(parts[13]) + int(parts[14])) / ticks, rss


def quantile(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))] if xs else None


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--shim", type=Path, required=True)
    p.add_argument("--serial", required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--seconds", type=float, default=6)
    p.add_argument(
        "--sizes", type=int, nargs="+", default=[0, 32768, 65536], choices=[0, 32768, 65536]
    )
    p.add_argument("--display-decimates", type=int, nargs="+", default=[256, 1])
    p.add_argument("--floor-source", choices=["capture", "view"], default="capture")
    p.add_argument("--fill-seconds", type=float, default=0)
    args = p.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    process = None
    source = None
    log = (args.output / "service.log").open("w")
    report = {
        "source": "SAN-90 RF input from TinySA CW, 100.05 MHz, requested -42 dBm; uncalibrated cable loss",
        "floor_source": args.floor_source,
        "runs": [],
    }
    try:
        source = serial.Serial(args.serial, 115200, timeout=0.35, write_timeout=1)

        def command(text):
            source.reset_input_buffer()
            source.write((text + "\r\n").encode())
            source.flush()
            time.sleep(0.25)
            reply = source.read(16384).decode(errors="replace")
            if "usage:" in reply.lower() or "error" in reply.lower():
                raise RuntimeError((text, reply))
            return reply

        command("output off")
        report["tinysa"] = command("info")
        for text in [
            "level -42",
            "mode output",
            "output off",
            "level -42",
            "output normal",
            "output off",
            "level -42",
            "sweep cw 100050000",
        ]:
            command(text)
        report["source_sweep"] = command("sweep")
        command("output on")
        report["source_status"] = command("resume") + command("status")
        recordings = (args.output / "recordings").resolve()
        recordings.mkdir(exist_ok=True)
        os.environ["WEBSA_RECORDING_DIR"] = str(recordings)
        os.environ["WEBSA_RUST_IQS_SHIM"] = str(args.shim.resolve())
        process, origin = start_service(free_port(), hardware=True, log=log)
        post(origin, {"cmd": "SET_MODE", "mode": "sdr"})
        post(
            origin,
            {
                "cmd": "SET_SDR_CHANNEL",
                "channel": 1,
                "mode": "cw",
                "ifbw": 500,
                "freq": 100050000,
                "enabled": True,
                "volume": 0,
            },
        )
        post(origin, {"cmd": "SET_SDR", "center": 100e6, "listen": 100050000, "decimate": 256})
        with sync_playwright() as pw:
            browser = pw.chromium.launch(headless=True)
            page = browser.new_page(viewport={"width": 1600, "height": 1000})
            page.add_init_script("(" + HOOK + ")()")
            errors = []
            page.on("pageerror", lambda e: errors.append(str(e)))
            page.goto(origin)
            wait_ui_ready(page, "sdr")
            page.mouse.click(10, 10)
            monitor = browser.new_browser_cdp_session()
            heap = page.context.new_cdp_session(page)
            page.wait_for_function("window.__fft.rows.length>5", timeout=30000)
            # Keep measured physical Ref fixed across tiers. Browser startup may request an entry fit.
            fixed = state(origin)["sdr"]["actual"]["ref"]
            post(origin, {"cmd": "SET_REF", "ref": fixed})
            page.locator("#select-wf-floor-source").evaluate(
                '(e,value)=>{e.value=value;e.dispatchEvent(new Event("change",{bubbles:true}));}',
                args.floor_source,
            )
            for decimate in args.display_decimates:
                post(
                    origin,
                    {"cmd": "SET_SDR", "center": 100e6, "listen": 100050000, "decimate": decimate},
                )
                page.wait_for_timeout(1000)
                uid = api(origin)["receivers"][0]["receiver_uid"]
                recording_id = start_recording(origin, uid, f"fft-record-{decimate}")
                for size in args.sizes:
                    post(origin, {"cmd": "SET_SDR_FFT", "size": size})
                    page.wait_for_function(
                        's=>document.querySelector("#select-sdr-fft").value===String(s)',
                        arg=size,
                        timeout=10000,
                    )
                    page.wait_for_timeout((1 + args.fill_seconds) * 1000)
                    before = state(origin)
                    start_cpu, rss = cpu(process.pid)
                    chrome_start = {
                        p["id"]: p for p in monitor.send("SystemInfo.getProcessInfo")["processInfo"]
                    }
                    page.evaluate(
                        "()=>{window.__fft.rows=[];window.__fft.paint=[];window.__fft.bytes=0;}"
                    )
                    start = time.monotonic()
                    page.wait_for_timeout(args.seconds * 1000)
                    elapsed = time.monotonic() - start
                    end_cpu, rss_end = cpu(process.pid)
                    after = state(origin)
                    metrics = page.evaluate("window.__fft")
                    chrome_end = monitor.send("SystemInfo.getProcessInfo")["processInfo"]
                    chrome_cpu = (
                        sum(
                            p["cpuTime"]
                            - chrome_start.get(p["id"], {"cpuTime": p["cpuTime"]})["cpuTime"]
                            for p in chrome_end
                        )
                        / elapsed
                        * 100
                    )
                    chrome_rss = sum(
                        cpu(p["id"])[1]
                        for p in chrome_end
                        if Path(f"/proc/{p['id']}/stat").exists()
                    )
                    heap_bytes = heap.send("Runtime.getHeapUsage")
                    rows = metrics["rows"]
                    actual = after["sdr"]["actual"]
                    assert rows and actual["display_fft"]["size"] == size, (size, actual)
                    assert (
                        before["sdr"]["channels"][0]["channel_id"]
                        == after["sdr"]["channels"][0]["channel_id"]
                    )
                    run = {
                        "display_decimate": decimate,
                        "requested_fft": size,
                        "seconds": elapsed,
                        "frames": len(rows),
                        "fps": len(rows) / elapsed,
                        "chromium_cpu_percent_one_core": chrome_cpu,
                        "chromium_rss_mib_sum": chrome_rss,
                        "chromium_heap_bytes": heap_bytes,
                        "backend_cpu_percent_one_core": (end_cpu - start_cpu) / elapsed * 100,
                        "backend_rss_mib": rss_end,
                        "wire_mib_s": metrics["bytes"] / elapsed / 1024**2,
                        "frame_points": rows[-1]["n"],
                        "peak_dbm_median": quantile([r["peak"] for r in rows], 0.5),
                        "peak_hz_median": quantile([r["hz"] for r in rows], 0.5),
                        "floor_q30_dbm_median": quantile([r["floor"] for r in rows], 0.5),
                        "message_to_waterfall_paint_p95_ms": quantile(metrics["paint"], 0.95),
                        "actual": actual,
                        "receiver_before": before["sdr"]["actual"].get("receiver_workers"),
                        "receiver_after": actual.get("receiver_workers"),
                        "history": page.locator("#waterfall").evaluate("e=>e.dataset.history"),
                        "channel": after["sdr"]["channels"][0],
                        "capture_epoch": after["sdr"].get("capture_epoch"),
                    }
                    report["runs"].append(run)
                    page.screenshot(path=str(args.output / f"live-{decimate}-{size}.png"))
                    (args.output / "live.json").write_text(json.dumps(report, indent=2) + "\n")
                saved = finish_recording(origin, recording_id)
                indexed = []
                for volume in saved["tracks"][0]["volumes"]:
                    blocks, complete = read_archive(recordings / recording_id / volume["file"])
                    assert complete
                    indexed.extend(index for index, _ in blocks)
                identities = {
                    (i["meta"]["clock"]["epoch"], i["meta"]["clock"]["channel_id"]) for i in indexed
                }
                assert len(identities) == 1, identities
                report.setdefault("recordings", []).append(
                    {
                        "display_decimate": decimate,
                        "task": saved,
                        "validated_blocks": len(indexed),
                        "identities": list(identities),
                        "first_clock": indexed[0]["meta"]["clock"],
                        "last_clock": indexed[-1]["meta"]["clock"],
                    }
                )
            assert not errors, errors
            browser.close()
        print(
            json.dumps(
                [
                    {
                        k: v
                        for k, v in r.items()
                        if k
                        not in [
                            "actual",
                            "channel",
                            "receiver_before",
                            "receiver_after",
                            "chromium_heap_bytes",
                        ]
                    }
                    for r in report["runs"]
                ],
                indent=2,
            )
        )
    finally:
        original_error = sys.exc_info()[1]
        cleanup_errors = []

        def cleanup(name, action):
            try:
                action()
            except Exception as exc:
                cleanup_errors.append(f'{name}: {exc}')

        if source:
            def output_off():
                source.write(b"output off\r\n")
                source.flush()
                time.sleep(0.3)
                report["output_off_reply"] = source.read(4096).decode(errors="replace")

            cleanup('output off', output_off)
            cleanup('source close', source.close)
        cleanup('service stop', lambda: stop(process))
        cleanup('log close', log.close)
        if original_error is not None:
            report['error'] = str(original_error)
        report['cleanup_errors'] = cleanup_errors
        cleanup('report write', lambda: (args.output / "live.json").write_text(
            json.dumps(report, indent=2) + "\n"))
        if cleanup_errors:
            print('; '.join(cleanup_errors), file=sys.stderr)
            if original_error is None:
                raise RuntimeError('; '.join(cleanup_errors))


if __name__ == "__main__":
    main()
