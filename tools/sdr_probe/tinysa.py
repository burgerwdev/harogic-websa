#!/usr/bin/env python3
"""Minimal tinySA Ultra serial driver for SDR probe signal generation.

Protocol reference: tinysa-webgen (read-only): commands end with '\\r', the
response is framed by the 'ch> ' prompt. See src/serial/transport.ts.

Usage as a library:
    sa = TinySA('/dev/ttyACM0')
    sa.cmd('version')
    sa.cw(100e6, -20)         # low-output CW
    sa.am(100e6, -20, 1000, 50)
    sa.off()
    sa.close()
"""
from __future__ import annotations

import os
import time

import serial


class TinySA:
    PROMPT = 'ch> '

    def __init__(self, port='/dev/ttyACM0', baud=115200, timeout=2.0):
        self.ser = serial.Serial(port, baud, timeout=timeout)
        time.sleep(0.3)
        self.ser.reset_input_buffer()
        # Stop any running sweep/output as early as possible: the firmware CLI is slow
        # to answer while a sweep is active, which makes `sync` look like a hang.
        try:
            self.ser.write(b'pause\r'); self.ser.flush()
            time.sleep(0.2)
            self.ser.write(b'output off\r'); self.ser.flush()
            time.sleep(0.2)
        except Exception:
            pass
        self.sync()

    def _read_until_prompt(self, timeout=5.0):
        end = time.time() + timeout
        buf = b''
        while time.time() < end:
            try:
                chunk = self.ser.read(256)
            except serial.SerialException:
                break
            if chunk:
                buf += chunk
                if buf.endswith(self.PROMPT.encode()):
                    break
            else:
                if buf:
                    break
        text = buf.decode('utf-8', 'replace')
        # strip echo of the command: drop everything up to the first \r\n
        if '\n' in text:
            text = text.split('\n', 1)[1]
        return text.replace(self.PROMPT, '').strip()

    def sync(self, tries=1):
        for _ in range(tries):
            self.ser.write(b'\r')
            self.ser.flush()
            time.sleep(0.15)
            self._read_until_prompt(timeout=1.0)
        return True

    def cmd(self, command, timeout=5.0, wait=True):
        if os.environ.get('TINYSA_DEBUG'):
            print(f'  [tinySA tx] {command}', flush=True)
        self.ser.write((command + '\r').encode())
        self.ser.flush()
        if not wait:
            return ''
        out = self._read_until_prompt(timeout=timeout)
        if os.environ.get('TINYSA_DEBUG'):
            print(f'  [tinySA rx] {out!r}', flush=True)
        return out

    # ---- convenience ----
    def ident(self):
        return self.cmd('info'), self.cmd('version')

    def enter_low_output(self):
        # Full, self-contained reset into the generator (low output) mode with RF OFF.
        # Bench-verified: this prefix is what makes a later `output on` actually
        # radiate; without it (device left in input mode) RF stays silent.
        self.cmd('pause')
        self.cmd('modulation off')
        self.cmd('output off')
        self.cmd('mode low output')
        self.cmd('pause')
        self.cmd('sweeptime 0.003')
        self.cmd('sweep abort')
        return self.cmd('sweep')

    def _prepare_cw(self, freq_hz, level_dbm=-25.0):
        """Enter generator mode, set a CW frequency/level and settle (RF still off)."""
        self.enter_low_output()
        self.cmd(f'sweep cw {int(freq_hz)}', timeout=40)
        self.cmd('wait', timeout=10)          # safe: modulation is off here
        self.cmd(f'level {level_dbm}')

    def _transmit(self):
        # `output on` exactly ONCE. Bench-verified: a second `output on` acts as a
        # toggle and kills the RF.
        self.cmd('output on')
        self.cmd('resume')
        return self.cmd('sweep')

    def cw(self, freq_hz, level_dbm=-20.0):
        self._prepare_cw(freq_hz, level_dbm)
        return self._transmit()

    def am(self, freq_hz, level_dbm=-20.0, mod_hz=1000, depth=50):
        self._prepare_cw(freq_hz, level_dbm)
        self.cmd('modulation am')
        self.cmd(f'modulation freq {int(mod_hz)}')
        self.cmd(f'modulation depth {int(depth)}')
        return self._transmit()

    def fm(self, freq_hz, level_dbm=-20.0, mod_hz=1000, dev_hz=25000):
        self._prepare_cw(freq_hz, level_dbm)
        self.cmd('modulation fm')
        self.cmd(f'modulation freq {int(mod_hz)}')
        self.cmd(f'modulation deviation {int(dev_hz)}')
        return self._transmit()

    def off(self):
        self.cmd('modulation off')
        self.cmd('output off')
        return self.cmd('pause')

    def close(self):
        try:
            self.off()
        except Exception:
            pass
        self.ser.close()


if __name__ == '__main__':
    import sys
    sa = TinySA(sys.argv[1] if len(sys.argv) > 1 else '/dev/ttyACM0')
    print('info:\n', sa.ident()[0])
    print('version:\n', sa.ident()[1])
    sa.close()
