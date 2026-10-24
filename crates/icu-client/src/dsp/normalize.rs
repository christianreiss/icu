//! Loudness normalization: brings a voice to a common level.
//!
//! Three uses, one per [`Profile`]: receive side (each remote speaker before
//! their volume slider and the mix), mic auto leveling (the automatic input
//! gain ahead of denoise and VAD) and outbound normalization (right before
//! the encoder, only while the mic leveler is off — `dsp::chain`). The
//! level estimate moves on VAD-confirmed speech or blocks above the voice's noise
//! floor (the minimum block level of the last 1.5–3 s of sound, so steady
//! hiss never counts as speech) and, on the send side, only while the VAD
//! hears voice; in pauses the gain holds, so nothing pumps or lifts room
//! noise. The estimate follows an upper envelope — fast up so a loud onset
//! is caught early, slow down so the gain does not breathe between words.
//!
//! One leveler per direction. Two fast-attack/slow-release stages in
//! series fight: on a louder voice both cut at once, then the downstream
//! one releases only as slowly as the upstream one takes over, and the
//! voice sags for seconds (measured 6 dB after a +15 dB jump,
//! `tests/cascade.rs`). So the send side runs one leveler, and the receive
//! side, which meets voices the sender already leveled, attacks slower and
//! leaves a ±2 dB dead band around the target untouched. 48 kHz, any block
//! length, allocates nothing.

use super::meter::{from_db, measure, to_db};

/// Target, bounds and speed of one normalizer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Profile {
    /// Target of the level estimate in dBFS: the RMS of 10 ms blocks,
    /// followed with the profile's rise time — a fast rise tracks the loud
    /// blocks of every syllable, so the voice's RMS lands a few dB lower.
    pub target_db: f32,
    /// Gain bounds in dB.
    pub max_boost_db: f32,
    pub max_cut_db: f32,
    /// Envelope time constants in seconds.
    pub rise_s: f32,
    pub fall_s: f32,
    /// No gain while the level is this close to the target; beyond it, the
    /// correction starts from zero (no jump at the edge).
    pub dead_band_db: f32,
    /// Blocks below this level never count as speech, whatever the floor.
    pub speech_min_db: f32,
}

impl Profile {
    /// Each remote speaker, in the mixer: a backstop behind the sender's
    /// own leveling, so it must not fight it (see the module docs). Voices
    /// the sender leveled arrive within its dead band and pass untouched;
    /// their denoised pauses are near digital silence, so the absolute
    /// speech floor, not the noise floor, keeps consonants and tails from
    /// dragging the estimate down.
    pub const RECEIVE: Profile = Profile {
        target_db: -22.0,
        max_boost_db: 12.0,
        max_cut_db: -12.0,
        rise_s: 0.6,
        fall_s: 2.0,
        dead_band_db: 2.0,
        speech_min_db: -40.0,
    };
    /// Mic auto leveling: down within a word (a louder voice is caught
    /// before the receive side reacts), up over seconds. −20 dBFS on the
    /// block envelope puts the voice's RMS near −23 dBFS.
    pub const MIC_LEVEL: Profile = Profile {
        target_db: -20.0,
        max_boost_db: 15.0,
        max_cut_db: -15.0,
        rise_s: 0.2,
        fall_s: 4.0,
        dead_band_db: 0.0,
        speech_min_db: -50.0,
    };
    /// Own voice right before the encoder, when the mic leveler is off.
    pub const OUTBOUND: Profile = Profile {
        target_db: -20.0,
        max_boost_db: 12.0,
        max_cut_db: -12.0,
        rise_s: 0.08,
        fall_s: 2.0,
        dead_band_db: 0.0,
        speech_min_db: -50.0,
    };
}

/// A block counts as speech this far above the noise floor.
const SPEECH_MARGIN_DB: f32 = 10.0;
/// Floor = minimum over the current and the previous window of this length.
const FLOOR_WINDOW_S: f32 = 1.5;
const SR: f32 = 48_000.0;

/// Per-block smoothing factor for a time constant, given the block length.
fn alpha(seconds: f32, samples: usize) -> f32 {
    1.0 - (-(samples as f32) / (seconds * SR)).exp()
}

