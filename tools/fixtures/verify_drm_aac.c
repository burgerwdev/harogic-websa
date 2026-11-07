/*
 * Verify the DRM AAC audio fixture (tests/fixtures/drm/aac_sine_24k.{drm,ga}).
 *
 * Decodes the re-serialised DRM access units with the FDK AAC decoder in TT_DRM
 * transport and the original GA access units in TT_MP4_RAW, then asserts that the
 * PCM is non-silent and that the two decodes match bit-for-bit (the re-serialiser
 * is lossless). This is the native counterpart of the in-tree fixture test.
 *
 * Build against a native FDK AAC library (the 960-sample patch is only needed for
 * the *encoder*; the decoder is unmodified):
 *
 *   cc -O2 -I <fdk>/libAACdec/include -I <fdk>/libSYS/include \
 *      -I <fdk>/libMpegTPDec/include -I <fdk>/libFDK/include \
 *      -I <fdk>/libSBRdec/include -I <fdk>/libSACdec/include \
 *      -I <fdk>/libPCMutils/include -I <fdk>/libArithCoding/include \
 *      -I <fdk>/libDRCdec/include \
 *      verify_drm_aac.c <fdk>/build/libfdk-aac.a -lm -o verify_drm_aac
 *
 * Run:
 *   ./verify_drm_aac tests/fixtures/drm/aac_sine_24k.drm \
 *                     tests/fixtures/drm/aac_sine_24k.ga 36 320
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <inttypes.h>
#include <limits.h>

#include "aacdecoder_lib.h"

static unsigned char *read_file(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    unsigned char *d = NULL;
    if (!f) { fprintf(stderr, "cannot open %s\n", path); return NULL; }
    if (fseek(f, 0, SEEK_END) != 0) goto failed;
    long size = ftell(f);
    if (size <= 0 || (uintmax_t)size > SIZE_MAX || fseek(f, 0, SEEK_SET) != 0) goto failed;
    *len = (size_t)size;
    d = (unsigned char *)malloc(*len);
    if (!d || fread(d, 1, *len, f) != *len) goto failed;
    if (fclose(f) != 0) {
        fprintf(stderr, "cannot close %s\n", path);
        free(d);
        return NULL;
    }
    return d;
failed:
    fprintf(stderr, "cannot read complete nonempty file %s\n", path);
    free(d);
    fclose(f);
    return NULL;
}

static int parse_frame_size(const char *text, size_t *size) {
    if (!*text) return 0;
    for (const char *p = text; *p; ++p) if (*p < '0' || *p > '9') return 0;
    errno = 0;
    char *end;
    uintmax_t value = strtoumax(text, &end, 10);
    if (errno || *end || value == 0 || value > UINT_MAX || value > SIZE_MAX) return 0;
    *size = (size_t)value;
    return 1;
}

static int decode_frame(HANDLE_AACDECODER h, unsigned char *buf, size_t n,
                        short *pcm, CStreamInfo **si) {
    UCHAR *p = buf; UINT size = (UINT)n, valid = (UINT)n;
    if (n > UINT_MAX || aacDecoder_Fill(h, &p, &size, &valid) != AAC_DEC_OK || valid != 0) return -1;
    int r = aacDecoder_DecodeFrame(h, pcm, 4096, 0);
    *si = r == AAC_DEC_OK ? aacDecoder_GetStreamInfo(h) : NULL;
    return r;
}

static int valid_pcm_geometry(const CStreamInfo *si) {
    return si && si->frameSize > 0 && si->numChannels > 0 && si->sampleRate > 0
        && (size_t)si->frameSize <= 4096 / (size_t)si->numChannels;
}

int main(int argc, char **argv) {
    if (argc != 5) {
        fprintf(stderr, "usage: %s <file.drm> <file.ga> <drm_frame_bytes> <ga_frame_bytes>\n", argv[0]);
        return 2;
    }
    int status = 2;
    size_t drm_len = 0, ga_len = 0, drm_fs, ga_fs;
    unsigned char *drm = NULL, *ga = NULL;
    HANDLE_AACDECODER h_drm = NULL, h_ga = NULL;
    if (!parse_frame_size(argv[3], &drm_fs) || !parse_frame_size(argv[4], &ga_fs)) {
        fprintf(stderr, "frame sizes must be positive decimal UINT byte counts\n");
        goto cleanup;
    }
    drm = read_file(argv[1], &drm_len);
    if (!drm) goto cleanup;
    ga = read_file(argv[2], &ga_len);
    if (!ga) goto cleanup;
    if (drm_len % drm_fs != 0 || ga_len % ga_fs != 0) {
        fprintf(stderr, "incomplete final access unit\n");
        goto cleanup;
    }
    size_t drm_frames = drm_len / drm_fs, ga_frames = ga_len / ga_fs;
    if (drm_frames == 0 || drm_frames != ga_frames) {
        fprintf(stderr, "frame count mismatch: drm=%zu ga=%zu\n", drm_frames, ga_frames);
        goto cleanup;
    }

    status = 3;
    h_drm = aacDecoder_Open(TT_DRM, 1);
    h_ga = aacDecoder_Open(TT_MP4_RAW, 1);
    if (!h_drm || !h_ga) {
        fprintf(stderr, "cannot open both AAC decoders\n");
        goto cleanup;
    }
    UCHAR t9[2] = {0x03, 0x00}, asc[2] = {0x13, 0x0C};
    UCHAR *p = t9; UINT l = 2;
    if (aacDecoder_ConfigRaw(h_drm, &p, &l) != AAC_DEC_OK) {
        fprintf(stderr, "DRM decoder configuration failed\n");
        goto cleanup;
    }
    p = asc; l = 2;
    if (aacDecoder_ConfigRaw(h_ga, &p, &l) != AAC_DEC_OK) {
        fprintf(stderr, "GA decoder configuration failed\n");
        goto cleanup;
    }

    uint64_t total_mismatch = 0, total_energy = 0;
    for (size_t i = 0; i < drm_frames; i++) {
        short drm_pcm[4096], ga_pcm[4096];
        CStreamInfo *si_drm, *si_ga;
        memset(drm_pcm, 0, sizeof drm_pcm);
        memset(ga_pcm, 0, sizeof ga_pcm);
        int r1 = decode_frame(h_drm, drm + i * drm_fs, drm_fs, drm_pcm, &si_drm);
        int r2 = decode_frame(h_ga, ga + i * ga_fs, ga_fs, ga_pcm, &si_ga);
        if (r1 != 0 || r2 != 0) {
            fprintf(stderr, "frame %zu decode failed (drm=0x%x ga=0x%x)\n", i, r1, r2);
            goto cleanup;
        }
        if (!valid_pcm_geometry(si_drm) || !valid_pcm_geometry(si_ga)
            || si_drm->frameSize != si_ga->frameSize
            || si_drm->numChannels != si_ga->numChannels
            || si_drm->sampleRate != si_ga->sampleRate) {
            fprintf(stderr, "frame %zu has invalid or different PCM geometry\n", i);
            goto cleanup;
        }
        size_t n = (size_t)si_drm->frameSize * (size_t)si_drm->numChannels;
        uint64_t mismatch = 0, energy = 0;
        for (size_t j = 0; j < n; j++) {
            mismatch += drm_pcm[j] != ga_pcm[j];
            energy += ga_pcm[j] < 0 ? -ga_pcm[j] : ga_pcm[j];
        }
        if (UINT64_MAX - total_mismatch < mismatch || UINT64_MAX - total_energy < energy) {
            fprintf(stderr, "PCM comparison totals exceed uint64 capacity\n");
            goto cleanup;
        }
        total_mismatch += mismatch;
        total_energy += energy;
    }
    printf("%zu frames: mismatch=%" PRIu64 " energy=%" PRIu64 " -> %s\n", drm_frames, total_mismatch,
           total_energy, (total_mismatch == 0 && total_energy > 0) ? "OK" : "FAIL");
    status = (total_mismatch == 0 && total_energy > 0) ? 0 : 1;
cleanup:
    if (h_drm) aacDecoder_Close(h_drm);
    if (h_ga) aacDecoder_Close(h_ga);
    free(drm);
    free(ga);
    return status;
}
