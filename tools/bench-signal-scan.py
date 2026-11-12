#!/usr/bin/env python3
"""Opt-in live SDR scan CPU/RSS, capacity-race and recording continuity gate."""

import argparse
import contextlib
import json
import os
import pathlib
import sys
import time
import urllib.parse
import urllib.request
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--origin", default="http://127.0.0.1:8080")
parser.add_argument("--artifacts", type=pathlib.Path, required=True)
parser.add_argument("--seconds", type=float, default=10)
parser.add_argument("--warmup", type=float, default=3)
parser.add_argument("--mixed-audio", action="store_true", help="Exercise AM/NFM/USB/LSB audio and monitor every IQ/audio stream")
args = parser.parse_args()
if os.environ.get("WEBSA_SCAN_LIVE") != "1":
    parser.error("set WEBSA_SCAN_LIVE=1; this exercises a live SDR")
if args.artifacts.resolve().is_relative_to(pathlib.Path(__file__).resolve().parents[1]):
    parser.error("artifacts must be outside the checkout")
args.artifacts.mkdir(parents=True, exist_ok=True)


def api(path, body=None):
    request = urllib.request.Request(
        args.origin + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=15) as r:
        return json.load(r)


def receivers():
    return api("/api/sdr/receivers?detail=live")


def receiver_command(kind, **fields):
    state = receivers()
    r = api(
        "/api/sdr/receivers",
        dict(type=kind, service_id=state["service_id"], request_id=str(uuid.uuid4()), **fields),
    )
    assert not r.get("fault"), r.get("fault")
    return r["snapshot"]


def scan(kind, **fields):
    state = api("/api/sdr/scan")
    return api(
        "/api/sdr/scan",
        dict(
            type=kind,
            service_id=state["service_id"],
            scan_id=state["scan_id"],
            capture_epoch=state["capture"]["epoch"],
            expected_revision=state["revision"],
            request_id=str(uuid.uuid4()),
            **fields,
        ),
    )


def wait(condition, seconds=8):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = condition()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError("timed out waiting for live state")


def recording_index(recording, task):
    entries, volumes = [], []
    track = task["tracks"][0]
    for volume in track.get("volumes", []):
        offset = 0
        while True:
            query = urllib.parse.urlencode(dict(volume=volume["index"], offset=offset))
            page = api("/api/sdr/recordings/" + recording + "/index?" + query)
            assert int(page["offset"]) == offset
            assert int(page["next_offset"]) == offset + len(page["entries"])
            entries.extend(page["entries"])
            assert len(entries) <= 100000, "recording index exceeds bounded bench evidence"
            offset = int(page["next_offset"])
            if len(page["entries"]) < 128:
                break
        volumes.append(dict(index=volume["index"], blocks=offset))
    assert len(entries) == int(track.get("blocks", 0)), "recording index was truncated"
    samples = sum(int(entry["meta"]["clock"]["samples"]) for entry in entries)
    assert samples == sum(int(segment.get("samples", 0)) for segment in track["segments"]), "recording sample totals differ"
    return dict(entries=entries, volumes=volumes, samples=str(samples), complete=True)


initial = receivers()
assert initial["capture"]["state"] == "running"
known = {r["receiver_uid"] for r in initial["receivers"]}
primary = next(r for r in initial["receivers"] if r["display_number"] == 1)
assert len(known) == 1, (
    "Use an isolated 1-channel baseline; existing saved receivers are never removed."
)
# Read the actual process serving the port from the checkout-scoped launcher.
pids = []
for entry in pathlib.Path("/proc").iterdir():
    if not entry.name.isdigit():
        continue
    try:
        if (entry / "comm").read_text().strip() == "websad":
            pids.append(entry.name)
    except OSError:
        pass
assert len(pids) == 1, "Ambiguous websad process; run with one owned service"
pid = pids[0]


def sample():
    fields = pathlib.Path(f"/proc/{pid}/stat").read_text().split()
    rss = int(pathlib.Path(f"/proc/{pid}/statm").read_text().split()[1]) * os.sysconf(
        "SC_PAGE_SIZE"
    )
    return dict(
        time=time.monotonic(),
        cpu=(int(fields[13]) + int(fields[14])) / os.sysconf("SC_CLK_TCK"),
        bench_cpu=time.process_time(),
        rss=rss,
    )