/// Loudness normalizer for one voice.
#[derive(Debug, Clone)]
pub struct Normalizer {
    profile: Profile,
    /// Speech envelope in dBFS; starts at the target (unity gain).
    level_db: f32,
    /// Linear gain applied at the end of the last block.
    gain: f32,
    win_min: f32,
    prev_win_min: f32,
    /// Samples seen in the current floor window.
    win_samples: usize,
    headroom_limited: bool,
    voicing: super::voicing::Voicing,
    unhinted: bool,
}

impl Default for Normalizer {
    fn default() -> Self {
        Normalizer::new()
    }
}

impl Normalizer {
    /// Receive-side normalizer ([`Profile::RECEIVE`]).
    pub fn new() -> Normalizer {
        Normalizer::with(Profile::RECEIVE)
    }

    /// Fresh state: unity gain until speech has been heard.
    pub fn with(profile: Profile) -> Normalizer {
        Normalizer {
            profile,
            level_db: profile.target_db,
            gain: 1.0,
            win_min: f32::INFINITY,
            prev_win_min: f32::INFINITY,
            win_samples: 0,
            headroom_limited: false,
            voicing: Default::default(),
            unhinted: true,
        }
    }

    /// Clears all state (when switched off, so re-enabling starts clean).
    pub fn reset(&mut self) {
        *self = Normalizer::with(self.profile);
    }

    /// Current gain in dB (for tests and diagnostics).
    pub fn gain_db(&self) -> f32 {
        to_db(self.gain)
    }

    /// Whether microphone boost was restricted to retain pre-denoiser headroom.
    pub fn headroom_limited(&self) -> bool {
        self.headroom_limited
    }
    /// Seed the estimate only, never the noise floor; used for a validated device profile.
    pub fn warm_start(&mut self, db: f32) {
        if db.is_finite() {
            self.level_db = self.profile.target_db - db.clamp(self.profile.max_cut_db, 6.0);
            self.gain = 1.0;
        }
    }
    /// Processes one block in place.
    pub fn process(&mut self, block: &mut [f32]) {
        self.process_hinted(block, None);
    }

    /// Measures without changing samples; shared by feedback and actual leveling.
    pub fn observe(&mut self, block: &[f32], voice: Option<bool>) -> bool {
        if block.is_empty() || block.iter().any(|x| !x.is_finite()) {
            self.unhinted = false;
            return false;
        }
        let rms_db = measure(block).rms_db;
        let voice = if voice.is_some() {
            self.unhinted = false;
            voice
        } else {
            if !self.unhinted {
                self.voicing = Default::default();
                self.unhinted = true;
            }
            self.voicing.observe(block).then_some(true)
        };

        // Exact silence (jitter-buffer gaps, a closed gate) says nothing
        // about the speaker's noise floor: neither counted nor clocked, so
        // the floor survives a pause instead of being forgotten after 3 s.
        // Confirmed speech cannot teach us the noise floor. Otherwise a
        // sustained phrase eventually becomes its own floor and freezes
        // leveling (and the local measurement needed for peer assistance).
        if voice != Some(true) && rms_db > -99.0 {
            self.win_min = self.win_min.min(rms_db);
            self.win_samples += block.len();
        }
        if self.win_samples as f32 >= FLOOR_WINDOW_S * SR {
            self.prev_win_min = self.win_min;
            self.win_min = f32::INFINITY;
            self.win_samples = 0;
        }
        let floor = self.win_min.min(self.prev_win_min);

        let p = self.profile;
        let eligible = rms_db > p.speech_min_db
            && match voice {
                Some(true) => true,
                Some(false) => false,
                None => rms_db > floor + SPEECH_MARGIN_DB,
            };
        if eligible {
            let a = if rms_db > self.level_db {
                alpha(p.rise_s, block.len())
            } else {
                alpha(p.fall_s, block.len())
            };
            self.level_db += a * (rms_db - self.level_db);
        }

        eligible
    }

    /// Unapplied correction with the same limits as the audio processor.
    pub fn correction_db(&self) -> f32 {
        let error = self.profile.target_db - self.level_db;
        (error.signum() * (error.abs() - self.profile.dead_band_db).max(0.0))
            .clamp(self.profile.max_cut_db, self.profile.max_boost_db)
    }

