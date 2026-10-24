#!/usr/bin/env python3
"""Measure the running websad SDR path without retaining IQ payloads.

The service/RF lifecycle stays with tools/deploy and tools/pluto_tx.py. This
probe never opens the device, starts browsers, or changes recovery thresholds.
"""
from __future__ import annotations

import argparse
import json
import math
import struct
import threading
import time
import urllib.request
from pathlib import Path

from websockets.sync.client import connect

IQ_HEADER = struct.Struct("<4sIIIddIIQd")

# /api/state is a 1 Hz status cache; the producer snapshot is refreshed every
# 50 ms. 1.0 s cache age + 0.5 s for snapshot/framing/scheduling covers the
# observed skew without hiding a real clock mismatch.
STATUS_PERIOD = 1.0
SNAPSHOT_MARGIN = 0.5
INDEX_TOLERANCE = STATUS_PERIOD + SNAPSHOT_MARGIN


def registry_rows(rows):
    if not isinstance(rows, list) or not rows:
        raise ValueError("channel_registry unavailable or empty")
    keys = set()
    for row in rows:
        key = (row["epoch"], row["channel_id"])
        if key in keys or row.get("state") != "active":
            raise ValueError("duplicate/inactive registry identity")
        keys.add(key)
        if not all(math.isfinite(row[field]) for field in
                   ("frequency_hz", "output_rate", "input_rate", "group_delay_samples")):
            raise ValueError("nonfinite registry metadata")
        if row["output_rate"] <= 0 or row["input_rate"] < row["output_rate"]:
            raise ValueError("invalid registry rates")
        hop = row["input_rate"] / row["output_rate"]
        if not hop.is_integer() or hop <= 0:
            raise ValueError("nonintegral registry input/output rate ratio")
    return {f"{row['epoch']}:{row['channel_id']}": row for row in rows}




def request(url, payload=None):
    headers = {}
    body = None if payload is None else json.dumps(payload).encode()
    if body is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=body, headers=headers)
    with urllib.request.urlopen(req, timeout=15) as response:
        return json.load(response)


class Receiver(threading.Thread):
    def __init__(self, url, registry):
        super().__init__(daemon=True)
        self.socket = connect(url, max_size=16 << 20, max_queue=16)
        self.halt = threading.Event()
        self.lock = threading.Lock()
        self.streams = {}
        self.registry = registry
        self.spectra = 0
        self.errors = []

    def error(self, text):
        if len(self.errors) < 32:
            self.errors.append(text)

    def run(self):
        try:
            while not self.halt.is_set():
                try:
                    frame = self.socket.recv(timeout=0.2)
                except TimeoutError:
                    continue
                if not isinstance(frame, bytes):
                    continue
                with self.lock:
                    if frame[:4] in (b"RTAF", b"RTA2"):
                        self.spectra += 1
                        continue
                    if frame[:4] not in (b"IQBF", b"IQAX"):
                        continue
                    if len(frame) < IQ_HEADER.size:
                        self.error("short IQ header")
                        continue
                    magic, version, seq, count, rate, center, channel_id, epoch, index, delay = IQ_HEADER.unpack_from(frame)
                    if version != 2 or len(frame) != IQ_HEADER.size + count * 8:
                        self.error(f"invalid IQ frame: v={version}, bytes={len(frame)}")
                        continue
                    if not all(math.isfinite(value) for value in (rate, center, delay)) or rate <= 0 \
                            or delay < 0 or not channel_id:
                        self.error("invalid IQ metadata")
                        continue
                    if index == 0xFFFFFFFFFFFFFFFF:
                        self.error("native input index unavailable")
                    if (seq == 0) != (count == 0):
                        self.error("invalid empty identity marker")
                    key = f"{epoch}:{channel_id}"
                    registered = self.registry.get(key)
                    if registered is None:
                        self.error(f"{key}: wire identity absent from initial registry")
                        continue
                    expected_magic = "IQBF" if registered["channel"] == 1 else "IQAX"
                    if (magic.decode(), rate, center, delay) != (
                            expected_magic, registered["output_rate"],
                            registered["frequency_hz"], registered["group_delay_samples"]):
                        self.error(f"{key}: wire metadata/slot differs from registry")
                    row = self.streams.get(key)
                    if row is None:
                        # Every initial registry identity may join in the middle of a stream.
                        row = {"channel_id": channel_id, "epoch": epoch, "magic": magic.decode(),
                               "rate": rate, "center": center, "delay": delay,
                               "hop": int(registered["input_rate"] / registered["output_rate"]),
                               "frames": 0, "samples": 0, "gaps": 0, "last_seq": None,
                               "first_index": None, "last_index": None, "last_count": 0}
                        self.streams[key] = row
                    elif seq != (row["last_seq"] + 1) & 0xFFFFFFFF:
                        row["gaps"] += 1
                        self.error(f"{key}: sequence gap {row['last_seq']} -> {seq}")
                    if (row["magic"], row["rate"], row["center"], row["delay"]) != (magic.decode(), rate, center, delay):
                        self.error(f"{key}: metadata changed within identity")
                    if count:
                        if row["last_index"] is not None:
                            if index != row["last_index"] + row["last_count"] * row["hop"]:
                                self.error(f"{key}: noncontiguous causal input index")
                        else:
                            row["first_index"] = index
                        row["last_index"], row["last_count"] = index, count
                    row["last_seq"] = seq
                    row["frames"] += 1
                    row["samples"] += count
        except Exception as exc:
            if not self.halt.is_set():
                with self.lock:
                    self.error(repr(exc))

    def snapshot(self):
        with self.lock:
            return {"spectra": self.spectra,
                    "streams": {key: dict(row) for key, row in self.streams.items()},
                    "errors": list(self.errors)}

    def close(self):
        self.halt.set()
        self.socket.close()
        self.join(timeout=5)
        if self.is_alive():
            raise RuntimeError("receiver did not stop")


