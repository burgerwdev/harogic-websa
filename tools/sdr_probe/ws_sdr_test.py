#!/usr/bin/env python3
"""Headless end-to-end test of the WebSA SDR mode over the real WebSocket.

Sets SDR mode, tunes to a tinySA AM/FM signal, collects AUDF (audio) and RTAF
(spectrum) frames, writes a WAV and reports the demodulated tone.
"""
from __future__ import annotations

import asyncio
import os
import struct
import sys
import wave

import aiohttp
import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from tinysa import TinySA  # noqa: E402

WS = 'ws://127.0.0.1:8080/ws'
CENTER = 100e6
SIGNAL = 100.2e6
SECONDS = 3.0


async def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else 'am'
    sa = TinySA('/dev/ttyACM0')
    try:
        sa.enter_low_output()
        if mode == 'am':
            sa.am(SIGNAL, -25, 1000, 50)
        else:
            sa.fm(SIGNAL, -25, 1000, 25000)
        print('tinySA', mode, sa.cmd('sweep'), flush=True)
        await run_ws(mode)
    finally:
        try:
            sa.off()
        except Exception:
            pass
        sa.ser.close()


async def run_ws(mode):
    audio = bytearray()
    rate = 48000
    rtaf = 0
    status = None
    async with aiohttp.ClientSession() as session, session.ws_connect(WS, max_msg_size=0) as ws:
            async def send(obj):
                await ws.send_str(__import__('json').dumps(obj))
            await send({'cmd': 'SET_MODE', 'mode': 'sdr'})
            await send({'cmd': 'SET_SDR', 'center': CENTER, 'decimate': 32})
            await send({'cmd': 'SET_SDR_TUNE', 'listen': SIGNAL})
            await send({'cmd': 'SET_SDR_DEMOD', 'mode': mode, 'ifbw': 6000 if mode == 'am' else 25000,
                        'volume': 1.0, 'squelch': -120.0, 'agc': True})
            await send({'cmd': 'STATUS'})
            loop = asyncio.get_event_loop()
            # wait until the backend really is in SDR mode before collecting
            t0 = loop.time()
            while loop.time() - t0 < 8.0:
                if status and status.get('mode') == 'sdr' and (status.get('sdr', {}) or {}).get('actual'):
                    break
                try:
                    msg = await asyncio.wait_for(ws.receive(), timeout=2.0)
                except asyncio.TimeoutError:
                    continue
                if msg.type == aiohttp.WSMsgType.TEXT:
                    import json
                    obj = json.loads(msg.data)
                    if obj.get('cmd') == 'STATUS':
                        status = obj
                    elif obj.get('cmd') == 'ERROR':
                        print('ERROR:', obj, flush=True)
            print('mode after config:', (status or {}).get('mode'), flush=True)
            t0 = loop.time()
            while loop.time() - t0 < SECONDS:
                try:
                    msg = await asyncio.wait_for(ws.receive(), timeout=2.0)
                except asyncio.TimeoutError:
                    continue
                if msg.type == aiohttp.WSMsgType.TEXT:
                    import json
                    obj = json.loads(msg.data)
                    if obj.get('cmd') == 'STATUS':
                        status = obj
                    elif obj.get('cmd') == 'ERROR':
                        print('ERROR:', obj, flush=True)
                elif msg.type == aiohttp.WSMsgType.BINARY:
                    data = msg.data
                    magic = data[:4]
                    if magic == b'AUDF':
                        _, seq, r, n = struct.unpack_from('<4sIII', data, 0)
                        rate = r
                        audio += data[16:16 + n * 2]
                    elif magic == b'RTAF':
                        rtaf += 1
                elif msg.type in (aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.ERROR):
                    break
            await send({'cmd': 'SET_MODE', 'mode': 'std'})
            await asyncio.sleep(0.2)

    pcm = np.frombuffer(bytes(audio), dtype=np.int16).astype(np.float32) / 32768.0
    path = f'/tmp/ws_sdr_{mode}.wav'
    with wave.open(path, 'wb') as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(rate)
        w.writeframes(bytes(audio))
    tone = 0.0
    if len(pcm) > 256:
        wnd = np.hanning(len(pcm)); s = np.abs(np.fft.rfft((pcm - pcm.mean()) * wnd))
        f = np.fft.rfftfreq(len(pcm), 1 / rate)
        tone = float(f[int(np.argmax(s))])
    sdr = (status or {}).get('sdr', {})
    print(f'mode={mode} rtaf_frames={rtaf} audio={len(pcm)/rate:.2f}s rate={rate} tone={tone:.1f} Hz')
    print('sdr req:', {k: sdr.get(k) for k in ('center', 'decimate', 'listen', 'demod', 'if_bw')})
    print('sdr actual:', sdr.get('actual'))
    print('sdr level_dbfs:', sdr.get('level_dbfs'), 'squelch_open:', sdr.get('squelch_open'))
    print('sdr adm:', sdr.get('adm'))
    print('wav:', path)


if __name__ == '__main__':
    asyncio.run(main())
