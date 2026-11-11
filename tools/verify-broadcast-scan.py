#!/usr/bin/env python3
"""Opt-in SAN-90 AM (9–10 MHz) / broadcast FM (100–107 MHz) regression.

Requires real broadcasts in both bands and an isolated Current receiver. This
retunes the shared capture temporarily and restores its initial geometry. It
checks evidence and repetition, not calibrated RF detection/classification rates.
"""

import argparse
import json
import os
import pathlib
import time
import urllib.request
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--origin", default="http://127.0.0.1:8080")
parser.add_argument("--artifacts", type=pathlib.Path, required=True)
parser.add_argument("--seconds", type=float, default=18)
args = parser.parse_args()
if os.environ.get("WEBSA_SCAN_LIVE") != "1":
    parser.error("set WEBSA_SCAN_LIVE=1; this retunes a live SDR")
root = args.artifacts.resolve()
if root.is_relative_to(pathlib.Path(__file__).resolve().parents[1]):
    parser.error("artifacts must be outside the checkout")
root.mkdir(parents=True, exist_ok=True)
origin = args.origin


def api(path, body=None):
    with urllib.request.urlopen(
        urllib.request.Request(
            origin + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        ),
        timeout=15,
    ) as r:
        return json.load(r)


def command(kind, **fields):
    s = api("/api/sdr/scan")
    return api(
        "/api/sdr/scan",
        dict(
            type=kind,
            service_id=s["service_id"],
            request_id=str(uuid.uuid4()),
            capture_epoch=s["capture"]["epoch"],
            scan_id=s["scan_id"],
            expected_revision=s["revision"],
            **fields,
        ),
    )


def collect(name, seconds, region=False):
    start = time.monotonic()
    timeline = []
    while time.monotonic() - start < seconds:
        time.sleep(0.25)
        s = api("/api/sdr/scan")
        timeline.append(s)
        if region:
            rows = [r for r in s["signals"] if r["source"] == "region"]
            if rows and all(
                r["identify_state"]
                in [
                    "complete",
                    "unavailable",
                    "discontinuous",
                    "insufficient_iq",
                    "cancelled",
                    "budget_skipped",
                ]
                for r in rows
            ):
                break
    (root / (name + ".json")).write_text(json.dumps(timeline, ensure_ascii=False, indent=2))
    rows = [r for r in s["signals"] if not region or r["source"] == "region"]
    print(
        name,
        "epoch",
        s["capture"]["epoch"],
        "scale updates",
        s["statistics"].get("scale_updates"),
        "rows",
        len(rows),
        "complete",
        [
            (
                round(r["center_hz"] / 1e6, 6),
                round(r["bandwidth_hz"]),
                r["label"],
                r["identify_state"],
                r["activity"],
            )
            for r in rows
            if r["identify_state"] != "idle"
        ][:20],
        flush=True,
    )
    return s, rows


before = api("/api/state")
receivers = api("/api/sdr/receivers?detail=live")
report = {}
assert len(receivers["receivers"]) == 1, (
    "Use an isolated Current baseline; shared saved receivers must not be retuned"
)
assert receivers["capture"]["state"] == "running", "A healthy hardware capture is required"
try:
    for name, center, decimate, lo, hi, mode in [
        ("AM", 11e6, 8, 9e6, 10e6, "am"),
        ("FM", 103.5e6, 4, 100e6, 107e6, "wfm"),
    ]:
        command("pause")
        command("cancel_region")
        api("/api/config", dict(cmd="SET_SDR", center=center, listen=center, decimate=decimate))
        time.sleep(0.8)
        command("configure", range=dict(low_hz=lo, high_hz=hi), channels=[], auto_identify=True)
        command("start")
        s, rows = collect(name + "-连续扫描", args.seconds)
        known = [
            r
            for r in rows
            if r["identify_state"] == "complete"
            and r["suggested_mode"] == mode
            and r["activity"] != "stale"
        ]
        report[name] = {"discovered": len(rows), "classified": len(known)}
        command("pause")
        command("identify_region", low_hz=lo, high_hz=hi, origin="manual")
        region, regions = collect(name + "-多信号选区", args.seconds, True)
        report[name]["region_classified"] = sum(
            r["suggested_mode"] == mode and r["identify_state"] == "complete" for r in regions
        )
        assert all(r["activity"] != "stale" for r in regions), (
            "unchanged RF capture was invalidated"
        )
        selected = next((r for r in known if lo + 150000 < r["center_hz"] < hi - 150000), None)
        if selected:
            width = max(selected["bandwidth_hz"], 16000 if mode == "am" else 220000)
            hz = selected["center_hz"]
            details = []
            for attempt in range(3):
                command(
                    "identify_region",
                    low_hz=hz - width / 2,
                    high_hz=hz + width / 2,
                    origin="waterfall",
                )
                _, r = collect(name + "-重复选区-" + str(attempt + 1), 8, True)
                assert r and all(x["activity"] != "stale" for x in r), (
                    "region marked stale without a capture change"
                )
                assert any(
                    x["identify_state"] == "complete" and x["suggested_mode"] == mode for x in r
                ), "known broadcast identification did not complete"
                details.extend(api("/api/sdr/scan/signals/" + x["id"]) for x in r)
            (root / (name + "-特征依据.json")).write_text(
                json.dumps(details, ensure_ascii=False, indent=2)
            )
            report[name]["repeat_complete"] = 3
        assert len(known) >= 2, name + " continuous scan did not identify multiple broadcasts"
        assert report[name]["region_classified"] >= 2, (
            name + " multi-signal region did not identify multiple broadcasts"
        )
finally:
    command("pause")
    command("cancel_region")
    command("configure", range=None, channels=[], auto_identify=True)
    api(
        "/api/config",
        dict(
            cmd="SET_SDR",
            center=before["sdr"]["center"],
            listen=before["sdr"]["listen"],
            decimate=before["sdr"]["decimate"],
        ),
    )
    (root / "实机回归结果.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
print(json.dumps(report, ensure_ascii=False, indent=2))
