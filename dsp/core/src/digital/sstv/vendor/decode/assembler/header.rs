//! Identifying the mode from the calibration header among the tones.

use crate::digital::sstv::vendor::modes::{LEADER, Mode, SYNC_FREQUENCY, VisCode};
use crate::digital::sstv::vendor::units::Tone;
use crate::digital::sstv::vendor::{ms, tone};
use super::correct_frequency;

/// Generous in duration, so a leader clipped by a trimmed recording still
/// counts.
const LEADER_TOLERANCE: Tone = tone!(50 Hz, 150 ms);
/// The stop bit closing the VIS code merges with the first line's sync
/// pulse, which has the same frequency and lasts up to 20 ms.
const STOP_BIT_AND_SYNC: Tone = Tone::new(SYNC_FREQUENCY, ms!(40));
/// Covers the stop bit alone (30 ms) up to the stop bit plus the longest
/// sync pulse (50 ms), with 10 ms of slack either side.
const STOP_BIT_AND_SYNC_TOLERANCE: Tone = tone!(50 Hz, 20 ms);
/// The calibration header spans at most this many tones: leader, break,
/// leader, start bit, eight data bits and stop bit.
const HEADER_TONES: usize = 13;
// Margin from the 2300 Hz white tone to the decoder band's 2700 Hz edge.
const MAX_HEADER_OFFSET_HZ: i64 = 400;

/// The most recent tones, watched for a calibration header.
pub(super) struct HeaderWindow {
    /// Oldest first.
    tones: [Tone; HEADER_TONES],
    /// The mode a header must announce to count; `None` accepts any.
    expected_mode: Option<Mode>,
    frequency_offset_hz: i32,
}

impl HeaderWindow {
    pub const fn new(expected_mode: Option<Mode>) -> Self {
        Self {
            tones: [tone!(0 Hz, 0 ns); HEADER_TONES],
            expected_mode,
            frequency_offset_hz: 0,
        }
    }

    /// Take in the next tone. Returns the mode once this tone completes a
    /// header announcing it.
    pub fn push(&mut self, tone: Tone) -> Option<Mode> {
        self.tones.copy_within(1.., 0);
        self.tones[HEADER_TONES - 1] = tone;

        let (mode, offset) = identify_header(&self.tones)?;
        if self.expected_mode.is_some_and(|expected| expected != mode) { return None; }
        self.frequency_offset_hz = offset;
        Some(mode)
    }

    pub const fn frequency_offset_hz(&self) -> i32 {
        self.frequency_offset_hz
    }
}

/// The mode announced by a header ending at the newest tone.
fn identify_header(tones: &[Tone; HEADER_TONES]) -> Option<(Mode, i32)> {
    // The 1900 Hz leader calibrates tuning/LO offset. Framing, data-bit
    // spacing, duration and parity retain their original strict tolerances.
    for leader in tones.iter().rev() {
        let offset = i64::from(leader.frequency.hz()) - i64::from(LEADER.frequency.hz());
        if offset.abs() > MAX_HEADER_OFFSET_HZ
            || leader.duration.ns().abs_diff(LEADER.duration.ns()) > LEADER_TOLERANCE.duration.ns() {
            continue;
        }
        let offset = offset as i32;
        let relative = tones.map(|tone| Tone::new(correct_frequency(tone.frequency, offset), tone.duration));
        let Some(data_bits) = find_vis_data_bits(&relative) else { continue; };
        let Some(code) = VisCode::from_received_tones(data_bits) else { continue; };
        if let Ok(mode) = Mode::try_from(code) { return Some((mode, offset)); }
    }
    None
}

/// The VIS data-bit tones of a header ending at the newest tone: the tones
/// between a start bit that follows a leader and the stop bit. The break and
/// first leader are not required, so a recording trimmed at the front is
/// still identified.
fn find_vis_data_bits(tones: &[Tone]) -> Option<&[Tone]> {
    let (stop_bit, earlier) = tones.split_last()?;
    if !stop_bit.is_near(STOP_BIT_AND_SYNC, STOP_BIT_AND_SYNC_TOLERANCE) {
        return None;
    }

    let start_bit = earlier
        .iter()
        .rposition(|tone| tone.is_near(VisCode::FRAMING_BIT, VisCode::BIT_TOLERANCE))?;
    let leader = earlier.get(..start_bit)?.last()?;
    if !leader.is_near(LEADER, LEADER_TOLERANCE) {
        return None;
    }

    earlier
        .get(start_bit + 1..)
        .filter(|data_bits| !data_bits.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digital::sstv::vendor::modes::{MARTIN_1, ROBOT_36};

    #[test]
    fn identifies_the_mode_at_the_headers_last_tone() {
        let mut window = HeaderWindow::new(None);

        let identified = push_header(&mut window, ROBOT_36);

        assert_eq!(identified, Some(ROBOT_36));
    }

    #[test]
    fn ignores_headers_announcing_another_mode_than_expected() {
        let mut window = HeaderWindow::new(Some(MARTIN_1));

        let identified = push_header(&mut window, ROBOT_36);

        assert_eq!(identified, None);
    }

    #[test]
    fn leader_calibrates_bounded_offsets_without_accepting_bad_parity() {
        for offset in [-425, -400, -65, 0, 65, 400, 425] {
            for corrupt in [false, true] {
                let mut window = HeaderWindow::new(None);
                let (mut start_seen, mut changed) = (false, false);
                let mut found = None;
                for mut tone in ROBOT_36.header_tones() {
                    if tone == VisCode::FRAMING_BIT { start_seen = true; }
                    else if start_seen && corrupt && !changed {
                        let hz = if tone.frequency.hz() == 1100 {1300} else {1100};
                        tone.frequency = crate::digital::sstv::vendor::units::Frequency::from_hz(hz);
                        changed = true;
                    }
                    tone.frequency = correct_frequency(tone.frequency, -offset);
                    found = window.push(tone).or(found);
                }
                if !corrupt && offset.abs() <= MAX_HEADER_OFFSET_HZ as i32 {
                    assert_eq!(found, Some(ROBOT_36), "offset={offset}");
                    assert_eq!(window.frequency_offset_hz(), offset);
                } else {
                    assert_eq!(found, None, "offset={offset}, corrupt={corrupt}");
                }
            }
        }
    }

    #[test]
    fn identifies_a_header_trimmed_down_to_its_second_leader() {
        let mut window = HeaderWindow::new(None);
        let second_leader_on = ROBOT_36
            .header_tones()
            .skip_while(|tone| *tone != LEADER)
            .skip(2);

        let identified = second_leader_on
            .map(|tone| window.push(tone))
            .last()
            .flatten();

        assert_eq!(identified, Some(ROBOT_36));
    }

    /// Push the mode's header tones, returning what the last one identified.
    fn push_header(window: &mut HeaderWindow, mode: Mode) -> Option<Mode> {
        mode.header_tones()
            .map(|tone| window.push(tone))
            .last()
            .flatten()
    }
}
