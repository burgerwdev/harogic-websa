#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <monitor.h>
#include "wave.h"
#include <ft8/decode.h>
#include <ft8/message.h>
#include <ft8/text.h>

int main(int argc, char** argv) {
    const char* path = argc > 1 ? argv[1] : "/tmp/ft8_slot02_ratio2.57.wav";
    int time_osr = argc > 2 ? atoi(argv[2]) : 2;
    int freq_osr = argc > 3 ? atoi(argv[3]) : 2;
    int rate = argc > 4 ? atoi(argv[4]) : 48000;
    int max_samples = 48000 * 20;
    float* signal = malloc(sizeof(float) * max_samples);
    int num_samples = max_samples, wav_rate = 0;   // capacity in, count out
    if (load_wav(signal, &num_samples, &wav_rate, path) != 0) { printf("cannot open %s\n", path); return 1; }

    monitor_config_t cfg = { .f_min = 100, .f_max = 3000, .sample_rate = rate,
                             .time_osr = time_osr, .freq_osr = freq_osr, .protocol = FTX_PROTOCOL_FT8 };
    monitor_t mon; monitor_init(&mon, &cfg);
    printf("rate %d, time_osr %d, freq_osr %d, block %d, subblock %d, nfft %d, bins %d\n",
           rate, time_osr, freq_osr, mon.block_size, mon.subblock_size, mon.nfft, mon.wf.num_bins);
    long total = 0;
    for (long at = 0; at + mon.subblock_size <= num_samples; at += mon.subblock_size) {
        monitor_process(&mon, signal + at);
        total += mon.subblock_size;
    }
    printf("processed %ld samples (%.2f s), %d blocks\n", total, (double)total/rate, mon.wf.num_blocks);
    ftx_candidate_t cands[30];
    int num = ftx_find_candidates(&mon.wf, 30, cands, -1000);
    printf("candidates (any score): %d\n", num);
    for (int i = 0; i < num && i < 6; ++i) {
        printf("  score %3d  time_offset %3d sub %d  freq_bin %3d sub %d  (~%.1f Hz)\n",
               cands[i].score, cands[i].time_offset, cands[i].time_sub,
               cands[i].freq_offset, cands[i].freq_sub,
               (cands[i].freq_offset + cands[i].freq_sub/(float)freq_osr) * 6.25);
        ftx_message_t msg; ftx_decode_status_t st;
        if (ftx_decode_candidate(&mon.wf, &cands[i], 25, &msg, &st)) {
            char text[FTX_MAX_MESSAGE_LENGTH];
            if (ftx_message_decode(&msg, NULL, text, NULL) == FTX_MESSAGE_RC_OK) {
                printf("      DECODED: %s\n", text);
            }
        }
    }
    monitor_free(&mon); free(signal);
    return 0;
}
