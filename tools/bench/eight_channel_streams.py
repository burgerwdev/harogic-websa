#!/usr/bin/env python3
"""Bounded real stream consumers for full-band eight-channel acceptance."""
from __future__ import annotations

import asyncio
import json
import struct
import threading
import time
import traceback
from fractions import Fraction

import numpy as np
from websockets.asyncio.client import connect


def input_stride(input_rate, output_rate):
    # Rates are IEEE f64, while source positions are exact. Recover the rational
    # cadence (e.g. 192 for 62.5e6 / 325520.8333333333) without rounding the rate.
    ratio = float(input_rate) / output_rate
    stride = Fraction(ratio).limit_denominator(1 << 20)
    assert abs(float(stride) - ratio) < 1e-8
    return stride


class StreamMonitor:
    def __init__(self, origin, snapshot):
        self.origin, self.snapshot = origin.replace('http', 'ws'), snapshot
        self.rows, self.errors = {}, []
        self.lock, self.ready = threading.Lock(), threading.Event()
        self.loop = self.finished = None
        self.thread = None
        self.started = time.monotonic()
        self.expected_connections = sum(bool(r['confirmed']['processors']) for r in snapshot['receivers']) + 1
        self.expected_connections += sum(r['confirmed']['purpose'] == 'digital' for r in snapshot['receivers'])

    def __enter__(self):
        self.thread = threading.Thread(target=lambda: asyncio.run(self.run()), daemon=True)
        self.thread.start()
        try:
            assert self.ready.wait(30), 'stream subscriptions did not become ready'
            self.check()
        except BaseException:
            self.close()
            raise
        return self

    def __exit__(self, *_):
        self.close()

    def close(self):
        if self.loop and self.finished and self.loop.is_running():
            self.loop.call_soon_threadsafe(self.finished.set)
        if self.thread:
            self.thread.join(timeout=10)
            assert not self.thread.is_alive(), 'stream consumer did not stop'

    def check(self):
        with self.lock:
            assert not self.errors, self.errors

    def status(self):
        self.check()
        with self.lock:
            return json.loads(json.dumps(self.rows))

    async def run(self):
        self.loop, self.finished = asyncio.get_running_loop(), asyncio.Event()
        tasks = []
        for receiver in self.snapshot['receivers']:
            if receiver['confirmed']['processors']:
                tasks.append(asyncio.create_task(self.consume(receiver, 'audio')))
            if receiver['confirmed']['purpose'] == 'digital':
                tasks.append(asyncio.create_task(self.consume(receiver, 'decode')))
        tasks.append(asyncio.create_task(self.consume(self.snapshot['receivers'][-1], 'iq')))
        try:
            await self.finished.wait()
        finally:
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)

    async def consume(self, receiver, kind):
        uid = receiver['receiver_uid']
        key = f'{receiver["display_number"]}-{kind}'
        route = f'/ws/{kind}?receiver_uid={uid}' + ('&ack=1' if kind == 'decode' else '')
        row = {'receiver_uid': uid, 'kind': kind, 'packets': 0, 'samples': 0, 'energy': 0.0,
            'rate_hz': None, 'identity': None, 'first_position': None, 'last_position': None,
            'expected_text_count': 0, 'recent_decodes': []}
        last_seq = last_position = last_count = last_audio_index = None
        rate_input = Fraction(self.snapshot['capture']['input_rate_hz'])
        try:
            async with connect(self.origin + route, max_size=4 * 1024 * 1024, max_queue=16) as socket:
                with self.lock:
                    self.rows[key] = row
                    if len(self.rows) == self.expected_connections:
                        self.ready.set()
                async for packet in socket:
                    if kind == 'iq' and isinstance(packet, str):
                        event = json.loads(packet)
                        assert event['receiver_uid'] == uid, event
                        assert event['stream'] == receiver['stream'], (event['stream'], receiver['stream'])
                        assert event['type'] == 'stream_start', event
                        assert last_seq is None, 'unexpected IQ boundary during sustained capture'
                        continue
                    assert isinstance(packet, bytes)
                    if kind == 'decode':
                        assert packet[:4] == b'SDE2' and len(packet) == 8 + struct.unpack_from('<I', packet, 4)[0]
                        envelope = json.loads(packet[8:])
                        assert envelope['receiver_uid'] == uid and envelope['service_id'] == self.snapshot['service_id']
                        identity = [envelope['channel_id'], envelope['epoch']]
                        sequence = envelope['seq']
                        assert all(isinstance(envelope[name], str) for name in
                            ['input_index', 'input_fraction_num', 'input_fraction_den'])
                        assert int(envelope['input_fraction_den']) > int(envelope['input_fraction_num']) >= 0
                        if last_seq is not None:
                            assert sequence == last_seq + 1 and row['identity'] == identity
                        event = envelope['event']
                        with self.lock:
                            row['identity'], row['packets'] = identity, row['packets'] + 1
                            row['last_position'] = envelope['input_index']
                            if event.get('type') == 'ft8':
                                row['recent_decodes'].append(event)
                                row['recent_decodes'] = row['recent_decodes'][-32:]
                                if 'CQ WE0BSA PM95' in json.dumps(event):
                                    row['expected_text_count'] += 1
                                    row['last_expected_seconds'] = time.monotonic() - self.started
                        last_seq = sequence
                        await socket.send(json.dumps({'type': 'ack', **{name: envelope[name] for name in
                            ['channel', 'channel_id', 'epoch', 'seq']}}))
                        continue
                    magic = b'AUD2' if kind == 'audio' else None
                    assert packet[:4] == magic if magic else packet[:4] in [b'IQBF', b'IQAX']
                    sequence, count = struct.unpack_from('<II', packet, 8)
                    rate = struct.unpack_from('<d', packet, 16)[0]
                    identity = list(struct.unpack_from('<II', packet, 32))
                    position = Fraction(struct.unpack_from('<Q', packet, 40)[0])
                    if kind == 'audio':
                        channels = struct.unpack_from('<I', packet, 56)[0]
                        assert channels in [1, 2] and len(packet) == 96 + count * channels * 4
                        numerator, denominator = struct.unpack_from('<QQ', packet, 80)
                        assert denominator > numerator >= 0
                        position += Fraction(numerator, denominator)
                        audio_index = struct.unpack_from('<Q', packet, 64)[0]
                        values = np.frombuffer(packet, dtype='<f4', offset=96)
                        assert struct.unpack_from('<d', packet, 72)[0] == float(rate_input)
                    else:
                        assert len(packet) == 56 + count * 8
                        values = np.frombuffer(packet, dtype='<f4', offset=56)
                    if not count:
                        assert last_seq is None and sequence == 0
                        continue
                    assert np.isfinite(values).all() and rate > 0
                    if last_seq is not None:
                        assert sequence == last_seq + 1 and row['identity'] == identity
                        if kind == 'iq':
                            stride = input_stride(rate_input, rate)
                            assert position == last_position + last_count * stride, (position, last_position, last_count, stride)
                        else:
                            assert audio_index == last_audio_index + last_count
                            assert position >= last_position
                    with self.lock:
                        row.update(rate_hz=rate, identity=identity, last_position=str(position))
                        if kind == 'audio':
                            row['channels'] = channels
                        if row['first_position'] is None:
                            row['first_position'] = str(position)
                        row['packets'] += 1; row['samples'] += count
                        row['energy'] += float(np.dot(values, values))
                    last_seq, last_position, last_count = sequence, position, count
                    if kind == 'audio':
                        last_audio_index = audio_index
                if not self.finished.is_set():
                    raise AssertionError('subscription ended before the measurement')
        except asyncio.CancelledError:
            raise
        except Exception as error:
            with self.lock:
                self.errors.append({'stream': key, 'error': repr(error), 'traceback': traceback.format_exc()})
            self.ready.set(); self.finished.set()
