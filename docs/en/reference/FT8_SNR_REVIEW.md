# Official FT8 source review, 2026-11-10

Reviewed source is pinned, independently of the executable used for IQ comparison:

- WSJT-X official `WSJTX/wsjtx`, commit
  `967c85a6119110788b0df3782cf796485b2cee14`:
  [ft8b.f90](https://github.com/WSJTX/wsjtx/blob/967c85a6119110788b0df3782cf796485b2cee14/lib/ft8/ft8b.f90),
  [baseline.f90](https://github.com/WSJTX/wsjtx/blob/967c85a6119110788b0df3782cf796485b2cee14/lib/ft8/baseline.f90),
  [get_spectrum_baseline.f90](https://github.com/WSJTX/wsjtx/blob/967c85a6119110788b0df3782cf796485b2cee14/lib/ft8/get_spectrum_baseline.f90),
  [sync8.f90](https://github.com/WSJTX/wsjtx/blob/967c85a6119110788b0df3782cf796485b2cee14/lib/ft8/sync8.f90),
  [ft8_decode.f90](https://github.com/WSJTX/wsjtx/blob/967c85a6119110788b0df3782cf796485b2cee14/lib/ft8_decode.f90).
- JTDX official `jtdx-project/jtdx`, commit
  `2a0e2bea8c66c9ca94d2ea8034cf83a68cfa40eb`:
  [ft8b.f90](https://github.com/jtdx-project/jtdx/blob/2a0e2bea8c66c9ca94d2ea8034cf83a68cfa40eb/lib/ft8b.f90),
  [ft8_downsample.f90](https://github.com/jtdx-project/jtdx/blob/2a0e2bea8c66c9ca94d2ea8034cf83a68cfa40eb/lib/ft8_downsample.f90).
- Same-input executable comparison retains Debian WSJT-X **2.7.0+repack-1**
  `jt9`/`ft8sim`, with SHA-256 hashes in the validation JSON. This is not a claim
  that the packaged executable was compiled from the source commits above.

| Stage | WSJT-X | JTDX |
|---|---|---|
| Noise statistic | `ft8b` sums decoded-tone powers across 79 symbols; an alternative reference samples an off-tone `mod(itone+4,7)`. The normal path uses `xbase` from the averaged-spectrum baseline. The baseline fits a degree-four polynomial to the lowest 10% of points in ten segments. `sync8` also uses a separate 40th-percentile normalization for candidate sync contrast; that is not the reported SNR noise estimate. | Per symbol, decoded-tone power is compared to the mean of all seven other tone powers; average the per-symbol ratios, then subtract one. The noise denominator is floored at 0.01 in its internal scale. |
| Empirical/scale correction | Baseline adds 0.65 dB; `xbase` conversion subtracts 40 dB; the second ratio uses `3e6`; both ratios use a −27 dB conversion in that audio/filter scale. Initial ratio sentinels and the `arg > 0.1` branch are numerical/heuristic choices. | −26.5 dB offset; above +7 dB a nonlinear upward correction; further corrections above +30/+40 dB; nonlinear weak-signal correction below −17 dB. These are empirical reporting corrections, not IQ physics. |
| Report limits | Values below −25 dB are floored at −25 dB after the false-decode check. | Upper cap +49 dB; lower floors −23 dB without AP or −24 dB with AP; FT8S-specific branches include a −26 dB floor. |
| False-decode selection | Sync-count gates, LDPC/CRC and payload checks; `nsync <= 10` together with pre-floor SNR below −25 dB rejects a likely false decode. This is distinct from the subsequent report floor. | LDPC/CRC, hard-error/distance-derived `qual`, timing, AP mode, message/callsign checks and SNR-dependent branches. These affect message acceptance, not just presentation. |

Applicable to raw complex IQ: use CRC-decoded tones, include Costas and data,
refine timing/frequency, debias ON-filter power with a measured local background,
and convert using the **actual filter ENBW / 2500 Hz**. Measure original samples
even when cancellation helped recover the payload. Complex two-sided noise and
real-audio one-sided noise need their own consistent injected-power accounting.

Do not import audio normalization constants, lower-percentile offsets, denominator
floors, nonlinear report corrections or report clamps. In particular, copying a
low-envelope selection without calibrating its statistical selection bias would
inflate SNR. Our median initializer uses exact exponential order expectations;
the upper-tail fit corrects the truncated exponential conditional mean. Its
four-scale cutoff and twelve-iteration cap control robustness/work, not the
permitted SNR interval. Tested numerical bias is recorded independently.

The meter does not replace or add message rejection gates. Existing refined
Costas synchronization, coherent joint metrics, BP/OSD and LDPC/CRC remain in the
decoder; meter failure only annotates a successful message. The implemented
quality flags are diagnostics, not equivalents of WSJT-X/JTDX decoder confidence.

Limits remain: dense majority contamination can conceal thermal noise; exact
co-channel power is inseparable; sharp PSD curvature, residual frequency error,
GFSK mismatch and fading can bias a local estimate. No finite quality flag turns
this into an exact measurement or a guaranteed error bound. See [SNR](SNR.md).
