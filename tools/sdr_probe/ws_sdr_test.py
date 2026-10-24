#!/usr/bin/env python3
"""Tune a tinySA signal and capture native AUD2 PCM plus SDR display frames.

Display/control uses /ws; the selected channel's float32 PCM uses /ws/audio.
Writes a PCM16 WAV with the stream's actual sample rate and channel count.
"""
from __future__ import annotations

import asyncio
import json
import math
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
AUDIO_HEADER = struct.Struct('<4sIIIddIIQdIIQdQQ')


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
    mode = 'nfm' if mode == 'fm' else mode
    audio = bytearray()
    config = None
    last_seq = last_audio_index = last_count = None
    format_flags = 0
    rtaf = 0
    status = None
    async with aiohttp.ClientSession() as session, session.ws_connect(WS, max_msg_size=0) as ws:
        await ws.send_json({'cmd': 'SET_MODE', 'mode': 'sdr'})
        await ws.send_json({'cmd': 'SET_SDR', 'center': CENTER, 'decimate': 32})
        await ws.send_json({'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True,
                            'freq': SIGNAL, 'mode': mode,
                            'ifbw': 6000 if mode == 'am' else 25000,
                            'volume': 1.0, 'squelch': -120.0, 'agc': True})
        await ws.send_json({'cmd': 'STATUS'})
        loop = asyncio.get_running_loop()

        async def collect_display(seconds):
            nonlocal status, rtaf
            deadline = loop.time() + seconds
            while loop.time() < deadline:
                try:
                    msg = await asyncio.wait_for(ws.receive(), timeout=max(0.01, deadline - loop.time()))
                except asyncio.TimeoutError:
                    break
                if msg.type == aiohttp.WSMsgType.TEXT:
                    obj = json.loads(msg.data)
                    if obj.get('cmd') == 'STATUS':
                        status = obj
                    elif obj.get('cmd') == 'ERROR':
                        raise RuntimeError(f'backend error: {obj}')
                elif msg.type == aiohttp.WSMsgType.BINARY and msg.data[:4] in (b'RTAF', b'RTA2'):
                    rtaf += 1
                elif msg.type in (aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.ERROR):
                    raise RuntimeError('display socket closed')

        await collect_display(2.0)
        if not status or status.get('mode') != 'sdr':
            raise RuntimeError('backend did not enter SDR mode')
        try:
            async with session.ws_connect(f'{WS}/audio?channel=1', max_msg_size=0) as audio_ws:
                await ws.send_json({'cmd': 'STATUS'})

                async def collect_audio():
                    nonlocal config, last_seq, last_audio_index, last_count, format_flags
                    deadline = loop.time() + SECONDS
                    while loop.time() < deadline:
                        try:
                            msg = await asyncio.wait_for(audio_ws.receive(), timeout=max(0.01, deadline - loop.time()))
                        except asyncio.TimeoutError:
                            break
                        if msg.type in (aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.ERROR):
                            raise RuntimeError('audio socket closed')
                        if msg.type != aiohttp.WSMsgType.BINARY:
                            continue
                        data = msg.data
                        if len(data) < AUDIO_HEADER.size:
                            raise RuntimeError('truncated AUD2 header')
                        (magic, version, seq, frames, rate, freq, channel_id, epoch,
                         input_index, delay, channels, sample_format, audio_index,
                         input_rate, fraction_num, fraction_den) = AUDIO_HEADER.unpack_from(data)
                        if (magic != b'AUD2' or version != 1 or sample_format & 0xFFFF != 1
                                or sample_format & ~0x30001 or not channel_id
                                or channels not in (1, 2) or not math.isfinite(rate) or rate <= 0
                                or not math.isfinite(input_rate) or input_rate <= 0
                                or not math.isfinite(delay) or delay < 0
                                or not math.isfinite(freq) or abs(freq - SIGNAL) > 1
                                or not fraction_den or fraction_num >= fraction_den
                                or len(data) != AUDIO_HEADER.size + frames * channels * 4):
                            raise RuntimeError('invalid AUD2 frame')
                        identity = (channel_id, epoch, freq, rate, channels, input_rate)
                        format_flags = sample_format
                        if seq == 0:
                            if frames or audio_index:
                                raise RuntimeError('AUD2 config marker must have empty PCM at audio index zero')
                            config = identity
                            audio.clear()
                            last_seq = 0
                            last_audio_index = last_count = 0
                            continue
                        if (not frames or config != identity or last_seq is None
                                or seq != (last_seq + 1) & 0xFFFFFFFF
                                or (last_audio_index is not None and audio_index != last_audio_index + last_count)):
                            raise RuntimeError('AUD2 source identity or PCM continuity changed without a marker')
                        pcm = np.frombuffer(data, dtype='<f4', offset=AUDIO_HEADER.size)
                        if not np.isfinite(pcm).all():
                            raise RuntimeError('AUD2 contains non-finite PCM')
                        last_seq, last_audio_index, last_count = seq, audio_index, frames
                        audio.extend(data[AUDIO_HEADER.size:])

                await asyncio.gather(collect_display(SECONDS), collect_audio())
                await ws.send_json({'cmd': 'STATUS'})
                await collect_display(0.5)
                channel = next((ch for ch in (status or {}).get('sdr', {}).get('channels', [])
                                if ch.get('channel') == 1), None)
                if (config is None or not audio or channel is None
                        or (channel.get('channel_id'), channel.get('epoch')) != config[:2]):
                    raise RuntimeError('no PCM from the configured channel identity')
        finally:
            await ws.send_json({'cmd': 'SET_MODE', 'mode': 'std'})

    channel_id, epoch, freq, rate, channels, input_rate = config
    pcm = np.frombuffer(audio, dtype='<f4').reshape(-1, channels)
    mono = pcm.mean(axis=1)
    path = f'/tmp/ws_sdr_{mode}.wav'
    wav_pcm = (np.clip(pcm, -1, 1) * 32767).astype('<i2')
    with wave.open(path, 'wb') as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(round(rate))
        w.writeframes(wav_pcm.tobytes())
    tone = 0.0
    if len(mono) > 256:
        wnd = np.hanning(len(mono))
        spectrum = np.abs(np.fft.rfft((mono - mono.mean()) * wnd))
        frequencies = np.fft.rfftfreq(len(mono), 1 / rate)
        tone = float(frequencies[int(np.argmax(spectrum))])
    print(f'mode={mode} rtaf_frames={rtaf} audio={len(pcm)/rate:.2f}s '
          f'rate={rate} channels={channels} tone={tone:.1f} Hz')
    print(f'source: channel_id={channel_id} epoch={epoch} freq={freq} input_rate={input_rate}')
    print(f'delay_known={not bool(format_flags & 0x10000)} '
          f'causal_presentation={bool(format_flags & 0x20000)}')
    print('sdr channel:', channel)
    print('wav:', path)


if __name__ == '__main__':
    asyncio.run(main())
