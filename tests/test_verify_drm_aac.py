"""CLI/resource contracts of the fixture verifier, using an isolated fake FDK API.

These tests validate the C comparison tool, not production AAC decoding fidelity.
The fake decoder writes only the caller-provided PCM capacity and records ownership.
"""
from __future__ import annotations

import os
import resource
import shutil
import subprocess
from pathlib import Path

import pytest

HEADER = r'''
#ifndef FAKE_AACDECODER_LIB_H
#define FAKE_AACDECODER_LIB_H
typedef unsigned char UCHAR;
typedef unsigned int UINT;
typedef struct { int frameSize, numChannels, sampleRate; } CStreamInfo;
typedef struct Decoder *HANDLE_AACDECODER;
enum { AAC_DEC_OK = 0, TT_DRM = 1, TT_MP4_RAW = 2 };
HANDLE_AACDECODER aacDecoder_Open(int transport, unsigned layers);
int aacDecoder_ConfigRaw(HANDLE_AACDECODER h, UCHAR **data, UINT *length);
int aacDecoder_Fill(HANDLE_AACDECODER h, UCHAR **data, UINT *length, UINT *valid);
int aacDecoder_DecodeFrame(HANDLE_AACDECODER h, short *pcm, int capacity, unsigned flags);
CStreamInfo *aacDecoder_GetStreamInfo(HANDLE_AACDECODER h);
void aacDecoder_Close(HANDLE_AACDECODER h);
#endif
'''

FAKE = r'''
#include "aacdecoder_lib.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
struct Decoder { int transport; CStreamInfo info; };
static int scenario(const char *name) {
    const char *value = getenv("FAKE_AAC_CASE");
    return value && strcmp(value, name) == 0;
}
static void record(const char *event) {
    FILE *f = fopen(getenv("FAKE_AAC_LOG"), "a");
    if (f) { fputs(event, f); fputc('\n', f); fclose(f); }
}
HANDLE_AACDECODER aacDecoder_Open(int transport, unsigned layers) {
    (void)layers;
    if (transport == TT_MP4_RAW && scenario("open_failure")) return NULL;
    HANDLE_AACDECODER h = calloc(1, sizeof *h);
    if (!h) abort();
    h->transport = transport;
    h->info = (CStreamInfo){ 2, 1, 24000 };
    if (transport == TT_MP4_RAW) {
        if (scenario("rate_mismatch")) h->info.sampleRate = 48000;
        if (scenario("frame_mismatch")) h->info.frameSize = 4;
        if (scenario("channel_mismatch")) h->info.numChannels = 2;
    }
    if (scenario("negative_frame")) h->info.frameSize = -2;
    if (scenario("zero_channels")) h->info.numChannels = 0;
    if (scenario("zero_rate")) h->info.sampleRate = 0;
    if (scenario("capacity_boundary")) h->info.frameSize = 4096;
    if (scenario("capacity_exceeded")) h->info.frameSize = 4097;
    record("open");
    return h;
}
int aacDecoder_ConfigRaw(HANDLE_AACDECODER h, UCHAR **data, UINT *length) {
    (void)data; (void)length;
    record("config");
    return !h || (h->transport == TT_MP4_RAW && scenario("config_failure")) ? 19 : 0;
}
int aacDecoder_Fill(HANDLE_AACDECODER h, UCHAR **data, UINT *length, UINT *valid) {
    (void)data; (void)length;
    record("fill");
    if (!h || scenario("fill_failure")) return 20;
    if (!scenario("unconsumed")) *valid = 0;
    return 0;
}
int aacDecoder_DecodeFrame(HANDLE_AACDECODER h, short *pcm, int capacity, unsigned flags) {
    (void)flags;
    record("decode");
    if (!h || scenario("decode_failure")) return 21;
    for (int i = 0; i < capacity; ++i) pcm[i] = scenario("silence") ? 0 : 37;
    if (h->transport == TT_MP4_RAW && scenario("pcm_mismatch")) pcm[0] = 38;
    return 0;
}
CStreamInfo *aacDecoder_GetStreamInfo(HANDLE_AACDECODER h) {
    return !h || scenario("null_info") ? NULL : &h->info;
}
void aacDecoder_Close(HANDLE_AACDECODER h) {
    if (h) { record("close"); free(h); }
}
'''


@pytest.fixture(scope='module')
def verifier(tmp_path_factory):
    compiler = shutil.which('cc')
    if compiler is None:
        pytest.skip('C compiler required for isolated fixture-verifier tests')
    build = tmp_path_factory.mktemp('verify-drm-aac')
    (build / 'aacdecoder_lib.h').write_text(HEADER)
    (build / 'fake.c').write_text(FAKE)
    target = build / 'verify-drm-aac'
    source = Path(__file__).resolve().parents[1] / 'tools/fixtures/verify_drm_aac.c'
    subprocess.run([compiler, '-std=c11', '-Wall', '-Wextra', '-Werror', '-I', str(build),
                    str(source), str(build / 'fake.c'), '-o', str(target)], check=True,
                   capture_output=True, text=True)
    return target


def no_core():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def run_verifier(verifier, tmp_path, *, sizes=('2', '2'), lengths=(2, 2), scenario='valid'):
    drm, ga, log = (tmp_path / name for name in ('input.drm', 'input.ga', 'ownership.log'))
    drm.write_bytes(b'\x01' * lengths[0])
    ga.write_bytes(b'\x01' * lengths[1])
    run = subprocess.run([str(verifier), str(drm), str(ga), *sizes], capture_output=True,
                         text=True, preexec_fn=no_core, timeout=5,
                         env={**os.environ, 'FAKE_AAC_CASE': scenario, 'FAKE_AAC_LOG': str(log)})
    events = log.read_text().splitlines() if log.exists() else []
    return run, events


@pytest.mark.parametrize('size', ['0', '-1', 'abc', '1junk', '4294967296'])
def test_invalid_frame_sizes_are_rejected_before_decoder_open(verifier, tmp_path, size):
    result, events = run_verifier(verifier, tmp_path, sizes=(size, '2'))
    assert result.returncode == 2, result.stderr
    assert events == []


def test_incomplete_last_access_units_are_rejected(verifier, tmp_path):
    result, events = run_verifier(verifier, tmp_path, lengths=(3, 3))
    assert result.returncode == 2, result.stdout + result.stderr
    assert events == []


@pytest.mark.parametrize('scenario', [
    'open_failure', 'config_failure', 'fill_failure', 'unconsumed', 'decode_failure',
    'null_info', 'negative_frame', 'zero_channels', 'zero_rate', 'capacity_exceeded',
    'rate_mismatch', 'frame_mismatch', 'channel_mismatch',
])
def test_invalid_decoder_results_release_every_open_handle(verifier, tmp_path, scenario):
    result, events = run_verifier(verifier, tmp_path, scenario=scenario)
    assert result.returncode == 3, result.stdout + result.stderr
    assert events.count('close') == events.count('open')
    if scenario in ('open_failure', 'config_failure'):
        assert 'decode' not in events


@pytest.mark.parametrize(('scenario', 'status'), [
    ('valid', 0), ('capacity_boundary', 0), ('silence', 1), ('pcm_mismatch', 1),
])
def test_valid_geometry_compares_all_pcm_and_closes_handles(verifier, tmp_path, scenario, status):
    result, events = run_verifier(verifier, tmp_path, scenario=scenario)
    assert result.returncode == status, result.stdout + result.stderr
    assert events.count('open') == events.count('close') == 2
