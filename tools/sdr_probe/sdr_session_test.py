#!/usr/bin/env python3
"""Direct SdrSession test (no WebSA/publisher): isolates IQS+DDC+demod stability."""
from __future__ import annotations

import os
import sys
import time

import numpy as np

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, REPO)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from tinysa import TinySA  # noqa: E402

from web_sa.hardware.device import HarogicDevice  # noqa: E402
from web_sa.measurements.sdr import SdrSession  # noqa: E402


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else 'am'
    sa = TinySA('/dev/ttyACM0')
    try:
        sa.enter_low_output()
        if mode == 'am':
            sa.am(100.2e6, -25, 1000, 50)
        else:
            sa.fm(100.2e6, -25, 1000, 25000)
        print('tinySA', sa.cmd('sweep'), flush=True)
        dev = HarogicDevice()
        ok, err = dev.open()
        print('device open', ok, err, flush=True)
        s = dev.state
        s.sdr_center_hz = 100e6
        s.sdr_decimate = 32
        s.sdr_listen_hz = 100.2e6
        s.sdr_demod = mode
        s.sdr_if_bw = 6000.0 if mode == 'am' else 25000.0
        s.sdr_squelch = -120.0
        s.sdr_volume = 1.0
        s.sdr_agc = True
        dev.set_session(SdrSession(dev))
        dev.session.enter()
        audio = []
        rtaf = 0
        t0 = time.monotonic()
        while time.monotonic() - t0 < 3.0:
            frames, _ = dev.step()
            for f in frames:
                if f[:4] == b'AUDF':
                    audio.append(f[16:])
                elif f[:4] == b'RTAF':
                    rtaf += 1
        sess = dev.session
        print(f'packets_ok={sess._packets_ok} packets_err={sess._packets_err} '
              f'last_status={sess._last_status} rtaf={rtaf} audio_chunks={len(audio)}', flush=True)
        pcm = b''.join(audio)
        a = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0
        if len(a) > 256:
            w = np.hanning(len(a)); sp = np.abs(np.fft.rfft((a - a.mean()) * w))
            f = np.fft.rfftfreq(len(a), 1 / 48000)
            print(f'audio={len(a)/48000:.2f}s tone={f[int(np.argmax(sp))]:.1f} Hz '
                  f'level_dbfs={s.sdr_level_dbfs:.1f} adm={s.sdr_adm}', flush=True)
        else:
            print('no audio; level_dbfs=', s.sdr_level_dbfs, 'adm=', s.sdr_adm, flush=True)
        dev.session.exit()
        dev.close()
    finally:
        try:
            sa.off()
        except Exception:
            pass
        sa.ser.close()


if __name__ == '__main__':
    main()
