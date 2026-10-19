//! The frames a VAD gate held back, kept for the spurt that is about to open
//! it. Voice is only recognized a block or two into a word, and with an
//! attack time the gate waits longer still, so the start of the word is in
//! frames that were not sent; they go out first when the gate opens.

use std::collections::VecDeque;

/// The last few unsent frames, oldest first.
#[derive(Debug, Default)]
pub struct PreRoll {
    frames: VecDeque<Vec<f32>>,
}

impl PreRoll {
    /// Keeps `frame` (of the current frame length) as pre-roll, at most
    /// `depth` of them. A frame of another length than the ones held (the
    /// frame size changed) starts the ring over.
    pub fn push(&mut self, frame: &[f32], depth: usize) {
        if self.frames.front().is_some_and(|f| f.len() != frame.len()) {
            self.frames.clear();
        }
        while self.frames.len() >= depth.max(1) {
            self.frames.pop_front();
        }
        self.frames.push_back(frame.to_vec());
    }

    /// Forgets everything: a frame went out, or the mic was muted — nothing
    /// captured while muted may leave the machine.
    pub fn clear(&mut self) {
        self.frames.clear();
    }

    /// The last `n` held frames, oldest first; the ring is emptied.
    pub fn take(&mut self, n: usize) -> Vec<Vec<f32>> {
        let keep_from = self.frames.len().saturating_sub(n);
        let out = self.frames.drain(keep_from..).collect();
        self.frames.clear();
        out
    }
}

/// How many frames of pre-roll to keep for an attack of up to `attack_ms`:
/// the frames the gate holds back, one for the onset RNNoise reacts to late,
/// and one for the jitter in the frame clock.
pub fn depth(attack_ms: u32, frame_ms: u32) -> usize {
    (attack_ms.div_ceil(frame_ms.max(1)) + 2) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_frames_and_hands_them_out_oldest_first() {
        let mut r = PreRoll::default();
        for i in 0..5 {
            r.push(&[i as f32; 4], 3);
        }
        let out = r.take(2);
        assert_eq!(out, vec![vec![3.0; 4], vec![4.0; 4]]);
        assert!(r.take(2).is_empty(), "taken once");
    }

    #[test]
    fn take_more_than_held_returns_what_there_is() {
        let mut r = PreRoll::default();
        r.push(&[1.0; 4], 3);
        assert_eq!(r.take(5).len(), 1);
    }

    #[test]
    fn clear_and_a_new_frame_length_drop_the_old_frames() {
        let mut r = PreRoll::default();
        r.push(&[1.0; 4], 3);
        r.clear();
        assert!(r.take(1).is_empty());
        r.push(&[1.0; 4], 3);
        r.push(&[2.0; 8], 3);
        assert_eq!(r.take(5), vec![vec![2.0; 8]]);
    }

    #[test]
    fn depth_covers_the_attack() {
        assert_eq!(depth(0, 20), 2);
        assert_eq!(depth(40, 20), 4);
        assert_eq!(depth(80, 10), 10);
    }
}
