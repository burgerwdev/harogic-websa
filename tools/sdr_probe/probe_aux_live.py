#!/usr/bin/env python3
"""Live aux-DDC verification (G1') against a running native service: enable aux
channel 2 and assert IQAX frames arrive on ?iqaux=1 (and nowhere else).

Usage: python3 tools/sdr_probe/probe_aux_live.py --url http://127.0.0.1:8080
       [--center 100e6] [--aux-freq 100.1e6]
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import struct
import sys
import time

import aiohttp

IQ_HEADER = struct.Struct('<4sIIIddIIQd')  # IQ wire v2, including magic


async def send(ws, payload):
    await ws.send_str(json.dumps(payload))


async def wait_connected(ws, timeout=30.0):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        raw = await ws.receive()
        if raw.type == aiohttp.WSMsgType.BINARY:
            continue
        if raw.type != aiohttp.WSMsgType.TEXT:
            break
        msg = json.loads(raw.data)
        if msg.get('cmd') == 'ERROR':
            raise RuntimeError(json.dumps(msg))
        if msg.get('cmd') == 'STATUS':
            if msg.get('connected'):
                return msg
            if 'Device open' in str(msg.get('last_error', '')):
                pass
    raise TimeoutError('device never connected')


async def drain_status(ws, seconds=2.0):
    """Collect messages for a while; return (last STATUS, non-STATUS messages)."""
    last = None
    others = []
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        try:
            raw = await asyncio.wait_for(ws.receive(), timeout=end - time.monotonic())
        except (asyncio.TimeoutError, TimeoutError):
            break
        if raw.type == aiohttp.WSMsgType.BINARY:
            continue
        if raw.type != aiohttp.WSMsgType.TEXT:
            break
        msg = json.loads(raw.data)
        if msg.get('cmd') == 'ERROR':
            raise RuntimeError(json.dumps(msg))
        if msg.get('cmd') == 'STATUS':
            last = msg
        else:
            others.append(msg)
    return last, others


async def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--url', default=os.environ.get('WEBSA_URL', 'http://127.0.0.1:8080'),
                    help='HTTP base URL of the running native service')
    ap.add_argument('--center', type=float, default=100e6)
    ap.add_argument('--aux-freq', type=float, default=100.1e6)
    args = ap.parse_args()

    ws_url = args.url.rstrip('/') + '/ws'
    async with aiohttp.ClientSession() as http, http.ws_connect(ws_url) as ws:
        await send(ws, {'cmd': 'CONNECT'})
        await wait_connected(ws)
        await send(ws, {'cmd': 'SET_MODE', 'mode': 'sdr'})
        await send(ws, {'cmd': 'SET_SDR', 'center': args.center, 'decimate': 32})
        await send(ws, {'cmd': 'SET_SDR_CHANNEL', 'channel': 1, 'enabled': True,
                        'freq': args.center, 'mode': 'am', 'ifbw': 6000})
        await drain_status(ws, 3.0)           # let the SDR chain settle
        await send(ws, {'cmd': 'SET_SDR_CHANNEL', 'channel': 2, 'enabled': True,
                        'freq': args.aux_freq, 'mode': 'am', 'ifbw': 6000})
        st, others = await drain_status(ws, 3.0)
        for m in others:
            print('non-STATUS:', json.dumps(m)[:300])
        aux_echo = (st or {}).get('req', {}).get('sdr', {}).get('aux', {})
        print('aux echo:', aux_echo)
        registry = (st or {}).get('channel_registry', [])
        expected = {(row['epoch'], row['channel_id']) for row in registry
                    if row.get('channel') == 2 and row.get('state') == 'active'}
        if len(expected) != 1:
            raise ValueError('active auxiliary slot 2 missing from channel registry')

        async with http.ws_connect(ws_url + '?iqaux=1') as aux_ws:
            end = time.monotonic() + 5.0
            iqax = 0
            iqbf_on_aux = 0
            centers = set()
            identities = set()
            while time.monotonic() < end:
                try:
                    raw = await asyncio.wait_for(aux_ws.receive(),
                                                 timeout=end - time.monotonic())
                except (asyncio.TimeoutError, TimeoutError):
                    break
                if raw.type != aiohttp.WSMsgType.BINARY:
                    continue
                magic = raw.data[:4]
                if magic not in (b'IQAX', b'IQBF'):
                    continue
                if len(raw.data) < IQ_HEADER.size:
                    raise ValueError('short IQ header')
                (_magic, version, _seq, count, _rate, center, channel_id,
                 epoch, _input_index, _group_delay) = IQ_HEADER.unpack_from(raw.data)
                if version != 2 or len(raw.data) != IQ_HEADER.size + count * 8:
                    raise ValueError('invalid IQ v2 frame')
                if magic == b'IQAX':
                    iqax += 1
                    identities.add((epoch, channel_id))
                    centers.add(center)
                else:
                    iqbf_on_aux += 1
        print(f'IQAX frames: {iqax}, identities: {identities}, centers: {centers}, '
              f'IQBF on aux socket: {iqbf_on_aux}')
        ok = iqax > 0 and identities == expected and iqbf_on_aux == 0
        print('RESULT:', 'PASS' if ok else 'FAIL')
    sys.exit(0 if ok else 1)


if __name__ == '__main__':
    asyncio.run(main())
