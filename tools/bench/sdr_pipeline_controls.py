#!/usr/bin/env python3
"""Validate controls against an already-running hardware websad service.

No device/service/RF lifecycle or browser control. IQ payloads are counted and
released; receiver identity and boundary history are bounded. Native causal
input indices, registry identity, and replacement marker ordering are checked.
"""
from __future__ import annotations

import argparse
import copy
import json
import math
import struct
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import OrderedDict, deque
from pathlib import Path

from websockets.sync.client import connect

IQ_HEADER = struct.Struct("<4sIIIddIIQd")


def registry(state):
    rows = state.get("sdr", {}).get("actual", {}).get("channel_registry")
    if not isinstance(rows, list):
        raise ValueError("channel_registry unavailable")
    for copy_rows in (state.get("channel_registry"), state.get("actual", {}).get("channel_registry")):
        if copy_rows != rows:
            raise ValueError("STATUS registry copies disagree")
    slots, keys = set(), set()
    for row in rows:
        key = (row["epoch"], row["channel_id"])
        if row["channel"] in slots or key in keys or row.get("state") != "active":
            raise ValueError("invalid/duplicate registry identity or slot")
        slots.add(row["channel"])
        keys.add(key)
        if not all(math.isfinite(row[field]) for field in
                   ("frequency_hz", "output_rate", "input_rate", "group_delay_samples")):
            raise ValueError("nonfinite registry metadata")
        if row["output_rate"] <= 0 or row["input_rate"] < row["output_rate"] or row["group_delay_samples"] < 0:
            raise ValueError("invalid registry rate/delay")
    return {row["channel"]: row for row in rows}

def command_clock_identity(state):
    """A command reply must carry clocks from the same snapshot as its registry."""
    actual = state.get("sdr", {}).get("actual", {})
    rows, clocks = actual.get("channel_registry"), actual.get("channel_clocks")
    if not isinstance(rows, list) or not isinstance(clocks, list):
        raise ValueError("command reply lacks channel_registry/channel_clocks")
    keys = {(row["epoch"], row["channel_id"]) for row in rows}
    clock_keys = {(clock.get("epoch"), clock.get("channel_id")) for clock in clocks}
    if clock_keys != keys or len(clocks) != len(keys):
        raise ValueError(f"command reply clocks {sorted(clock_keys)} != registry {sorted(keys)}")
    for clock in clocks:
        key = (clock.get("epoch"), clock.get("channel_id"))
        row = next(row for row in rows if (row["epoch"], row["channel_id"]) == key)
        if clock.get("channel") != row["channel"]:
            raise ValueError("command reply clock slot differs from registry")



def identity(row):
    return f"{row['epoch']}:{row['channel_id']}"


