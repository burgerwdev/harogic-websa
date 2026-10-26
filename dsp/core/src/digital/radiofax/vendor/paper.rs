// Copyright (c) 2026 boybook and Rasterwave contributors
// SPDX-License-Identifier: MIT

/// Why a divider was inserted into a continuous decoded raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PaperBoundaryKind {
    /// The paper began with fallback or manually selected parameters.
    Initial,
    /// A valid SSTV VIS header established a trusted capture.
    Vis,
    /// Stable, unambiguous SSTV sync timing established a trusted capture.
    SyncTiming,
    /// Radiofax APT and phasing established a trusted capture.
    AptPhasing,
    /// A trusted transmission reached its protocol-defined end.
    ProtocolEnd,
    /// Input time continuity was broken.
    Discontinuity,
    /// The caller explicitly reset the decoder.
    Reset,
}