def sample(url, receivers, start, initial_registry):
    state = request(url + "/api/state")
    actual = state.get("sdr", {}).get("actual", {})
    if actual.get("channel_clock_error"):
        raise ValueError(actual["channel_clock_error"])
    rows = actual.get("channel_registry")
    if rows != state.get("channel_registry") or rows != state.get("actual", {}).get("channel_registry"):
        raise ValueError("STATUS channel_registry copies differ")
    if registry_rows(rows) != initial_registry:
        raise ValueError("channel registry changed during stable capture")
    clocks = actual.get("channel_clocks")
    if not isinstance(clocks, list):
        raise ValueError("STATUS channel_clocks unavailable")
    clock_keys = {(clock.get("epoch"), clock.get("channel_id")) for clock in clocks}
    expected_keys = {(row["epoch"], row["channel_id"]) for row in initial_registry.values()}
    if clock_keys != expected_keys or len(clocks) != len(expected_keys):
        raise ValueError("STATUS channel_clocks identities differ from registry")
    snapshots = [rx.snapshot() for rx in receivers]
    limit = actual.get("input_samples")
    skew = {}
    if limit is not None:
        for snapshot in snapshots:
            for key, wire in snapshot["streams"].items():
                if wire["last_index"] is None:
                    continue
                row = initial_registry.get(key)
                if row is None:
                    continue
                end = wire["last_index"] + wire["last_count"] * wire["hop"]
                skew[key] = (end - limit) / row["input_rate"]
    return {"seconds": time.monotonic() - start,
            "connected": state.get("connected"), "last_error": state.get("last_error"),
            "actual": actual, "skew_s": skew,
            "receivers": snapshots}


