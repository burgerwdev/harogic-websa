//! Streaming push front end over the batch decoder.
//!
//! This module is this project's addition. The upstream pieces it drives — the demodulator, the
//! header/sync search, the tone cutter — are already step-wise state machines; this module owns them
//! across sample blocks, so each scanline leaves the decoder as soon as its sequence pass completes.
//! Upstream files change visibility only, plus the documented `pull`/`read_available`/`pop`/`is_done`
//! additions in `assembler`.
//!
//! The sample source is a shared queue: the caller pushes decoded baseband in, and the decoder pulls
//! from it within the same call. An empty queue means "no more yet", never "the stream ended" — the
//! state machines keep their position between calls.
//!
//! Rows are assembled here rather than by the upstream `RowWalk`, because a sequence pass that runs
//! out of tones mid-scan would lose the tones it already consumed. This walker keeps the partial scan
//! and the finished scans of the current pass as bounded state, and nothing is lost across a pause.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use std::sync::{Arc, Mutex};

use super::assembler::{ImageSearch, ImageTones};
use super::demodulator::Demodulator;
use crate::digital::sstv::vendor::modes::step::Channel;
use crate::digital::sstv::vendor::modes::step::Step;
use crate::digital::sstv::vendor::modes::{frequency_value, Mode};
use crate::digital::sstv::vendor::units::Tone;
use crate::digital::sstv::vendor::RgbPixel;

/// Sample source the decoder pulls from. `Arc<Mutex<..>>` because the decoder must be `Send`; the
/// lock is uncontended (one thread pushes, the same thread pulls inside the push).
#[derive(Clone)]
struct Feed(Arc<Mutex<VecDeque<i16>>>);

impl Feed {
    fn push(&self, samples: &[i16]) {
        self.0.lock().expect("feed queue").extend(samples.iter().copied());
    }

    fn clear(&self) {
        self.0.lock().expect("feed queue").clear();
    }
}

impl Iterator for Feed {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        self.0.lock().expect("feed queue").pop_front()
    }
}

/// One completed scanline.
pub struct StreamRow {
    /// The row's position in the picture, 0 at the top.
    pub index: u32,
    pub pixels: Vec<RgbPixel>,
}

/// What one [`push`](StreamingDecoder::push) produced, beside the rows themselves.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StreamEvents {
    /// A header or sync lock was acquired: a transmission found and its mode known.
    pub acquired: bool,
    /// The picture's last scanline passed: the transmission is over.
    pub completed: bool,
    /// The transmission's tones ran out before the picture's last row: the signal ended early. The
    /// next picture starts a new one.
    pub abandoned: bool,
}

/// The tones of the picture as they are cut, pulled on demand.
struct Tones(ImageTones<Demodulator<Feed>>);

impl Tones {
    fn is_done(&self) -> bool {
        self.0.is_done()
    }
}

impl Iterator for Tones {
    type Item = Tone;

    fn next(&mut self) -> Option<Tone> {
        self.0.read_available();
        self.0.pop()
    }
}

/// Assembles tones into scanlines, one timing-sequence pass at a time, keeping the partial pass as
/// bounded state between pushes.
struct RowBuilder {
    mode: Mode,
    /// Position in the mode's timing sequence.
    step: usize,
    /// Pixel values collected for the current scan step.
    values: Vec<u8>,
    /// The finished scans of the current pass.
    scans: Vec<(Channel, Vec<u8>)>,
    /// Rows of the current pass waiting to be handed out.
    completed: VecDeque<Vec<RgbPixel>>,
    rows_left: usize,
}

impl RowBuilder {
    fn new(mode: Mode) -> Self {
        Self {
            mode,
            step: 0,
            values: Vec::new(),
            scans: Vec::new(),
            completed: VecDeque::new(),
            rows_left: mode.resolution().1 as usize,
        }
    }