def measure(label):
    start = sample()
    samples = []
    for _ in range(max(1, int(args.seconds * 5))):
        time.sleep(0.2)
        samples.append(sample())
    last = samples[-1]
    r = receivers()
    s = api("/api/sdr/scan")
    assert r["capture"]["state"] == "running", r["capture"]
    current = next(row for row in r["receivers"] if row["receiver_uid"] == primary["receiver_uid"])
    assert current["stream"] == primary["stream"], "primary IQ identity changed"
    assert (current.get("reception") or {}).get("interruptions", 0) == (
        primary.get("reception") or {}
    ).get("interruptions", 0), "primary processing was interrupted"
    state = api("/api/state")
    result = dict(
        label=label,
        seconds=last["time"] - start["time"],
        cpu_core_percent=100 * (last["cpu"] - start["cpu"]) / (last["time"] - start["time"]),
        bench_core_percent=100 * (last["bench_cpu"] - start["bench_cpu"]) / (last["time"] - start["time"]),
        rss_mean=sum(s["rss"] for s in samples) / len(samples),
        rss_peak=max(s["rss"] for s in samples),
        receivers=r,
        scan=s,
        iqs_gaps=state["actual"].get("iqs_stamp_gaps"),
        actual=state["actual"],
    )
    (args.artifacts / (label + ".json")).write_text(
        json.dumps(result, ensure_ascii=False, indent=2)
    )
    print(
        label,
        round(result["cpu_core_percent"], 2),
        round(result["rss_mean"] / 1048576, 2),
        s["statistics"],
        flush=True,
    )
    return result


center = initial["capture"]["center_hz"]
listen = primary["confirmed"]["baseband"]["frequency_hz"]


def config(index):
    hz = center - 800000 + index * 50000
    result = dict(
        name=f"Scan bench {index}",
        color="#6b9b73",
        tags=[bench_tag],
        enabled=True,
        purpose="iq",
        baseband=dict(frequency_hz=hz, passband_hz=8000, transition_hz=1000, minimum_rate_hz=24000),
        processors=[],
    )
    if args.mixed_audio:
        mode = ["am", "nfm", "usb", "lsb"][index % 4]
        result["purpose"] = "listen"
        result["processors"] = [dict(id=f"scan-bench-{index}", kind="demodulator",
                                     mode=mode, bandwidth_hz=2400 if mode in ["usb", "lsb"] else 6000,
                                     minimum_rate_hz=0, pcm_gain=0.7, agc=True, squelch_dbm=-120,
                                     nr=False)]
    return result


def stream_monitor():
    if not args.mixed_audio:
        return contextlib.nullcontext(None)
    sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent / "bench"))
    from eight_channel_streams import StreamMonitor

    return StreamMonitor(args.origin, receivers(), all_iq=True, include_broadcast_decode=True)


