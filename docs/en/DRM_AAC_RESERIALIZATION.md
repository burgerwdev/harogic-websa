# DRM AAC re-serialization (MPEG-4 → DRM syntax) — design notes

DRM30 uses a *different* AAC syntax from MPEG-4: the spectral data is carried with
**HCR** (Huffman Code Reordering) and the section data with **VCB11** (11-bit
codebook indices), the SBR payload is moved to the end of the frame, and a CRC-8
covers specific side-information bit ranges. fdK-AAC has a *decoder* for this
(`tpdec_drm.cpp` + `aacdec_hcr*.cpp`) but **no encoder**, so a DRM-audio fixture
cannot be produced by re-using fdK-AAC directly. This note is the plan for an
independent reimplementation (ISO/IEC 14496-3 §4.5.4.2.4 / ES 201 980 §5.3.1.2),
which is the "rewrite" path.

## What the encoder must produce, per frame

1. **Section data as VCB11**: the per-section codebook is sent as an 11-bit value;
   section *lengths* are implicit (derived from the spectral data, unlike MPEG-4
   which sends explicit section lengths). Escape/extension codebooks are
   signalled differently.
2. **Spectral data as HCR**: the quantised spectral coefficients are Huffman-coded
   as in MPEG-4, but the codewords are *reordered* — grouped by codebook — and two
   side-info fields precede them: `reordered_spectral_data_length` and
   `longest_codeword_length`, followed by a segmentation grid that lets the
   decoder locate each codeword. (Inverse of `aacdec_hcr.cpp::HcrDecoder`.)
3. **SBR payload moved to the end** of the frame (its own 8-bit CRC comes from the
   SBR encoder; for a no-SBR AAC-LC signal this step is absent).
4. **`aac_crc_bits`**: CRC-8 (poly 0x1D, init 0xFFFF) over the side-information bit
   ranges the DRM decoder checks (`drmRead_CrcStartReg`/`CrcEndReg` in
   `tpdec_drm.cpp`), transmitted in front of the frame for `TT_DRM`.

## Implementation plan (independent, spec-based)

- **Step 1 — parse MPEG-4 AAC**: read the raw access unit (from fdK-AAC's MPEG-4
  encoder, or ffmpeg) at bit level: ICS info, section data (codebook per section),
  scale factors, and the spectral Huffman codewords. The Huffman codebook tables
  are the standard AAC tables (ISO 14496-3 §4.6.2); fdK-AAC's `aacdec_hufftab.h` /
  `aac_rom.cpp` are the reference.
- **Step 2 — VCB11**: re-encode the section codebook list as 11-bit entries.
- **Step 3 — HCR**: sort the codewords by codebook, compute `longest_codeword_length`
  and the segmentation grid, and emit the reordered spectral bits (inverse of the
  `HcrInit`/`HcrDecoder` ordering).
- **Step 4 — SBR + CRC**: append the SBR payload at the end and compute the
  side-info CRC-8 over the exact ranges `tpdec_drm.cpp` covers.

## References

- ISO/IEC 14496-3 §4.5.4.2.4 (HCR), §4.6.2 (Huffman tables).
- ES 201 980 §5.3.1.2 (DRM AAC frame + `aac_crc_bits`).
- fdK-AAC: `libAACdec/src/aacdec_hcr.cpp` (HCR decode), `libMpegTPDec/src/tpdec_drm.cpp`
  (CRC ranges), `libAACdec/src/aac_rom.cpp` (Huffman tables).
- DecDRM `FdkDrmEncoder` (GPL, algorithm reference only — not copyable).

A simpler milestone is to first implement Steps 1+2+4 for a single-codebook AAC-LC
frame (no SBR), then add HCR (Step 3) once the VCB11 framing round-trips through the
fdK-AAC `TT_DRM` decoder.