def evaluate(first, last, channels):
    problems = []
    before, after = first["actual"], last["actual"]
    ca, cb = before.get("iqs_capture", {}), after.get("iqs_capture", {})
    elapsed = last["seconds"] - first["seconds"]
    delta = {key: cb.get(key, 0) - ca.get(key, 0) for key in
             ("packets", "host_seconds", "device_seconds", "queue_full", "queue_wait_ms",
              "recoveries", "fail_calls", "skips", "spectrum_frames")}
    for key in ("recoveries", "fail_calls", "skips", "queue_full"):
        if delta[key]:
            problems.append(f"{key} increased by {delta[key]}")
    for key in ("iqs_stamp_gaps", "ddc_error_count"):
        diff = after.get(key, 0) - before.get(key, 0)
        delta[key] = diff
        if diff:
            problems.append(f"{key} increased by {diff}")
    if not last["connected"] or last["last_error"]:
        problems.append(f"service unhealthy: {last['last_error']!r}")
    def collect(snapshot):
        streams = {}
        for receiver in snapshot["receivers"]:
            problems.extend(receiver["errors"])
            for key, wire in receiver["streams"].items():
                if key in streams:
                    problems.append(f"{key}: identity delivered on multiple sockets")
                streams[key] = wire
        return streams

    prior_streams, wire_streams = collect(first), collect(last)
    streams, expected = [], registry_rows(last["actual"].get("channel_registry"))
    if expected != registry_rows(first["actual"].get("channel_registry")):
        problems.append("channel registry changed during stable capture")
    seen = set()
    for key, row in expected.items():
        wire = wire_streams.get(key)
        if wire is None:
            problems.append(f"{key}: registry identity has no wire stream")
            continue
        seen.add(key)
        prior = prior_streams.get(key, {"samples": 0, "frames": 0, "gaps": 0})
        diff = {name: wire[name] - prior[name] for name in ("samples", "frames", "gaps")}
        hop = row["input_rate"] / row["output_rate"]
        if not hop.is_integer() or hop <= 0:
            problems.append(f"{key}: nonintegral input/output rate ratio")
        streams.append({"key": key, "channel": row["channel"], "rate": wire["rate"],
                        "delay": wire["delay"], **diff})
        if diff["gaps"]:
            problems.append(f"{key}: {diff['gaps']} sequence gaps")
        if wire["rate"] != row["output_rate"] or wire["center"] != row["frequency_hz"] \
                or wire["delay"] != row["group_delay_samples"] \
                or wire["magic"] != ("IQBF" if row["channel"] == 1 else "IQAX"):
            problems.append(f"{key}: wire metadata differs from registry")
        # Framing boundaries contribute at most two partial 30-Hz frames.
        if abs(diff["samples"] - elapsed * wire["rate"]) > wire["rate"] * 0.15:
            problems.append(f"{key}: delivered sample rate outside framing tolerance")
    for key in wire_streams.keys() - seen:
        problems.append(f"{key}: wire identity absent from registry")
    clocks = last["actual"].get("channel_clocks")
    if not isinstance(clocks, list):
        problems.append("kernel channel clocks unavailable")
    else:
        clock_keys = {(clock.get("epoch"), clock.get("channel_id")) for clock in clocks}
        expected_keys = {(row["epoch"], row["channel_id"]) for row in expected.values()}
        if clock_keys != expected_keys or len(clocks) != len(expected_keys):
            problems.append("kernel clock identities differ from registry")
        for key, row in expected.items():
            if (row["epoch"], row["channel_id"]) not in clock_keys:
                problems.append(f"{key}: kernel clock identity unavailable")
                continue
            clock = next(clock for clock in clocks
                         if (clock.get("epoch"), clock.get("channel_id")) == (row["epoch"], row["channel_id"]))
            wire = wire_streams.get(key)
            if clock.get("channel") != row["channel"]:
                problems.append(f"{key}: kernel clock slot differs from registry")
            if wire is not None and wire["last_index"] is not None:
                # `/api/state` is served from a 1 Hz cache and the producer
                # snapshot refreshes every 50 ms; the receiver keeps draining
                # after the last HTTP poll. 1.5 s covers cache + snapshot +
                # framing/scheduling skew. Drained conservation is the internal
                # bench's job, not this live probe.
                limit = after.get("input_samples")
                if limit is None:
                    problems.append("native input sample clock unavailable")
                elif wire["last_index"] + wire["last_count"] * wire["hop"] > \
                        limit + row["input_rate"] * INDEX_TOLERANCE:
                    problems.append(f"{key}: wire input index exceeds native clock tolerance")
    if len(streams) != channels:
        problems.append(f"received {len(streams)} streams, expected {channels}")
    host = delta["host_seconds"]
    msps = delta["packets"] * 16240 / host / 1e6 if host > 0 else 0
    fps = delta["spectrum_frames"] / host if host > 0 else 0
    if not 19.5 <= fps <= 20.5:
        problems.append(f"SDK spectrum rate {fps:.3f} fps outside 20-fps tolerance")
    delivered_spectra = sum(new["spectra"] - old["spectra"] for old, new in
                            zip(first["receivers"], last["receivers"], strict=True))
    delivered_fps = delivered_spectra / elapsed if elapsed > 0 else 0
    if not 19.5 <= delivered_fps <= 20.5:
        problems.append(f"delivered spectrum rate {delivered_fps:.3f} fps outside tolerance")
    return {"seconds": elapsed, "capture_msps": msps, "spectrum_fps": fps,
            "delivered_spectrum_fps": delivered_fps,
            "delta": delta, "streams": streams, "problems": problems,
            "passed": not problems}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:8108")
    parser.add_argument("--seconds", type=float, default=600)
    parser.add_argument("--warmup", type=float, default=5)
    parser.add_argument("--center", type=float, default=411e6)
    parser.add_argument("--listen", type=float, default=411020000)
    parser.add_argument("--aux", type=float, action="append", default=[])
    parser.add_argument("--taps", type=int, choices=(4, 8, 16), default=16)
    parser.add_argument("--decimate", type=int, default=1)
    parser.add_argument("--ifbw", type=float, default=200000)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.seconds <= 0 or args.warmup < 0:
        parser.error("seconds must be positive and warmup nonnegative")
    # This probe targets the currently exposed service protocol. N-channel
    # internal pipeline verification is a separate executable scenario.
    if len(args.aux) > 2:
        parser.error("current SET_SDR_DDC protocol exposes only two aux slots")
    url = args.url.rstrip("/")
    def post(payload):
        return request(url + "/api/config", payload)
    post({"cmd": "SET_MODE", "mode": "sdr"})
    post({"cmd": "SET_SDR", "center": args.center, "decimate": args.decimate,
          "pfb_taps_per_phase": args.taps})
    post({"cmd": "SET_SDR_TUNE", "listen": args.listen})
    post({"cmd": "SET_SDR_DEMOD", "mode": "wfm", "ifbw": args.ifbw})
    for slot in range(2):
        post({"cmd": "SET_SDR_DDC", "channel": slot + 2, "enabled": slot < len(args.aux),
              "freq": args.aux[slot] if slot < len(args.aux) else 0})
    from urllib.parse import urlencode
    ws = url.replace("http://", "ws://").replace("https://", "wss://") + "/ws?"
    receivers = []
    result = {"config": {key: value for key, value in vars(args).items()
                          if key != "output"}, "samples": []}
    try:
        # The hardware pipeline starts only when a listener exists (the publisher's
        # listener gate). Subscribe display first, wait for the nonempty registry,
        # then build the IQ/aux receivers from that snapshot so no frame can
        # arrive before its identity is known.
        params = {}
        display = Receiver(ws + urlencode(params), {})
        receivers.append(display)
        display.start()
        deadline = time.monotonic() + 15
        while True:
            initial_state = request(url + "/api/state")
            try:
                initial_registry = registry_rows(
                    initial_state.get("sdr", {}).get("actual", {}).get("channel_registry"))
                break
            except ValueError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.2)
        display.registry = dict(initial_registry)
        for kind in ("iq", *(["iqaux"] if args.aux else [])):
            params = {}
            params[kind] = "1"
            rx = Receiver(ws + urlencode(params), dict(initial_registry))
            receivers.append(rx)
            rx.start()
        start = time.monotonic()
        while time.monotonic() - start < args.warmup:
            time.sleep(min(0.2, args.warmup))
        result["samples"].append(sample(url, receivers, start, initial_registry))
        print(json.dumps({"seconds": round(result["samples"][0]["seconds"], 2),
                          "skew_s": result["samples"][0]["skew_s"]}), flush=True)
        end = time.monotonic() + args.seconds
        while time.monotonic() < end:
            time.sleep(min(5, max(0, end - time.monotonic())))
            row = sample(url, receivers, start, initial_registry)
            result["samples"].append(row)
            cap = row["actual"].get("iqs_capture", {})
            print(json.dumps({"seconds": round(row["seconds"], 2),
                              "packets": cap.get("packets"),
                              "recoveries": cap.get("recoveries"),
                              "queue_full": cap.get("queue_full"),
                              "gaps": row["actual"].get("iqs_stamp_gaps"),
                              "skew_s": row["skew_s"]}), flush=True)
        result["verdict"] = evaluate(result["samples"][0], result["samples"][-1], 1 + len(args.aux))
    except Exception as exc:
        result["verdict"] = {"passed": False, "problems": [repr(exc)]}
    finally:
        for rx in receivers:
            try:
                rx.close()
            except Exception as exc:
                result.setdefault("verdict", {"passed": False, "problems": []})["problems"].append(repr(exc))
                result["verdict"]["passed"] = False
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result["verdict"], indent=2))
    return 0 if result["verdict"]["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