    /// Current speech envelope, in dBFS.
    pub fn level_db(&self) -> f32 {
        self.level_db
    }

    /// A confirmed capture gain change shifts both the estimate and its inverse gain.
    pub fn capture_changed(&mut self, db: f32) {
        self.level_db += db;
        self.win_min += db;
        self.prev_win_min += db;
        self.gain = from_db(
            (self.gain_db() - db).clamp(self.profile.max_cut_db, self.profile.max_boost_db),
        );
    }

    /// Like [`Normalizer::process`]; `Some(false)` (the VAD hears no voice)
    /// holds the level estimate. The noise floor keeps tracking either way.
    pub fn process_hinted(&mut self, block: &mut [f32], voice: Option<bool>) {
        if block.is_empty() {
            return;
        }
        for s in block.iter_mut() {
            if !s.is_finite() {
                *s = 0.0;
            }
        }
        self.observe(block, voice);
        let mut target = from_db(self.correction_db());
        let mut start = self.gain;
        self.headroom_limited = false;
        if self.profile == Profile::MIC_LEVEL {
            let peak = block.iter().fold(0.0f32, |p, x| p.max(x.abs()));
            if peak > 0.0 {
                let safe = from_db(-1.0) / peak;
                self.headroom_limited = target > safe || start > safe;
                target = target.min(safe);
                start = start.min(safe);
            }
        }
        let step = (target - start) / block.len() as f32;
        for (i, s) in block.iter_mut().enumerate() {
            *s *= start + step * (i + 1) as f32;
        }
        self.gain = target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 480;

    /// Speech-like test signal: 300 ms of 220 Hz tone at `db` dBFS RMS, then
    /// 100 ms of `noise_db` noise, repeated for `seconds`. Yields blocks.
    fn talk(db: f32, noise_db: f32, seconds: f32) -> Vec<Vec<f32>> {
        let amp = from_db(db) * std::f32::consts::SQRT_2;
        let noise = from_db(noise_db) * 3f32.sqrt();
        let mut rng = 0x1234_5678u32;
        let n = (seconds * SR) as usize / BLOCK;
        (0..n)
            .map(|b| {
                let speaking = (b % 40) < 30;
                (0..BLOCK)
                    .map(|i| {
                        rng ^= rng << 13;
                        rng ^= rng >> 17;
                        rng ^= rng << 5;
                        let r = (rng as f32 / u32::MAX as f32) * 2.0 - 1.0;
                        let t = (b * BLOCK + i) as f32 / SR;
                        let tone = if speaking {
                            amp * (t * 220.0 * std::f32::consts::TAU).sin()
                        } else {
                            0.0
                        };
                        tone + noise * r
                    })
                    .collect()
            })
            .collect()
    }

    fn run(n: &mut Normalizer, blocks: Vec<Vec<f32>>) -> Vec<Vec<f32>> {
        blocks
            .into_iter()
            .map(|mut b| {
                n.process(&mut b);
                b
            })
            .collect()
    }

    #[test]
    fn quiet_speaker_is_raised() {
        let mut n = Normalizer::with(Profile::OUTBOUND);
        run(&mut n, talk(-34.0, -70.0, 10.0));
        assert!(n.gain_db() > 9.0, "gain {}", n.gain_db());
    }

    /// Compressed vowels can have a constant block RMS and digitally silent
    /// pauses. They need evidence other than a lower nonzero noise floor.
    #[test]
    fn clean_contoured_voice_does_not_become_its_own_receive_noise_floor() {
        for (db, wanted) in [(-34.0, 10.0), (-10.0, -10.0)] {
            let mut n = Normalizer::new();
            let mut phase = 0.0f32;
            for b in 0..2000 {
                let mut block = [0.0; BLOCK];
                if b % 200 < 150 {
                    for (i, s) in block.iter_mut().enumerate() {
                        let t = (b * BLOCK + i) as f32 / SR;
                        let pitch = 180.0 + 20.0 * (t * std::f32::consts::TAU * 1.3).sin();
                        phase = (phase + pitch / SR).fract();
                        let p = phase * std::f32::consts::TAU;
                        *s = p.sin() + 0.4 * (2.0 * p).sin() + 0.2 * (3.0 * p).sin();
                    }
                    let scale = from_db(db) / from_db(measure(&block).rms_db);
                    for x in &mut block {
                        *x *= scale;
                    }
                }
                n.process(&mut block);
            }
            assert!((n.gain_db() - wanted).abs() < 1.0, "{db}: {}", n.gain_db());
        }
    }

    #[test]
    fn speech_recovers_after_noise_contaminated_the_receive_floor() {
        let mut n = Normalizer::new();
        run(&mut n, talk(-100.0, -22.0, 4.0));
        let mut phase = 0.0f32;
        for b in 0..1500 {
            let mut block = [0.0; BLOCK];
            for (i, s) in block.iter_mut().enumerate() {
                let t = (b * BLOCK + i) as f32 / SR;
                let pitch = 180.0 + 20.0 * (t * std::f32::consts::TAU * 1.3).sin();
                phase = (phase + pitch / SR).fract();
                let p = phase * std::f32::consts::TAU;
                *s = p.sin() + 0.4 * (2.0 * p).sin() + 0.2 * (3.0 * p).sin();
            }
            let scale = from_db(-34.0) / from_db(measure(&block).rms_db);
            for s in &mut block {
                *s *= scale;
            }
            n.process(&mut block);
        }
        assert!((9.0..10.1).contains(&n.gain_db()), "gain {}", n.gain_db());
    }

    #[test]
    fn receive_gain_agrees_for_ten_and_twenty_ms_blocks_and_clears_stale_voicing() {
        let mut phase = 0.0f32;
        let mut source = Vec::new();
        for b in 0..1500 {
            let mut block = [0.0; BLOCK];
            for (i, s) in block.iter_mut().enumerate() {
                let t = (b * BLOCK + i) as f32 / SR;
                let pitch = 180.0 + 20.0 * (t * std::f32::consts::TAU * 1.3).sin();
                phase = (phase + pitch / SR).fract();
                let p = phase * std::f32::consts::TAU;
                *s = p.sin() + 0.4 * (2.0 * p).sin() + 0.2 * (3.0 * p).sin();
            }
            let scale = from_db(-34.0) / from_db(measure(&block).rms_db);
            source.extend(block.map(|s| s * scale));
        }
        let mut gains = Vec::new();
        for quantum in [480, 960] {
            let mut n = Normalizer::new();
            for block in source.chunks(quantum) {
                n.process(&mut block.to_vec());
            }
            gains.push(n.gain_db());
            assert!(n.gain_db() > 9.0);
            n.observe(&[0.0; BLOCK], Some(false));
            assert!(
                !n.observe(&source[..BLOCK], None),
                "concealment must not freeze old voicing confidence"
            );
        }
        assert!((gains[0] - gains[1]).abs() < 0.1, "{gains:?}");
    }

    #[test]
    fn switched_tones_and_out_of_band_sweeps_do_not_raise_receive_gain() {
        for kind in 0..3 {
            let mut n = Normalizer::new();
            let mut phase = 0.0f32;
            for b in 0..1000 {
                let mut block = [0.0; BLOCK];
                for (i, s) in block.iter_mut().enumerate() {
                    let t = (b * BLOCK + i) as f32 / SR;
                    let hz = match kind {
                        0 => {
                            if b < 500 {
                                180.0
                            } else {
                                200.0
                            }
                        }
                        1 => 8000.0 + 180.0 + 20.0 * (t * std::f32::consts::TAU * 1.3).sin(),
                        _ => 16000.0 + 180.0 + 20.0 * (t * std::f32::consts::TAU * 1.3).sin(),
                    };
                    phase = (phase + hz / SR).fract();
                    *s = (phase * std::f32::consts::TAU).sin();
                }
                let scale = from_db(-34.0) / from_db(measure(&block).rms_db);
                for s in &mut block {
                    *s *= scale;
                }
                n.process(&mut block);
            }
            assert_eq!(n.gain_db(), 0.0, "interference kind {kind}");
        }
    }

    #[test]
    fn confirmed_sustained_speech_is_leveled_without_a_noise_only_pause() {
        let mut n = Normalizer::with(Profile::MIC_LEVEL);
        for _ in 0..2000 {
            let mut block = vec![from_db(-32.0); BLOCK];
            n.process_hinted(&mut block, Some(true));
        }
        assert!(n.gain_db() > 11.0, "gain {}", n.gain_db());
        // A louder phrase must still be caught immediately after the floor
        // windows have seen nothing but sustained speech.
        for _ in 0..100 {
            let mut block = vec![from_db(-10.0); BLOCK];
            n.process_hinted(&mut block, Some(true));
        }
        assert!(n.gain_db() < -9.0, "gain {}", n.gain_db());
    }

    #[test]
    fn confirmed_speech_still_requires_finite_samples_and_absolute_minimum() {
        let mut n = Normalizer::with(Profile::MIC_LEVEL);
        for block in [[0.0; BLOCK], [from_db(-60.0); BLOCK], [f32::NAN; BLOCK]] {
            assert!(!n.observe(&block, Some(true)));
        }
        assert_eq!(n.correction_db(), 0.0);
    }

    #[test]
    fn loud_speaker_is_lowered_quickly() {
        let mut n = Normalizer::with(Profile::OUTBOUND);
        run(&mut n, talk(-8.0, -70.0, 1.0));
        assert!(n.gain_db() < -9.0, "gain {}", n.gain_db());
    }

    /// The receive profile leaves a voice near the target alone and takes
    /// only the part beyond the dead band, without a jump at its edge.
    #[test]
    fn receive_dead_band() {
        let mut n = Normalizer::new();
        run(&mut n, talk(-23.5, -70.0, 10.0));
        assert_eq!(n.gain_db(), 0.0, "1.5 dB off the target is left alone");
        let mut n = Normalizer::new();
        run(&mut n, talk(-34.0, -70.0, 10.0));
        assert!(
            (9.0..10.2).contains(&n.gain_db()),
            "12 dB off → 10 dB, got {}",
            n.gain_db()
        );
        let mut n = Normalizer::new();
        run(&mut n, talk(-10.0, -70.0, 10.0));
        assert!(
            (-10.2..-9.0).contains(&n.gain_db()),
            "12 dB over → −10 dB, got {}",
            n.gain_db()
        );
        // Just beyond the edge, the gain is just above zero.
        let mut n = Normalizer::new();
        run(&mut n, talk(-25.0, -70.0, 10.0));
        assert!(
            (0.3..1.3).contains(&n.gain_db()),
            "3 dB off → ~1 dB, got {}",
            n.gain_db()
        );
    }

    /// The receive profile attacks slower than the sender's leveler, so a
    /// jump the sender is already correcting does not get cut twice.
    #[test]
    fn receive_attacks_slower_than_the_mic_leveler() {
        // 200 ms of room noise first, so the floor is known.
        let mut rx = Normalizer::new();
        run(&mut rx, talk(-100.0, -70.0, 0.2));
        run(&mut rx, talk(-8.0, -70.0, 0.15));
        let mut mic = Normalizer::with(Profile::MIC_LEVEL);
        run(&mut mic, talk(-100.0, -70.0, 0.2));
        run(&mut mic, talk(-8.0, -70.0, 0.15));
        assert!(
            mic.gain_db() < rx.gain_db() - 3.0,
            "after 150 ms: mic {} dB, receive {} dB",
            mic.gain_db(),
            rx.gain_db()
        );
    }

    #[test]
    fn gain_is_bounded() {
        let mut n = Normalizer::new();
        run(&mut n, talk(-48.0, -90.0, 20.0));
        assert!(n.gain_db() <= Profile::RECEIVE.max_boost_db + 0.01);
        let mut n = Normalizer::new();
        run(&mut n, talk(0.0, -90.0, 5.0));
        assert!(n.gain_db() >= Profile::RECEIVE.max_cut_db - 0.01);
    }

    /// After a pause of digital silence the floor is still known, so the
    /// next spurt moves the estimate from its first blocks on. With the
    /// floor forgotten, a spurt that starts loud and steady would not count
    /// as speech until it had a quiet moment of its own.
    #[test]
    fn floor_survives_digital_silence() {
        let mut n = Normalizer::with(Profile::OUTBOUND);
        run(&mut n, talk(-34.0, -70.0, 5.0));
        let held = n.gain_db();
        run(&mut n, vec![vec![0.0; BLOCK]; 500]);
        assert_eq!(n.gain_db(), held);
        // 300 ms of a louder voice, no quiet block in it.
        let loud: Vec<Vec<f32>> = talk(-14.0, -70.0, 0.3).into_iter().take(30).collect();
        run(&mut n, loud);
        assert!(
            n.gain_db() < held - 6.0,
            "the louder spurt was not caught: {} → {} dB",
            held,
            n.gain_db()
        );
    }

    #[test]
    fn silence_holds_gain() {
        let mut n = Normalizer::new();
        run(&mut n, talk(-34.0, -70.0, 10.0));
        let before = n.gain_db();
        run(&mut n, vec![vec![0.0; BLOCK]; 500]);
        assert!((n.gain_db() - before).abs() < 0.01);
    }

    #[test]
    fn steady_hiss_is_not_boosted() {
        // Hiss above the absolute gate, no speech at all.
        let mut n = Normalizer::new();
        run(&mut n, talk(-100.0, -40.0, 10.0));
        assert!(n.gain_db().abs() < 1.0, "gain {}", n.gain_db());
    }

    #[test]
    fn speech_over_hiss_is_normalized() {
        let mut n = Normalizer::with(Profile::OUTBOUND);
        run(&mut n, talk(-30.0, -45.0, 10.0));
        assert!(n.gain_db() > 5.0, "gain {}", n.gain_db());
    }

    #[test]
    fn output_is_finite() {
        let mut n = Normalizer::new();
        let mut b = vec![f32::NAN, f32::INFINITY, 0.5, -0.5];
        n.process(&mut b);
        assert!(b.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn reset_restores_unity() {
        let mut n = Normalizer::new();
        run(&mut n, talk(-34.0, -70.0, 10.0));
        n.reset();
        assert_eq!(n.gain_db(), 0.0);
    }

    #[test]
    fn mic_level_cuts_fast_and_recovers_slowly() {
        let mut n = Normalizer::with(Profile::MIC_LEVEL);
        run(&mut n, talk(-100.0, -70.0, 0.2));
        run(&mut n, talk(-8.0, -70.0, 0.3));
        assert!(n.gain_db() < -8.0, "300 ms in: gain {}", n.gain_db());
        run(&mut n, talk(-8.0, -70.0, 2.0));
        assert!(n.gain_db() < -9.9, "gain {}", n.gain_db());
        // Quiet again: the boost comes back over seconds, not syllables.
        run(&mut n, talk(-34.0, -70.0, 1.0));
        assert!(n.gain_db() < -4.0, "1 s later: gain {}", n.gain_db());
        run(&mut n, talk(-34.0, -70.0, 19.0));
        assert!(n.gain_db() > 11.0, "20 s later: gain {}", n.gain_db());
    }

    #[test]
    fn mic_level_boost_is_bounded() {
        let mut n = Normalizer::with(Profile::MIC_LEVEL);
        run(&mut n, talk(-48.0, -90.0, 30.0));
        assert!(
            n.gain_db() > 12.0 && n.gain_db() <= 15.01,
            "gain {}",
            n.gain_db()
        );
    }

    #[test]
    fn no_voice_hint_holds_gain() {
        let mut n = Normalizer::with(Profile::OUTBOUND);
        for mut b in talk(-34.0, -70.0, 10.0) {
            n.process_hinted(&mut b, Some(false));
        }
        assert_eq!(n.gain_db(), 0.0);
        for mut b in talk(-34.0, -70.0, 10.0) {
            n.process_hinted(&mut b, Some(true));
        }
        assert!(n.gain_db() > 9.0, "gain {}", n.gain_db());
    }

    #[test]
    fn reset_keeps_profile() {
        let mut n = Normalizer::with(Profile::MIC_LEVEL);
        n.reset();
        assert_eq!(n.profile, Profile::MIC_LEVEL);
    }
}