    /// Feed one tone; hands out the rows its pass completed.
    fn tone(&mut self, tone: Tone, rows: &mut Vec<StreamRow>) {
        match self.mode.sequence[self.step] {
            Step::Control(_) => self.step += 1,
            Step::Scan(channel, _) => {
                self.values.push(frequency_value(tone.frequency));
                if self.values.len() == self.mode.resolution().0 as usize {
                    let values = core::mem::take(&mut self.values);
                    self.scans.push((channel, values));
                    self.step += 1;
                }
            }
        }
        if self.step == self.mode.sequence.len() {
            self.step = 0;
            let scans = core::mem::take(&mut self.scans);
            for row in super::rows::assemble_rows(self.mode.color, &scans) {
                // One index per ROW (a pass emits 1 or 2): a shared index let the second row of the
                // pass overwrite the first, and every other scanline decoded to black.
                rows.push(StreamRow {
                    index: self.mode.resolution().1 - self.rows_left as u32,
                    pixels: row,
                });
                self.rows_left -= 1;
            }
        }
    }
}

enum Phase {
    /// Looking for the next picture: its calibration header, or (with a mode pinned) its line sync
    /// pulses.
    Search(Box<ImageSearch<Demodulator<Feed>>>),
    /// A picture is on the air; its builder consumes the cut tones.
    Walk(Box<RowBuilder>, Box<Tones>),
}

/// Push-decode SSTV: samples in, scanlines out.
pub struct StreamingDecoder {
    feed: Feed,
    rate: u32,
    /// The mode pinned by the operator, or `None` to take it from the VIS header.
    pinned: Option<Mode>,
    phase: Phase,
    /// The mode of the picture on the air (the last one seen between pictures).
    mode: Option<Mode>,
    /// Scanlines handed out for the picture on the air.
    lines: u32,
    /// Headers or sync locks acquired: transmissions found and decoding started.
    acquisitions: u32,
    /// Set when a picture's last scanline passed; cleared when the next one starts.
    complete: bool,
}

impl StreamingDecoder {
    /// Take the mode from the VIS header.
    pub fn new(rate: u32) -> Self {
        Self::with_pinned(rate, None)
    }

    /// Search by line sync for `mode` (a missing or unreadable header) instead of trusting the header.
    pub fn with_pinned(rate: u32, pinned: Option<Mode>) -> Self {
        let feed = Feed(Arc::new(Mutex::new(VecDeque::new())));
        let search = Self::search(&feed, rate, pinned);
        Self {
            feed,
            rate,
            pinned,
            phase: search,
            mode: None,
            lines: 0,
            acquisitions: 0,
            complete: false,
        }
    }

    fn search(feed: &Feed, rate: u32, pinned: Option<Mode>) -> Phase {
        Phase::Search(Box::new(ImageSearch::new(
            Demodulator::new(feed.clone(), rate),
            rate,
            pinned,
        )))
    }

    /// The mode of the picture on the air (the last one seen between pictures).
    pub const fn mode(&self) -> Option<Mode> {
        self.mode
    }

    /// Scanlines handed out for the picture on the air.
    pub const fn lines(&self) -> u32 {
        self.lines
    }

    /// Transmissions found (a header or sync lock acquired) since creation or reset.
    pub const fn acquisitions(&self) -> u32 {
        self.acquisitions
    }

    /// Whether the picture on the air has all its scanlines.
    pub const fn complete(&self) -> bool {
        self.complete
    }

    /// Re-pin the decoder (`None` = the VIS header decides). The search restarts at the current
    /// position; audio already decoded stays on screen.
    pub fn set_pinned(&mut self, pinned: Option<Mode>) {
        if self.pinned == pinned {
            return;
        }
        self.pinned = pinned;
        self.complete = false;
        self.phase = Self::search(&self.feed, self.rate, self.pinned);
    }

    /// Drop the decoder state: a new signal, nothing buffered.
    pub fn reset(&mut self) {
        self.feed.clear();
        self.phase = Self::search(&self.feed, self.rate, self.pinned);
        self.mode = None;
        self.lines = 0;
        self.acquisitions = 0;
        self.complete = false;
    }

    /// Feed samples; returns the scanlines completed by them, with the phase events.
    pub fn push(&mut self, samples: &[i16]) -> (Vec<StreamRow>, StreamEvents) {
        self.feed.push(samples);
        self.drain()
    }

    /// End of the transmission: cut the sequences whose scans have begun, with the missing tail
    /// standing in from the newest estimate, and hand out what that completes. A picture short of its
    /// last row reports `abandoned`; the next transmission starts a new one.
    pub fn finish(&mut self) -> (Vec<StreamRow>, StreamEvents) {
        let mut rows = Vec::new();
        let mut events = StreamEvents::default();
        if let Phase::Walk(walk, tones) = &mut self.phase {
            while !tones.0.is_done() {
                tones.0.read();
            }
            let drained = self.drain();
            rows = drained.0;
            events = drained.1;
        }
        (rows, events)
    }