recordings = {}
results = {}
bench_tag = "scan-bench-" + str(uuid.uuid4())
known_tasks = {t["task_id"] for t in initial["recordings"]}
try:
    scan("pause")
    scan("cancel_region")
    for kind in ["iq", "audio"]:
        archive = receiver_command(
            "record_start", receiver_uid=primary["receiver_uid"], kinds=[kind]
        )
        recording = next(
            t["task_id"]
            for t in archive["recordings"]
            if t["task_id"] not in known_tasks
            and t["receiver_uid"] == primary["receiver_uid"]
            and t["state"] in ["preparing", "waiting", "recording"]
        )
        recordings[kind] = recording
        known_tasks.add(recording)
    for count in [1, 8, 16]:
        while len(receivers()["receivers"]) < count:
            receiver_command("create", config=config(len(receivers()["receivers"])))
        scan("pause")
        scan("cancel_region")
        with stream_monitor() as monitor:
            time.sleep(args.warmup)
            off = measure(f"{count}-scan-off")
            scan("start")
            time.sleep(args.warmup)
            on = measure(f"{count}-scan-on")
            if monitor is not None:
                streams = monitor.status()
                assert len(streams) == monitor.expected_connections and all(row["packets"] > 0 for row in streams.values()), streams
                (args.artifacts / f"{count}-stream-continuity.json").write_text(json.dumps(streams, indent=2))
        results[str(count)] = {
            "added_core_percent": on["cpu_core_percent"] - off["cpu_core_percent"],
            "added_rss_mib": (on["rss_mean"] - off["rss_mean"]) / 1048576,
            "observations": on["scan"]["statistics"]["observations"]
            - off["scan"]["statistics"]["observations"],
            "skipped": on["scan"]["statistics"]["skipped"] - off["scan"]["statistics"]["skipped"],
        }
    scan("pause")
    scan("identify_region", low_hz=center + 900000, high_hz=center + 910000, origin="manual")
    full = wait(
        lambda: (
            (
                s
                if any(
                    r["identify_state"] == "waiting"
                    for r in s["signals"]
                    if r["source"] == "region"
                )
                else None
            )
            if (s := api("/api/sdr/scan"))
            else None
        )
    )
    assert full["probe_slots_used"] == 0
    results["full_capacity_wait"] = True
    # At full capacity an existing narrowband source is still eligible for bypass.
    scan("identify_region", low_hz=listen - 2000, high_hz=listen + 2000, origin="manual")
    reused = wait(
        lambda: (
            (
                s
                if any(
                    r["identify_state"] == "complete"
                    for r in s["signals"]
                    if r["source"] == "region"
                )
                else None
            )
            if (s := api("/api/sdr/scan"))
            else None
        )
    )
    assert reused["probe_slots_used"] == 0
    results["full_capacity_iq_reuse"] = True
    # Free one definition, obtain a lease, then race a formal create against it.
    last = next(r for r in reversed(receivers()["receivers"]) if r["receiver_uid"] not in known)
    receiver_command("retire", receiver_uid=last["receiver_uid"])
    scan("identify_region", low_hz=center + 900000, high_hz=center + 910000, origin="manual")
    wait(lambda: api("/api/sdr/scan")["probe_slots_used"] == 1)
    receiver_command("create", config=config(15))
    race = api("/api/sdr/scan")
    assert race["probe_slots_used"] == 0
    assert len(receivers()["receivers"]) == 16
    results["formal_create_retires_probe"] = True
finally:
    primary_error = sys.exc_info()[1]
    cleanup_errors = []
    try:
        for operation in ["pause", "cancel_region"]:
            try:
                scan(operation)
            except Exception as error:
                cleanup_errors.append(f"{operation}: {error}")
        for kind, recording in recordings.items():
            try:
                receiver_command("record_stop", task_id=recording)
                finished = wait(
                    lambda recording=recording: next(
                        (
                            t
                            for t in receivers()["recordings"]
                            if t["task_id"] == recording and t["state"] in ["completed", "failed"]
                        ),
                        None,
                    ),
                    15,
                )
                index = recording_index(recording, finished) if kind == "iq" else None
                (args.artifacts / (kind + "-recording-continuity.json")).write_text(
                    json.dumps(dict(task=finished, index=index), ensure_ascii=False, indent=2)
                )
                segments = finished["tracks"][0]["segments"]
                continuous = bool(segments) and all(
                    a["stream"] == b["stream"]
                    and a.get("next_audio_index") == b.get("first_audio_index")
                    for a, b in zip(segments, segments[1:], strict=False)
                )
                if kind == "iq":
                    entries = index["entries"]
                    continuous = continuous and bool(entries) and all(
                        int(b["meta"]["clock"]["first_output_index"])
                        == int(a["meta"]["clock"]["first_output_index"]) + int(a["meta"]["clock"]["samples"])
                        for a, b in zip(entries, entries[1:], strict=False)
                    )
                results[kind + "_recording"] = {
                    "state": finished["state"],
                    "segments": len(segments),
                    "continuous": continuous,
                }
                assert finished["state"] == "completed" and continuous, results[kind + "_recording"]
            except Exception as error:
                cleanup_errors.append(f"{kind}: {error}")
            finally:
                try:
                    receiver_command("record_delete", task_id=recording)
                except Exception as error:
                    cleanup_errors.append(f"delete {kind}: {error}")
    finally:
        try:
            for r in reversed(receivers()["receivers"]):
                if bench_tag in r["confirmed"]["tags"]:
                    try:
                        receiver_command("retire", receiver_uid=r["receiver_uid"])
                    except Exception as error:
                        cleanup_errors.append(f"retire {r['receiver_uid']}: {error}")
        finally:
            if cleanup_errors:
                results["cleanup_errors"] = cleanup_errors
            (args.artifacts / "performance-summary.json").write_text(
                json.dumps(results, ensure_ascii=False, indent=2)
            )
    if cleanup_errors and primary_error is None:
        raise AssertionError(cleanup_errors)
print(json.dumps(results, ensure_ascii=False, indent=2))
