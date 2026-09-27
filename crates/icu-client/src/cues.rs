//! Short cue tones rendered on the fly (join/leave, tx, mute), and the
//! queue the mixer plays them from.

use std::collections::VecDeque;

/// Which cue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Someone joined.
    Join,
    /// Someone left.
    Leave,
    /// Started transmitting.
    TxStart,
    /// Stopped transmitting.
    TxStop,
    /// Muted.
    Mute,
    /// Unmuted.
    Unmute,
}

/// Renders the cue as mono 48 kHz samples at −18 dBFS.
pub fn render(cue: Cue) -> Vec<f32> {
    let (notes, dur_ms): (&[f32], u32) = match cue {
        Cue::Join => (&[660.0, 880.0], 70),
        Cue::Leave => (&[880.0, 660.0], 70),
        Cue::TxStart => (&[1000.0], 40),
        Cue::TxStop => (&[800.0], 40),
        Cue::Mute => (&[600.0, 400.0], 60),
        Cue::Unmute => (&[400.0, 600.0], 60),
    };
    let n = (crate::audio::SAMPLE_RATE * dur_ms / 1000) as usize;
    let mut out = Vec::with_capacity(n * notes.len());
    for f in notes {
        for i in 0..n {
            let t = i as f32 / crate::audio::SAMPLE_RATE as f32;
            let env = (i as f32 / n as f32 * std::f32::consts::PI).sin();
            out.push((t * f * std::f32::consts::TAU).sin() * 0.125 * env);
        }
    }
    out
}

/// Most cue audio that may wait for the mixer: half a second.
pub const MAX_QUEUED: usize = 24_000;

/// Cues waiting for the mixer: at most [`MAX_QUEUED`] samples, and a cue
/// that is still waiting or playing is not queued a second time. A client
/// reconnecting in a loop costs every listener a leave and a join cue per
/// attempt; unbounded, they piled up faster than they played — endless
/// beeping and growing memory. Allocated once: queueing never allocates,
/// so the mixer thread may queue the transmit cues itself.
#[derive(Debug)]
pub struct CueQueue {
    samples: VecDeque<f32>,
    /// Every cue still (partly) queued, with the running sample count at
    /// which it ends. At most one per kind of cue.
    queued: Vec<(Cue, u64)>,
    pushed: u64,
    played: u64,
}

impl Default for CueQueue {
    fn default() -> Self {
        CueQueue::new()
    }
}

impl CueQueue {
    /// Empty, with room for [`MAX_QUEUED`] samples.
    pub fn new() -> CueQueue {
        CueQueue {
            samples: VecDeque::with_capacity(MAX_QUEUED),
            queued: Vec::with_capacity(8),
            pushed: 0,
            played: 0,
        }
    }

    /// Queues `cue`, rendered as `samples`, unless the same cue has not
    /// finished playing yet or it would not fit. Returns whether it was
    /// queued.
    pub fn push(&mut self, cue: Cue, samples: &[f32]) -> bool {
        let played = self.played;
        self.queued.retain(|&(_, end)| end > played);
        if self.queued.iter().any(|&(c, _)| c == cue)
            || self.samples.len() + samples.len() > MAX_QUEUED
        {
            return false;
        }
        self.samples.extend(samples.iter().copied());
        self.pushed += samples.len() as u64;
        self.queued.push((cue, self.pushed));
        true
    }

    /// The next block: `out` filled with queued samples, silence past their
    /// end. `false`, and `out` untouched, when nothing is queued.
    pub fn fill(&mut self, out: &mut [f32]) -> bool {
        if self.samples.is_empty() {
            return false;
        }
        for s in out.iter_mut() {
            *s = match self.samples.pop_front() {
                Some(v) => {
                    self.played += 1;
                    v
                }
                None => 0.0,
            };
        }
        true
    }

    /// Samples waiting.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Nothing waiting.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reconnect loop: every attempt brings a leave and a join cue. What
    /// is queued stays one of each until it has played, and never more
    /// than half a second in all; once played, a cue can come again.
    #[test]
    fn repeats_coalesce_and_the_queue_is_bounded() {
        let (join, leave) = (render(Cue::Join), render(Cue::Leave));
        let mut q = CueQueue::new();
        let mut queued = 0;
        for _ in 0..100 {
            queued += usize::from(q.push(Cue::Leave, &leave));
            queued += usize::from(q.push(Cue::Join, &join));
        }
        assert_eq!(queued, 2, "one leave and one join");
        assert_eq!(q.len(), leave.len() + join.len());
        assert!(q.push(Cue::Mute, &render(Cue::Mute)));
        assert!(
            !q.push(Cue::Unmute, &render(Cue::Unmute)),
            "beyond half a second"
        );
        assert!(q.len() <= MAX_QUEUED);

        let mut block = vec![0.0; 480];
        while q.len() > join.len() + render(Cue::Mute).len() {
            assert!(q.fill(&mut block));
        }
        assert!(q.push(Cue::Leave, &leave), "the leave cue has played");
        assert!(!q.push(Cue::Join, &join), "the join cue is still playing");
        while q.fill(&mut block) {}
        assert!(q.is_empty());
        assert!(q.push(Cue::Join, &join));
    }

    #[test]
    fn cues_are_short_and_bounded() {
        for c in [
            Cue::Join,
            Cue::Leave,
            Cue::TxStart,
            Cue::TxStop,
            Cue::Mute,
            Cue::Unmute,
        ] {
            let s = render(c);
            assert!(!s.is_empty() && s.len() < 48_000);
            assert!(s.iter().all(|v| v.abs() <= 0.13));
        }
    }
}