class Receiver(threading.Thread):
    def __init__(self, url, kind, initial_registry=()):
        super().__init__(daemon=True)
        self.kind = kind
        self.socket = connect(url, max_size=16 << 20, max_queue=16, close_timeout=2)
        self.halt = threading.Event()
        self.lock = threading.Lock()
        self.frames = self.samples = self.spectra = self.epochs = self.gaps = 0
        self.last_seq = self.center = self.rate = None
        self.allowed = False
        self.headers = deque(maxlen=32)
        self.errors = deque(maxlen=32)
        self.error_count = 0
        self.streams = OrderedDict()
        self.registry = {identity(row): row for row in initial_registry}
        self.join_keys = set(self.registry)

    def error(self, text):
        self.error_count += 1
        self.errors.append(text)

    def arm(self, allow_epoch=False, target=None):
        with self.lock:
            self.allowed = allow_epoch

    def reconcile(self, rows):
        with self.lock:
            current = {identity(row): row for row in rows}
            for key, meta in current.items():
                stream = self.streams.get(key)
                if stream is None:
                    self.error(f"registry identity {key} has no wire output")
                    continue
                expected_magic = "IQBF" if meta["channel"] == 1 else "IQAX"
                if (stream["magic"], stream["center"], stream["rate"], stream["delay"]) != (
                        expected_magic, meta["frequency_hz"], meta["output_rate"], meta["group_delay_samples"]):
                    self.error(f"registry/wire metadata mismatch for {key}")
                hop = meta["input_rate"] / meta["output_rate"]
                stride = round(hop)
                if not math.isclose(hop, stride, rel_tol=1e-12) or (
                        stream["hop"] is not None and stream["hop"] != stride):
                    self.error(f"registry/wire input-index stride mismatch for {key}")
                stream["hop"] = stride
                stream["channel"] = meta["channel"]
                for old_key, old_meta in self.registry.items():
                    if old_meta["channel"] != meta["channel"] or old_key == key:
                        continue
                    old = self.streams.get(old_key)
                    marker = stream["marker_time"]
                    if marker is None:
                        self.error(f"replacement {key} missing seq0")
                    elif old is not None and old["last_time"] > marker:
                        self.error(f"old identity {old_key} followed replacement marker {key}")
                if stream["first_index"] is not None and stream["last_index"] != (
                        stream["first_index"] + (stream["samples"] - stream["last_count"]) * stride):
                    self.error(f"{key}: wire sample count disagrees with causal input span")
            for key, stream in self.streams.items():
                if key not in current and key not in self.registry and not stream.get("retired"):
                    self.error(f"wire identity {key} absent from registry")
            for key in self.registry.keys() - current.keys():
                if key in self.streams:
                    self.streams[key]["retired"] = True
            self.registry = current
            self.join_keys.clear()

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
                    expected = b"IQBF" if self.kind == "iq" else b"IQAX"
                    if magic != expected or version != 2 or len(frame) != IQ_HEADER.size + count * 8:
                        self.error("invalid IQ magic/version/count")
                        continue
                    if not all(math.isfinite(value) for value in (rate, center, delay)) or rate <= 0 or delay < 0 or not channel_id:
                        self.error("invalid IQ metadata")
                        continue
                    if index == 0xFFFFFFFFFFFFFFFF:
                        self.error("native input index unavailable")
                    if (seq == 0) != (count == 0):
                        self.error("invalid empty identity marker")
                    key = f"{epoch}:{channel_id}"
                    now = time.monotonic()
                    stream = self.streams.get(key)
                    if stream is not None and stream.get("retired"):
                        self.error(f"retired identity {key} emitted again")
                    if stream is None:
                        if seq != 0 and key not in self.join_keys:
                            self.error(f"new identity {key} missing seq0")
                        if seq == 0 and key not in self.join_keys and self.streams and not self.allowed:
                            self.error(f"unexpected replacement marker {key}")
                        if len(self.streams) == 32:
                            self.streams.popitem(last=False)
                        stream = {"channel_id": channel_id, "epoch": epoch, "magic": magic.decode(),
                                  "center": center, "rate": rate, "delay": delay, "hop": None,
                                  "frames": 0, "samples": 0, "gaps": 0, "last_seq": None,
                                  "last_index": None, "last_count": 0, "first_index": None,
                                  "marker_time": now if seq == 0 else None, "last_time": now}
                        meta = self.registry.get(key)
                        if meta is not None:
                            stream["hop"] = round(meta["input_rate"] / meta["output_rate"])
                        self.streams[key] = stream
                    elif seq != stream["last_seq"] + 1:
                        self.gaps += 1
                        stream["gaps"] += 1
                        self.error(f"{key}: sequence gap {stream['last_seq']} -> {seq}")
                    if (center, rate, delay) != (stream["center"], stream["rate"], stream["delay"]):
                        self.error(f"{key}: metadata changed within identity")
                    if seq == 0:
                        self.epochs += 1
                        self.headers.append({"time": now, "seq": seq, "count": count, "center": center,
                                             "rate": rate, "channel_id": channel_id, "epoch": epoch,
                                             "input_index": index, "group_delay_samples": delay})
                    if count:
                        if stream["last_index"] is not None:
                            step, remainder = divmod(index - stream["last_index"], stream["last_count"])
                            if remainder or step <= 0 or (stream["hop"] is not None and step != stream["hop"]):
                                self.error(f"{key}: noncontiguous causal input index")
                            stream["hop"] = step
                        else:
                            stream["first_index"] = index
                        stream["last_index"], stream["last_count"] = index, count
                    stream["last_seq"], stream["last_time"] = seq, now
                    stream["frames"] += 1
                    stream["samples"] += count
                    self.last_seq, self.center, self.rate = seq, center, rate
                    self.frames += 1
                    self.samples += count
        except Exception as exc:
            if not self.halt.is_set():
                with self.lock:
                    self.error(repr(exc))

    def snapshot(self):
        with self.lock:
            return {"kind": self.kind, "frames": self.frames, "samples": self.samples,
                    "spectra": self.spectra, "epochs": self.epochs, "gaps": self.gaps,
                    "last_seq": self.last_seq, "center": self.center, "rate": self.rate,
                    "error_count": self.error_count, "errors": list(self.errors),
                    "streams": {key: dict(row) for key, row in self.streams.items()},
                    "boundary_headers": list(self.headers)}

    def close(self):
        self.halt.set()
        self.socket.close()
        self.join(timeout=3)
        if self.is_alive():
            raise RuntimeError(f"{self.kind} receiver did not terminate")


