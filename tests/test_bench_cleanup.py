"""Diagnostic failures must release all owned native handles and service resources."""
import ctypes as C
import json
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

from tools.bench import rust_am_rf_check as am
from tools.bench import sdr_display_live as display


@pytest.mark.parametrize("close_failure", [False, True])
def test_vendor_depth_releases_dsp_and_device_after_capture_failure(monkeypatch, close_failure):
    closed = []

    def close(name):
        def release(_handle):
            closed.append(name)
            if close_failure and name == "adm":
                raise OSError("fixture ADM close failure")
            return 0
        return release

    def capture(*_args):
        raise RuntimeError("fixture capture failure")

    fake = SimpleNamespace(
        bind_adm=lambda: None,
        open_device=lambda: tuple(C.c_void_p(value) for value in (1, 2, 3)),
        capture=capture,
        T=SimpleNamespace(dll=SimpleNamespace(
            ADM_Close=close("adm"), DSP_Close=close("dsp"), Device_Close=close("device"))))
    monkeypatch.setitem(sys.modules, "probe_adm", fake)
    with pytest.raises((RuntimeError, OSError)):
        am.vendor_depth(100e6)
    assert closed == ["adm", "dsp", "device"]


@pytest.mark.parametrize("fault", ["output_off", "source_close", "service_stop"])
def test_display_cleanup_failure_still_releases_service_and_writes_report(monkeypatch, tmp_path, fault):
    class Source:
        failed_body = False
        closed = False

        def reset_input_buffer(self):
            pass

        def write(self, _value):
            if self.failed_body and fault == "output_off":
                raise OSError("fixture output-off failure")

        def flush(self):
            pass

        def read(self, _count):
            return b"ok"

        def close(self):
            self.closed = True
            if fault == "source_close":
                raise OSError("fixture source-close failure")

    source = Source()
    process, logs, stopped = object(), [], []

    def start(*_args, **kwargs):
        logs.append(kwargs["log"])
        return process, "http://fixture"

    def fail_body(*_args, **_kwargs):
        source.failed_body = True
        raise RuntimeError("fixture measurement failure")

    def stop(child):
        stopped.append(child)
        if fault == "service_stop":
            raise OSError("fixture service-stop failure")

    monkeypatch.setattr(display.serial, "Serial", lambda *_a, **_k: source)
    monkeypatch.setattr(display, "start_service", start)
    monkeypatch.setattr(display, "post", fail_body)
    monkeypatch.setattr(display, "stop", stop)
    monkeypatch.setattr(display, "free_port", lambda: 19091)
    monkeypatch.setattr(display, "time", SimpleNamespace(sleep=lambda _seconds: None))
    monkeypatch.setenv("WEBSA_RECORDING_DIR", "fixture-original-recordings")
    monkeypatch.setenv("WEBSA_RUST_IQS_SHIM", "fixture-original-shim")
    monkeypatch.setattr(sys, "argv", ["sdr_display_live", "--shim", str(tmp_path / "shim.so"),
                                      "--serial", "fixture", "--output", str(tmp_path / "output")])
    error = None
    try:
        display.main()
    except Exception as exc:
        error = exc
    assert source.closed, "source close must run even when output-off fails"
    assert stopped == [process], "service stop must run after a serial failure"
    assert len(logs) == 1 and logs[0].closed
    report = json.loads((tmp_path / "output/live.json").read_text())
    assert "fixture" in json.dumps(report), "failed cleanup must be visible in the report"
    assert isinstance(error, RuntimeError) and str(error) == "fixture measurement failure"


@pytest.mark.parametrize("fault", ["display_close", "service_stop", "sample_cleanup", "report_write", "summary_print"])
def test_sustained_rf_cleanup_attempts_all_owners_and_preserves_failure(monkeypatch, tmp_path, capsys, fault):
    monkeypatch.syspath_prepend(str(Path(__file__).resolve().parents[1] / "tools/bench"))
    from tools.bench import multichannel_rf as rf

    calls = []
    process = SimpleNamespace(returncode=None, poll=lambda: process.returncode)
    output = tmp_path / "rf-run"
    shim = tmp_path / "shim.so"
    shim.touch()

    def release(name):
        calls.append(name)
        if name == "service_stop":
            process.returncode = 0
        if name == fault:
            raise OSError(f"fixture {fault} failure")

    def fail_measurement(*_args, **_kwargs):
        for name in ("calibration", "final-tones"):
            directory = output / name
            directory.mkdir()
            (directory / "tone-1.f32").write_bytes(b"owned fixture")
        raise RuntimeError("fixture RF measurement failure")

    def remove_samples(directory):
        calls.append(directory.name)
        release("sample_cleanup")

    original_save = rf.save

    def save(path, value):
        if path.name == "report.json" and fault == "report_write":
            raise OSError("fixture report_write failure")
        original_save(path, value)

    def summary_print(*args, **kwargs):
        if fault == "summary_print":
            raise BrokenPipeError("fixture summary_print failure")
        print(*args, **kwargs)

    monkeypatch.setattr(rf, "install_signal_handlers", lambda: None)
    monkeypatch.setattr(rf, "start_service", lambda *_a, **_k: (process, "http://fixture"))
    monkeypatch.setattr(rf, "DisplayConsumer", lambda _origin: SimpleNamespace(close=lambda: release("display_close")))
    monkeypatch.setattr(rf, "until", fail_measurement)
    monkeypatch.setattr(rf, "post", lambda *_a, **_k: {})
    monkeypatch.setattr(rf, "api", lambda *_a, **_k: {})
    monkeypatch.setattr(rf, "stop", lambda _process: release("service_stop"))
    monkeypatch.setattr(rf, "remove_tone_samples", remove_samples)
    monkeypatch.setattr(rf, "save", save)
    monkeypatch.setattr(rf, "print", summary_print, raising=False)
    monkeypatch.setenv("WEBSA_RECORDING_DIR", "fixture-original-recordings")
    monkeypatch.setenv("WEBSA_RUST_IQS_SHIM", "fixture-original-shim")
    monkeypatch.setattr(sys, "argv", ["multichannel_rf", "--shim", str(shim), "--artifacts", str(output)])
    error = None
    try:
        rf.main()
    except Exception as exc:
        error = exc
    assert "service_stop" in calls, "display close cannot strand the owned service"
    assert "calibration" in calls and "final-tones" in calls, "one cleanup failure cannot skip another directory"
    assert isinstance(error, RuntimeError) and str(error) == "fixture RF measurement failure"
    if fault == "report_write":
        diagnostic = capsys.readouterr().err
        assert "fixture RF measurement failure" in diagnostic
        assert "fixture report_write failure" in diagnostic
    else:
        report = json.loads((output / "report.json").read_text())
        assert report["passed"] is False and report["process_stopped"] is True
        assert any(fault in issue for issue in report["cleanup_errors"])
    assert rf.os.environ["WEBSA_RECORDING_DIR"] == "fixture-original-recordings"
    assert rf.os.environ["WEBSA_RUST_IQS_SHIM"] == "fixture-original-shim"
