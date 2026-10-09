use std::collections::VecDeque;

use anyhow::{Result, ensure};

use crate::SAMPLE_RATE;

/// Places asynchronous capture packets on a continuous sample clock, including silence.
pub(super) struct AudioTimeline {
    samples: VecDeque<f32>,
    cursor: u64,
}

impl AudioTimeline {
    pub fn new() -> Self { Self { samples: VecDeque::new(), cursor: 0 } }

    pub fn push(&mut self, start: i64, samples: &[f32]) -> Result<()> {
        let skip = (i128::from(self.cursor) - i128::from(start)).max(0).min(samples.len() as i128) as usize;
        let samples = &samples[skip..];
        if samples.is_empty() { return Ok(()); }
        let start = (i128::from(start) + skip as i128).max(0) as u64;
        let offset = start.saturating_sub(self.cursor) as usize;
        ensure!(offset.saturating_add(samples.len()) <= SAMPLE_RATE as usize * 3,
            "System audio buffering exceeded three seconds; capture stopped to protect timing");
        self.samples.resize(self.samples.len().max(offset + samples.len()), 0.0);
        for (index, sample) in samples.iter().enumerate() { self.samples[offset + index] = *sample; }
        Ok(())
    }

    pub fn take_until(&mut self, target: u64, maximum: usize) -> Vec<f32> {
        let count = target.saturating_sub(self.cursor).min(maximum as u64) as usize;
        let available = count.min(self.samples.len());
        let mut samples: Vec<_> = self.samples.drain(..available).collect();
        samples.resize(count, 0.0);
        self.cursor += count as u64;
        samples
    }

    pub fn discard_pending(&mut self) { self.samples.clear(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_gaps_keep_their_place_on_the_sample_clock() {
        let mut timeline = AudioTimeline::new();
        timeline.push(3, &[0.2, 0.4]).unwrap();
        assert_eq!(timeline.take_until(7, 10), [0.0, 0.0, 0.0, 0.2, 0.4, 0.0, 0.0]);
        assert!(timeline.take_until(7, 10).is_empty());
    }

    #[test]
    fn late_and_pre_start_samples_do_not_shift_later_audio() {
        let mut timeline = AudioTimeline::new();
        timeline.push(-2, &[1.0, 1.0, 0.1, 0.2]).unwrap();
        assert_eq!(timeline.take_until(3, 10), [0.1, 0.2, 0.0]);
        timeline.push(1, &[0.1, 0.2, 0.3, 0.4]).unwrap();
        assert_eq!(timeline.take_until(5, 10), [0.3, 0.4]);
    }

    #[test]
    fn bounded_reads_and_pause_clocks_preserve_pending_audio() {
        let mut timeline = AudioTimeline::new();
        timeline.push(0, &[0.5; 8]).unwrap();
        assert_eq!(timeline.take_until(4, 2), [0.5; 2]);
        assert_eq!(timeline.take_until(4, 10), [0.5; 2]);
        assert!(timeline.take_until(4, 10).is_empty());
        assert_eq!(timeline.take_until(8, 10), [0.5; 4]);
    }

    #[test]
    fn invalid_future_packets_fail_without_allocating_unbounded_memory() {
        let mut timeline = AudioTimeline::new();
        assert!(timeline.push(i64::MAX, &[1.0]).is_err());
        assert!(timeline.push(0, &vec![0.0; SAMPLE_RATE as usize * 3 + 1]).is_err());
        assert_eq!(timeline.take_until(2, 2), [0.0, 0.0]);
    }

    #[test]
    fn samples_crossing_a_pause_boundary_do_not_leak_into_resume() {
        let mut timeline = AudioTimeline::new();
        timeline.push(0, &[0.5; 8]).unwrap();
        assert_eq!(timeline.take_until(4, 10), [0.5; 4]);
        timeline.discard_pending();
        timeline.push(4, &[0.2; 2]).unwrap();
        assert_eq!(timeline.take_until(6, 10), [0.2; 2]);
    }
}