class Probe:
    def __init__(self, args):
        self.args = args
        self.url = args.url.rstrip("/")
        self.receivers = {}
        self.report = {"config": {key: value for key, value in vars(args).items()
                                  if key != "output"},
                       "http": [], "stages": [], "problems": [],
                       "scope": "V2 identity, native causal indices, registry correspondence, and replacement ordering."}
        self.highest_epoch = -1

    def http(self, path, payload=None):
        headers = {}
        data = None
        if payload is not None:
            headers["Content-Type"] = "application/json"
            data = json.dumps(payload).encode()
        started = time.monotonic()
        request = urllib.request.Request(self.url + path, data=data, headers=headers)
        try:
            with urllib.request.urlopen(request, timeout=5) as response:
                status, body = response.status, response.read()
        except urllib.error.HTTPError as exc:
            status, body = exc.code, exc.read()
        except Exception as exc:
            self.report["http"].append({"path": path, "command": payload,
                                        "latency_ms": (time.monotonic() - started) * 1000,
                                        "error": repr(exc)})
            raise
        try:
            result = json.loads(body)
        except (ValueError, UnicodeError):
            result = {"body": body[:2048].decode(errors="replace")}
        self.report["http"].append({"path": path, "command": payload, "status": status,
                                    "latency_ms": (time.monotonic() - started) * 1000,
                                    "response": result if payload is not None else None})
        return status, result

    def state(self):
        status, result = self.http("/api/state")
        if status != 200:
            raise RuntimeError(f"state HTTP {status}: {result}")
        return result

    def command(self, payload, accepted=True):
        status, result = self.http("/api/config", payload)
        ok = 200 <= status < 300 and not (isinstance(result, dict) and
             (result.get("ok") is False or result.get("error")))
        if ok != accepted:
            raise RuntimeError(f"command expected accepted={accepted}, HTTP {status}: {result}")
        return status, result
    def open(self):
        rows = registry(self.state())
        ws = self.url.replace("http://", "ws://", 1).replace("https://", "wss://", 1) + "/ws?"
        for kind in ("display", "iq", "iqaux"):
            params = {}
            if kind != "display":
                params[kind] = "1"
            initial = [row for slot, row in rows.items() if (kind == "iq" and slot == 1) or (kind == "iqaux" and slot != 1)]
            rx = Receiver(ws + urllib.parse.urlencode(params), kind, initial)
            self.receivers[kind] = rx
            rx.start()

    def close(self):
        errors = []
        for rx in self.receivers.values():
            try:
                rx.close()
            except Exception as exc:
                errors.append(repr(exc))
        self.receivers.clear()
        if errors:
            raise RuntimeError("; ".join(errors))

    def rx(self):
        return {kind: receiver.snapshot() for kind, receiver in self.receivers.items()}

    def reconcile(self, sample):
        rows = registry(sample["state"])
        if rows:
            self.highest_epoch = max(self.highest_epoch, max(row["epoch"] for row in rows.values()))
        for kind, receiver in self.receivers.items():
            if kind != "display":
                receiver.reconcile([row for slot, row in rows.items()
                                    if (kind == "iq") == (slot == 1)])
        sample["rx"] = self.rx()
        return rows

    def sample(self):
        return {"state": self.state(), "rx": self.rx(), "time": time.monotonic()}

    @staticmethod
    def fingerprint(state):
        actual = state.get("sdr", {}).get("actual", {})
        return {"req": copy.deepcopy(state.get("req")), "mode": state.get("mode"),
                "channels": copy.deepcopy(state.get("sdr", {}).get("channels")),
                "actual": {key: actual.get(key) for key in
                           ("center", "iq_center", "decimate", "capture_decimate",
                            "pfb_taps_per_phase", "channel_registry")}}

    def counters(self, before, after, allow_restart=False):
        a = before.get("sdr", {}).get("actual", {})
        b = after.get("sdr", {}).get("actual", {})
        delta, errors = {}, []
        for key in ("recoveries", "fail_calls", "queue_full"):
            old = a.get("iqs_capture", {}).get(key)
            new = b.get("iqs_capture", {}).get(key)
            if new is None or (old is None and not allow_restart):
                errors.append(f"capture counter unavailable: {key}")
                continue
            diff = new if allow_restart else new - old
            delta[key] = diff
            if diff and key != "queue_full":
                errors.append(f"{key} changed: {old} -> {new}")
        for key in ("iqs_stamp_gaps", "ddc_error_count"):
            old, new = a.get(key), b.get(key)
            if new is None or (old is None and not allow_restart):
                errors.append(f"pipeline counter unavailable: {key}")
                continue
            diff = new if allow_restart else new - old
            delta[key] = diff
            if diff:
                errors.append(f"{key} changed: {old} -> {new}")
        return delta, errors

    def settle(self):
        deadline = time.monotonic() + self.args.settle
        while time.monotonic() < deadline:
            time.sleep(min(0.1, max(0, deadline - time.monotonic())))

    def stage(self, name, payload, *, epochs=(), target=None, rejected=False,
              primary=True, aux=False, restarted=False, capture_stopped=False, check_state=None,
              unchanged=False, target_from_state=False, preserve=(), replace=(), new_epoch=False,
              different_from=None, display_change=False):
        before = self.sample()
        old_registry = registry(before["state"])
        previous_epoch = self.highest_epoch
        for kind, rx in self.receivers.items():
            epoch_target = target if kind == "iq" else payload.get("freq") if kind == "iqaux" else None
            rx.arm(kind in epochs or (display_change and kind in ("iq", "iqaux")), epoch_target)
        row = {"name": name, "command": payload, "expected_accepted": not rejected,
               "before": before, "problems": []}
        self.report["stages"].append(row)
        _, reply = self.command(payload, not rejected)
        row["reply_clock_identity"] = None
        if not rejected:
            try:
                command_clock_identity(reply)
                if capture_stopped and reply.get("sdr", {}).get("actual", {}).get("channel_clocks"):
                    raise ValueError("SET_MODE std reply retained channel_clocks")
                row["reply_clock_identity"] = "match"
            except ValueError as exc:
                row["reply_clock_identity"] = repr(exc)
                row["problems"].append(repr(exc))
        self.settle()
        after = self.sample()
        new_registry = self.reconcile(after)
        if display_change:
            old_actual = before["state"].get("sdr", {}).get("actual", {})
            new_actual = after["state"].get("sdr", {}).get("actual", {})
            capture_changed = any(old_actual.get(key) != new_actual.get(key)
                                  for key in ("iq_rate", "capture_decimate", "iq_center"))
            if capture_changed:
                epochs = tuple(kind for kind in ("iq", "iqaux") if any(
                    (kind == "iq") == (slot == 1) for slot in new_registry))
                if any(slot not in new_registry or identity(meta) == identity(new_registry[slot])
                       for slot, meta in old_registry.items()):
                    row["problems"].append("capture geometry changed without replacing channel identities")
            else:
                preserve = tuple(old_registry)
        for slot in preserve:
            if old_registry.get(slot) != new_registry.get(slot):
                row["problems"].append(f"channel {slot}: surviving identity changed")
                continue
            if slot in new_registry:
                kind = "iq" if slot == 1 else "iqaux"
                key = identity(new_registry[slot])
                prior = before["rx"][kind]["streams"].get(key, {})
                current = after["rx"][kind]["streams"].get(key, {})
                if current.get("last_seq", -1) <= prior.get("last_seq", -1):
                    row["problems"].append(f"channel {slot}: surviving sequence did not advance")
        for slot in replace:
            old_meta, new_meta = old_registry.get(slot), new_registry.get(slot)
            if not old_meta or not new_meta or identity(old_meta) == identity(new_meta):
                row["problems"].append(f"channel {slot}: changed configuration retained identity")
        if new_epoch and (not new_registry or any(meta["epoch"] <= previous_epoch for meta in new_registry.values())):
            row["problems"].append(f"replan did not advance epoch beyond {previous_epoch}")
        if different_from is not None:
            slot, previous_key = different_from
            if slot not in new_registry or identity(new_registry[slot]) == previous_key:
                row["problems"].append(f"channel {slot}: re-add reused removed identity")
        if capture_stopped and new_registry:
            row["problems"].append("SET_MODE std left nonempty registry")
        if payload.get("cmd") == "SET_SDR_CHANNEL" and not rejected:
            slot = payload["channel"]
            if payload["enabled"]:
                if slot not in new_registry or new_registry[slot]["frequency_hz"] != payload["freq"]:
                    row["problems"].append(f"channel {slot}: enabled frequency absent from registry")
            elif slot in new_registry:
                row["problems"].append(f"channel {slot}: removed slot remains in registry")
        for slot, meta in new_registry.items():
            kind = "iq" if slot == 1 else "iqaux"
            key = identity(meta)
            current = after["rx"][kind]["streams"].get(key, {})
            prior = before["rx"][kind]["streams"].get(key, {})
            if current.get("samples", 0) <= prior.get("samples", 0):
                row["problems"].append(f"channel {slot}/{key}: no new samples")
        row["after"] = after
        if target_from_state:
            target = new_registry.get(1, {}).get("frequency_hz")
            row["expected_primary_center_from_state"] = target
            if not isinstance(target, (int, float)) or not math.isfinite(target):
                row["problems"].append("accepted capture change has no finite primary registry target")
                target = None
        if capture_stopped:
            row["capture_delta"], problems = {"unavailable": "capture intentionally stopped by SET_MODE std"}, []
        else:
            row["capture_delta"], problems = self.counters(before["state"], after["state"], restarted)
        row["problems"].extend(problems)
        for kind, current in after["rx"].items():
            old = before["rx"][kind]
            if current["error_count"] != old["error_count"] or current["gaps"] != old["gaps"]:
                row["problems"].append(f"{kind}: new receiver errors/gaps: {current['errors']}")
            epochs_seen = current["epochs"] - old["epochs"]
            expected_markers = sum(identity(meta) not in {identity(old_meta) for old_meta in old_registry.values()}
                                   for slot, meta in new_registry.items()
                                   if (kind == "iq" and slot == 1) or (kind == "iqaux" and slot != 1))
            if kind in epochs and (not expected_markers or epochs_seen != expected_markers):
                row["problems"].append(f"{kind}: expected {expected_markers} identity markers, observed {epochs_seen}")
            if kind not in epochs and epochs_seen:
                row["problems"].append(f"{kind}: unexpected epoch reset")
            if (kind == "iq" and primary) or (kind == "iqaux" and aux):
                if current["samples"] <= old["samples"]:
                    row["problems"].append(f"{kind}: no new samples")
            if kind == "display" and primary and current["spectra"] <= old["spectra"]:
                row["problems"].append("no new spectra")
            self.receivers[kind].arm(False)
        if target is not None and after["rx"]["iq"]["center"] != target:
            row["problems"].append("primary did not reach target center")
        if (rejected or unchanged) and self.fingerprint(before["state"]) != self.fingerprint(after["state"]):
            row["problems"].append("command required to preserve configuration mutated it")
        if check_state and not check_state(after["state"]):
            row["problems"].append("accepted command did not update expected state")
        self.report["problems"].extend(f"{name}: {problem}" for problem in row["problems"])

    def reconnect(self):
        before = self.sample()
        self.close()
        time.sleep(3)
        idle = self.state()
        self.open()
        self.settle()
        after = self.sample()
        self.reconcile(after)
        delta, problems = self.counters(before["state"], after["state"])
        idle_delta, idle_errors = self.counters(before["state"], idle)
        problems.extend(idle_errors)
        for kind in ("iq", "display"):
            current = after["rx"][kind]
            if not current["samples" if kind == "iq" else "spectra"]:
                problems.append(f"{kind}: no output after reconnect")
            if current["error_count"] or current["gaps"]:
                problems.append(f"{kind}: reconnect errors: {current['errors']}")
        self.report["stages"].append({"name": "no_subscriber_3s_reconnect", "before": before,
                                      "idle_state": idle, "after": after, "capture_delta": delta,
                                      "idle_delta": idle_delta, "problems": problems})
        self.report["problems"].extend(problems)

    @staticmethod
    def mixed_channels(state, expected):
        channels = state.get("sdr", {}).get("channels", [])
        rows = registry(state)
        if len(channels) != 3 or {row.get("channel") for row in channels} != {1, 2, 3}:
            return False
        for channel in channels:
            slot = channel["channel"]
            mode, bandwidth = expected[slot]
            meta = rows.get(slot, {})
            if (not channel.get("enabled") or channel.get("mode") != mode
                    or channel.get("if_bw") != bandwidth
                    or channel.get("freq") != meta.get("frequency_hz")
                    or channel.get("channel_id") != meta.get("channel_id")
                    or channel.get("epoch") != meta.get("epoch")
                    or channel.get("actual_iq_rate") != meta.get("output_rate")):
                return False
        rates = {slot: rows[slot]["output_rate"] for slot in expected}
        return rates[1] == rates[2] > rates[3] if expected[2][0] == "wfm" else rates[1] > rates[2] > rates[3]

    def run(self):
        c = self.args.center
        spacing = 62_500_000 / 128
        def channel(slot, freq, mode="am", bandwidth=6000, enabled=True):
            return {"cmd": "SET_SDR_CHANNEL", "channel": slot, "enabled": enabled,
                    "freq": freq, "mode": mode, "ifbw": bandwidth,
                    "volume": 0.8, "audio_rate": 48000}

        self.command({"cmd": "SET_MODE", "mode": "sdr"})
        for slot in (2, 3):
            self.command(channel(slot, c, enabled=False))
        self.command({"cmd": "SET_SDR", "center": c, "decimate": 32, "pfb_taps_per_phase": 8})
        self.command(channel(1, c))
        self.open()
        ready_deadline = time.monotonic() + 15
        while True:
            initial = self.sample()
            initial_actual = initial["state"].get("sdr", {}).get("actual", {})
            capture = initial_actual.get("iqs_capture", {})
            configured = all(initial_actual.get(key) == value for key, value in
                             (("center", c), ("decimate", 32), ("pfb_taps_per_phase", 8)))
            channels = initial["state"].get("sdr", {}).get("channels", [])
            configured = configured and len(channels) == 3 and all(
                row.get("enabled") == (row.get("channel") == 1) for row in channels)
            configured = configured and any(
                row.get("channel") == 1 and row.get("freq") == c and row.get("mode") == "am"
                and row.get("if_bw") == 6000 for row in channels)
            if configured and capture.get("packets", 0) and initial["rx"]["iq"]["samples"] and initial["rx"]["display"]["spectra"]:
                break
            if time.monotonic() >= ready_deadline:
                self.report["problems"].append("initial capture/output did not become ready within 15 seconds")
                break
            time.sleep(0.2)
        self.reconcile(initial)
        self.report["initial"] = initial
        for kind, current in self.report["initial"]["rx"].items():
            if current["error_count"] or current["gaps"]:
                self.report["problems"].append(f"initial {kind}: {current['errors']}")
            if kind == "iq" and not current["samples"]:
                self.report["problems"].append("initial primary has no samples")
            if kind == "display" and not current["spectra"]:
                self.report["problems"].append("initial display has no spectra")
        def actual(state):
            return state.get("sdr", {}).get("actual", {})

        self.stage("mode_sdr_idempotent", {"cmd": "SET_MODE", "mode": "sdr"}, unchanged=True)
        # At D32 these capture/display windows are disjoint. Stream checks use
        # only old -> seq0 -> new ordering; the eventual state supplies the
        # clamped listen target without assuming HTTP and WS arrival order.
        self.stage("capture_center_out", {"cmd": "SET_SDR", "center": c + 10_000_000},
                   epochs=("iq",), target_from_state=True,
                   check_state=lambda s: actual(s).get("iq_center") == c + 10_000_000)
        self.stage("capture_center_return", {"cmd": "SET_SDR", "center": c},
                   epochs=("iq",), target_from_state=True,
                   check_state=lambda s: actual(s).get("iq_center") == c)
        if self.report["stages"][-1]["after"]["rx"]["iq"]["center"] != c:
            self.stage("restore_tune_baseline", channel(1, c),
                       epochs=("iq",), target=c, replace=(1,))
        self.stage("same_bin_tune", channel(1, c + 10000),
                   epochs=("iq",), target=c + 10000, replace=(1,))
        self.stage("cross_bin_tune", channel(1, c + spacing),
                   epochs=("iq",), target=c + spacing, replace=(1,))
        self.stage("taps_8_to_16", {"cmd": "SET_SDR", "pfb_taps_per_phase": 16},
                   epochs=("iq",), target=c + spacing,
                   check_state=lambda s: actual(s).get("pfb_taps_per_phase") == 16)
        self.stage("taps_16_to_8", {"cmd": "SET_SDR", "pfb_taps_per_phase": 8},
                   epochs=("iq",), target=c + spacing,
                   check_state=lambda s: actual(s).get("pfb_taps_per_phase") == 8)
        self.stage("taps_8_to_16_return", {"cmd": "SET_SDR", "pfb_taps_per_phase": 16},
                   epochs=("iq",), target=c + spacing,
                   check_state=lambda s: actual(s).get("pfb_taps_per_phase") == 16)
        self.stage("display_32_to_1", {"cmd": "SET_SDR", "decimate": 1},
                   display_change=True, target=c + spacing,
                   check_state=lambda s: s.get("req", {}).get("sdr", {}).get("decimate") == 1)
        self.stage("display_1_to_32", {"cmd": "SET_SDR", "decimate": 32},
                   display_change=True, target=c + spacing,
                   check_state=lambda s: s.get("req", {}).get("sdr", {}).get("decimate") == 32)
        self.stage("display_32_to_1_return", {"cmd": "SET_SDR", "decimate": 1},
                   display_change=True, target=c + spacing,
                   check_state=lambda s: s.get("req", {}).get("sdr", {}).get("decimate") == 1)
        self.stage("primary_wfm180k", channel(1, c + spacing, "wfm", 180000),
                   epochs=("iq",), replace=(1,), target=c + spacing)
        self.stage("aux_enable_am6k", channel(2, c - spacing),
                   epochs=("iqaux",), aux=True, preserve=(1,))
        self.stage("aux3_enable_cw500", channel(3, c - 2 * spacing, "cw", 500),
                   epochs=("iqaux",), aux=True, preserve=(1, 2),
                   check_state=lambda s: self.mixed_channels(s, {
                       1: ("wfm", 180000), 2: ("am", 6000), 3: ("cw", 500)}))
        self.stage("aux2_retune", channel(2, c - spacing * 1.25),
                   epochs=("iqaux",), aux=True, preserve=(1, 3), replace=(2,))
        self.stage("aux2_mode_bandwidth_rate", channel(2, c - spacing * 1.25, "wfm", 180000),
                   epochs=("iqaux",), aux=True, preserve=(1, 3), replace=(2,),
                   check_state=lambda s: self.mixed_channels(s, {
                       1: ("wfm", 180000), 2: ("wfm", 180000), 3: ("cw", 500)}))
        removed_aux3 = identity(registry(self.state())[3])
        self.stage("aux3_remove_keeps_aux2", channel(3, c - 2 * spacing, "cw", 500, False),
                   preserve=(1, 2))
        self.stage("aux3_readd", channel(3, c - 2 * spacing, "cw", 500),
                   epochs=("iqaux",), aux=True, preserve=(1, 2),
                   different_from=(3, removed_aux3))
        self.stage("reject_out_of_band_aux", channel(2, c + 100_000_000),
                   rejected=True, unchanged=True, preserve=(1, 2, 3))
        self.stage("aux3_disable", channel(3, c - 2 * spacing, "cw", 500, False),
                   preserve=(1, 2))
        self.stage("aux_disable", channel(2, c - spacing * 1.25, "wfm", 180000, False),
                   preserve=(1,))
        self.stage("boundary_tune", channel(1, c + spacing * 0.49, "wfm", 180000),
                   epochs=("iq",), target=c + spacing * 0.49, replace=(1,))
        self.stage("reject_4tap_wideband_boundary", {"cmd": "SET_SDR", "pfb_taps_per_phase": 4},
                   rejected=True, target=c + spacing * 0.49)
        self.reconnect()
        self.stage("mode_std", {"cmd": "SET_MODE", "mode": "std"}, primary=False,
                   capture_stopped=True, check_state=lambda s: s.get("mode") == "std")
        self.stage("mode_sdr_restore", {"cmd": "SET_MODE", "mode": "sdr"},
                   epochs=("iq",), primary=True, restarted=True, new_epoch=True,
                   check_state=lambda s: s.get("mode") == "sdr")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default="http://127.0.0.1:8108")
    parser.add_argument("--center", type=float, default=411e6)
    parser.add_argument("--settle", type=float, default=1.5)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not math.isfinite(args.center) or not math.isfinite(args.settle) or not 0.5 <= args.settle <= 5:
        parser.error("center must be finite; settle must be 0.5..5 seconds")
    probe = Probe(args)
    try:
        probe.run()
    except Exception as exc:
        probe.report["problems"].append(repr(exc))
    finally:
        try:
            probe.close()
        except Exception as exc:
            probe.report["problems"].append(f"cleanup: {exc!r}")
        probe.report["passed"] = not probe.report["problems"]
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(probe.report, indent=2) + "\n")
    print(json.dumps({"passed": probe.report["passed"], "problems": probe.report["problems"]}, indent=2))
    return 0 if probe.report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