    fn drain(&mut self) -> (Vec<StreamRow>, StreamEvents) {
        let mut rows = Vec::new();
        let mut events = StreamEvents::default();
        loop {
            match &mut self.phase {
                Phase::Search(search) => {
                    // The replacement track the search is left with is discarded, so it reads from an
                    // empty source: a live one would consume a sample at the picture's first row.
                    let spare = Feed(Arc::new(Mutex::new(VecDeque::new())));
                    let rate = self.rate;
                    match search.pull(|| Demodulator::new(spare, rate)) {
                        Some(tones) => {
                            let mode = tones.mode();
                            self.mode = Some(mode);
                            self.lines = 0;
                            self.complete = false;
                            self.acquisitions += 1;
                            events.acquired = true;
                            let walk = RowBuilder::new(mode);
                            self.phase = Phase::Walk(Box::new(walk), Box::new(Tones(tones)));
                        }
                        None => break,
                    }
                }
                Phase::Walk(walk, tones) => {
                    let height = self.mode.map_or(0, |mode| mode.resolution().1);
                    if self.lines >= height {
                        events.completed = true;
                        self.complete = true;
                        self.phase = Self::search(&self.feed, self.rate, self.pinned);
                        continue;
                    }
                    match tones.next() {
                        Some(tone) => {
                            let before = rows.len();
                            walk.tone(tone, &mut rows);
                            self.lines += (rows.len() - before) as u32;
                        }
                        None => {
                            // Queue empty. A set `done` means the transmission's tones ran out before
                            // the picture's last row: the signal was lost, and the next picture starts
                            // a new one. Otherwise more samples are needed.
                            if tones.is_done() {
                                events.abandoned = true;
                                self.phase = Self::search(&self.feed, self.rate, self.pinned);
                                continue;
                            }
                            break;
                        }
                    }
                }
            }
        }
        (rows, events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::sstv::vendor::encode::{Encoder, Synthesizer};
    use crate::digital::sstv::vendor::modes::ROBOT_36;
    use crate::digital::sstv::vendor::RgbPixel;

    const RATE: u32 = 8_000;

    fn robot36_baseband() -> Vec<i16> {
        let (width, height) = (ROBOT_36.resolution().0 as usize, ROBOT_36.resolution().1 as usize);
        let pixels: Vec<RgbPixel> = (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| {
                    RgbPixel::new(
                        (x * 255 / width) as u8,
                        (y * 255 / height) as u8,
                        ((x + y) * 255 / (width + height)) as u8,
                    )
                })
            })
            .collect();
        let encoder = Encoder::new(ROBOT_36, pixels.into_iter()).expect("encoder");
        let samples: Vec<i16> = Synthesizer::new(encoder, RATE).collect();
        samples
    }

    #[test]
    fn streams_rows_from_a_transmission() {
        let mut decoder = StreamingDecoder::new(RATE);
        let iq = robot36_baseband();
        let mut total = 0usize;
        let mut first_row_at = usize::MAX;
        let mut contiguous = true;
        for (block, chunk) in iq.chunks(4096).enumerate() {
            let (rows, _) = decoder.push(chunk);
            if !rows.is_empty() && first_row_at == usize::MAX {
                first_row_at = block;
            }
            for row in &rows {
                if row.index as usize != total {
                    contiguous = false;
                }
                total += 1;
            }
        }
        // End of the transmission: the caller declares it, and the tail rows are cut.
        let (tail, events) = decoder.finish();
        assert!(events.completed, "the transmission must complete at end of stream");
        for row in &tail {
            if row.index as usize != total {
                contiguous = false;
            }
            total += 1;
        }
        let height = ROBOT_36.resolution().1;
        assert_eq!(decoder.mode(), Some(ROBOT_36));
        assert!(decoder.complete(), "{} of {} rows", total, height);
        assert_eq!(total, height as usize, "row count");
        assert!(contiguous, "row indices must be contiguous");
        // First rows well before the transmission ends: block = 4096 samples = 0.512 s.
        let first_row_seconds = first_row_at as f64 * 4096.0 / f64::from(RATE);
        assert!(first_row_seconds < 10.0, "first row at {first_row_seconds}s");
    }
}








