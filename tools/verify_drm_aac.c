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

#include "aacdecoder_lib.h"

static unsigned char *read_file(const char *path, long *len) {
    FILE *f = fopen(path, "rb");
    if (!f) { fprintf(stderr, "cannot open %s\n", path); exit(2); }
    fseek(f, 0, SEEK_END); *len = ftell(f); fseek(f, 0, SEEK_SET);
    unsigned char *d = (unsigned char *)malloc((size_t)*len);
    if (fread(d, 1, (size_t)*len, f) != (size_t)*len) { fprintf(stderr, "short read\n"); exit(2); }
    fclose(f);
    return d;
}

static int decode_frame(HANDLE_AACDECODER h, unsigned char *buf, size_t n,
                        short *pcm, CStreamInfo **si) {
    UCHAR *p = buf; UINT size = (UINT)n, valid = (UINT)n;
    if (aacDecoder_Fill(h, &p, &size, &valid) != AAC_DEC_OK) return -1;
    int r = aacDecoder_DecodeFrame(h, pcm, 4096, 0);
    *si = aacDecoder_GetStreamInfo(h);
    return r;
}

int main(int argc, char **argv) {
    if (argc != 5) {
        fprintf(stderr, "usage: %s <file.drm> <file.ga> <drm_frame_bytes> <ga_frame_bytes>\n", argv[0]);
        return 2;
    }
    long drm_len, ga_len;
    unsigned char *drm = read_file(argv[1], &drm_len);
    unsigned char *ga = read_file(argv[2], &ga_len);
    size_t drm_fs = (size_t)strtoul(argv[3], NULL, 10);
    size_t ga_fs = (size_t)strtoul(argv[4], NULL, 10);
    size_t drm_frames = drm_len / drm_fs, ga_frames = ga_len / ga_fs;
    if (drm_frames == 0 || drm_frames != ga_frames) {
        fprintf(stderr, "frame count mismatch: drm=%zu ga=%zu\n", drm_frames, ga_frames);
        return 2;
    }

    HANDLE_AACDECODER h_drm = aacDecoder_Open(TT_DRM, 1);
    HANDLE_AACDECODER h_ga = aacDecoder_Open(TT_MP4_RAW, 1);
    UCHAR t9[2] = {0x03, 0x00}, asc[2] = {0x13, 0x0C};
    UCHAR *p = t9; UINT l = 2; aacDecoder_ConfigRaw(h_drm, &p, &l);
    p = asc; l = 2; aacDecoder_ConfigRaw(h_ga, &p, &l);

    long total_mismatch = 0, total_energy = 0;
    for (size_t i = 0; i < drm_frames; i++) {
        short drm_pcm[4096], ga_pcm[4096];
        CStreamInfo *si_drm, *si_ga;
        memset(drm_pcm, 0, sizeof drm_pcm);
        memset(ga_pcm, 0, sizeof ga_pcm);
        int r1 = decode_frame(h_drm, drm + i * drm_fs, drm_fs, drm_pcm, &si_drm);
        int r2 = decode_frame(h_ga, ga + i * ga_fs, ga_fs, ga_pcm, &si_ga);
        if (r1 != 0 || r2 != 0) {
            fprintf(stderr, "frame %zu decode failed (drm=0x%x ga=0x%x)\n", i, r1, r2);
            return 3;
        }
        int n = si_drm->frameSize * si_drm->numChannels;
        long mismatch = 0, energy = 0;
        for (int j = 0; j < n; j++) {
            mismatch += drm_pcm[j] != ga_pcm[j];
            energy += ga_pcm[j] < 0 ? -ga_pcm[j] : ga_pcm[j];
        }
        total_mismatch += mismatch;
        total_energy += energy;
    }
    aacDecoder_Close(h_drm);
    aacDecoder_Close(h_ga);

    printf("%zu frames: mismatch=%ld energy=%ld -> %s\n", drm_frames, total_mismatch,
           total_energy, (total_mismatch == 0 && total_energy > 0) ? "OK" : "FAIL");
    return (total_mismatch == 0 && total_energy > 0) ? 0 : 1;
}
