//! PTT/VAD gate state machine (`docs/reference/client.md` §6).
//!
//! Evaluated once per Opus frame. The gate sits behind AEC and denoise; both
//! keep running while the gate is closed.
//!
//! In VAD mode a closed gate opens only after voice has lasted the *attack*
//! time: a key click is one or two blocks of broadband noise the voice
//! detector may well call voice, and one such block used to hold the gate
//! open for the whole hangover.

use std::time::{Duration, Instant};

/// Transmission mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Push-to-talk.
    Ptt,
    /// Voice activation.
    Vad,
}

/// Per frame decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Encode and send.
    Send,
    /// Encode, send with the `END` flag, then stop.
    SendEnd,
    /// Do not send.
    Silent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Closed,
    /// Voice since `since`, `frames` voiced frames held back so far.
    Arming {
        since: Instant,
        frames: u32,
        audio: Duration,
    },
    Open,
    Releasing(Instant),
}

/// The gate.
#[derive(Debug)]
pub struct Gate {
    mode: Mode,
    /// PTT release delay.
    release: Duration,
    /// VAD hangover.
    hangover: Duration,
    /// How long voice must last before a closed gate opens (VAD only).
    attack: Duration,
    /// Voiced frames held back before the last opening.
    withheld: u32,
    state: State,
    ptt_down: bool,
    muted: bool,
}

impl Gate {
    /// New gate in `mode` with PTT release delay and VAD hangover.
    pub fn new(mode: Mode, release: Duration, hangover: Duration) -> Gate {
        Gate {
            mode,
            release,
            hangover,
            attack: Duration::ZERO,
            withheld: 0,
            state: State::Closed,
            ptt_down: false,
            muted: false,
        }
    }

    /// Current mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Switches the mode. An open gate closes at the next frame with `END`.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Changes the release delay.
    pub fn set_release(&mut self, release: Duration) {
        self.release = release;
    }

    /// Changes the VAD hangover.
    pub fn set_hangover(&mut self, hangover: Duration) {
        self.hangover = hangover;
    }

    /// Changes the VAD attack: voice must last this long before a closed
    /// gate opens. Zero opens on the first voiced frame.
    pub fn set_attack(&mut self, attack: Duration) {
        self.attack = attack;
    }

    /// How many voiced frames the gate held back before it last opened, so
    /// the caller can send them as pre-roll. Zero when it opened at once.
    pub fn withheld(&self) -> u32 {
        self.withheld
    }

    /// Reports the PTT key state.
    pub fn set_ptt(&mut self, down: bool) {
        self.ptt_down = down;
    }

    /// Mute closes the gate regardless of mode.
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
    }

    /// Whether mute (or no connection) holds the gate shut: closed, it then
    /// says nothing about whether the user is talking.
    pub fn is_muted(&self) -> bool {
        self.muted
    }

    /// Whether the gate currently lets audio through. Not while it is still
    /// arming: the first voiced frames are held back.
    pub fn is_open(&self) -> bool {
        matches!(self.state, State::Open | State::Releasing(_))
    }

    /// Forget a discontinued capture timeline, keeping the user's controls.
    pub fn reset(&mut self) {
        self.state = State::Closed;
        self.withheld = 0;
    }

    /// One frame: `voice` is the VAD verdict for this frame.
    pub fn update(&mut self, now: Instant, voice: bool) -> Decision {
        self.update_audio(now, voice, Duration::ZERO)
    }

    /// Audio may arrive in large hardware batches. Count its duration as
    /// well as elapsed wall time, so the attack cannot discard the beginning
    /// of a word merely because its frames were processed in one burst.
    pub fn update_audio(&mut self, now: Instant, voice: bool, frame: Duration) -> Decision {
        let want_open = !self.muted
            && match self.mode {
                Mode::Ptt => self.ptt_down,
                Mode::Vad => voice,
            };
        let hold = match self.mode {
            Mode::Ptt => self.release,
            Mode::Vad => self.hangover,
        };
        let attack = match self.mode {
            Mode::Ptt => Duration::ZERO,
            Mode::Vad => self.attack,
        };
        match (self.state, want_open) {
            (State::Closed, true) if !attack.is_zero() => {
                self.state = State::Arming {
                    since: now,
                    frames: 1,
                    audio: Duration::ZERO,
                };
                Decision::Silent
            }
            (
                State::Arming {
                    since,
                    frames,
                    audio,
                },
                true,
            ) if now.saturating_duration_since(since).max(audio + frame) < attack => {
                self.state = State::Arming {
                    since,
                    frames: frames + 1,
                    audio: audio + frame,
                };
                Decision::Silent
            }
            (State::Arming { frames, .. }, true) => {
                self.withheld = frames;
                self.state = State::Open;
                Decision::Send
            }
            (State::Arming { .. }, false) => {
                self.state = State::Closed;
                Decision::Silent
            }
            (State::Closed, true) => {
                self.withheld = 0;
                self.state = State::Open;
                Decision::Send
            }
            (_, true) => {
                self.state = State::Open;
                Decision::Send
            }
            (State::Closed, false) => Decision::Silent,
            (State::Open, false) => {
                if hold.is_zero() || self.muted {
                    self.state = State::Closed;
                    Decision::SendEnd
                } else {
                    self.state = State::Releasing(now + hold);
                    Decision::Send
                }
            }
            (State::Releasing(until), false) => {
                if now >= until || self.muted {
                    self.state = State::Closed;
                    Decision::SendEnd
                } else {
                    Decision::Send
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn batched_audio_opens_after_the_configured_audio_attack() {
        let now = Instant::now();
        for frame_ms in [10, 20] {
            for attack_ms in [40, 80] {
                let mut gate = Gate::new(Mode::Vad, ms(150), ms(200));
                gate.set_attack(ms(attack_ms));
                let held = attack_ms / frame_ms;
                for _ in 0..held {
                    assert_eq!(gate.update_audio(now, true, ms(frame_ms)), Decision::Silent);
                }
                assert_eq!(gate.update_audio(now, true, ms(frame_ms)), Decision::Send);
                assert_eq!(gate.withheld(), held as u32);
                gate.reset();
                assert_eq!(gate.update_audio(now, true, ms(frame_ms)), Decision::Silent);
            }
        }
    }

    #[test]
    fn ptt_press_sends_immediately_and_release_delay_ends_with_end_flag() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Ptt, ms(150), ms(200));
        assert_eq!(
            g.update(t0, true),
            Decision::Silent,
            "voice alone is not enough in PTT"
        );
        g.set_ptt(true);
        assert_eq!(g.update(t0, false), Decision::Send);
        assert!(g.is_open());
        g.set_ptt(false);
        // Release delay: keep sending for 150 ms.
        assert_eq!(g.update(t0 + ms(20), false), Decision::Send);
        assert_eq!(g.update(t0 + ms(100), false), Decision::Send);
        assert_eq!(g.update(t0 + ms(180), false), Decision::SendEnd);
        assert!(!g.is_open());
        assert_eq!(g.update(t0 + ms(200), false), Decision::Silent);
    }

    #[test]
    fn re_press_during_release_keeps_the_spurt() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Ptt, ms(150), ms(200));
        g.set_ptt(true);
        g.update(t0, false);
        g.set_ptt(false);
        assert_eq!(g.update(t0 + ms(20), false), Decision::Send);
        g.set_ptt(true);
        assert_eq!(g.update(t0 + ms(40), false), Decision::Send);
        g.set_ptt(false);
        assert_eq!(g.update(t0 + ms(60), false), Decision::Send);
        assert_eq!(g.update(t0 + ms(230), false), Decision::SendEnd);
    }

    #[test]
    fn vad_hangover() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Vad, ms(150), ms(200));
        assert_eq!(g.update(t0, true), Decision::Send);
        assert_eq!(g.update(t0 + ms(20), false), Decision::Send);
        assert_eq!(g.update(t0 + ms(100), false), Decision::Send);
        assert_eq!(g.update(t0 + ms(240), false), Decision::SendEnd);
        assert_eq!(g.update(t0 + ms(260), false), Decision::Silent);
    }

    #[test]
    fn mute_closes_immediately_with_end() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Ptt, ms(150), ms(200));
        g.set_ptt(true);
        assert_eq!(g.update(t0, false), Decision::Send);
        g.set_muted(true);
        assert_eq!(g.update(t0 + ms(20), false), Decision::SendEnd);
        assert_eq!(g.update(t0 + ms(40), false), Decision::Silent);
        g.set_muted(false);
        assert_eq!(g.update(t0 + ms(60), false), Decision::Send, "still held");
    }

    #[test]
    fn zero_release_ends_at_once() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Ptt, ms(0), ms(0));
        g.set_ptt(true);
        g.update(t0, false);
        g.set_ptt(false);
        assert_eq!(g.update(t0 + ms(20), false), Decision::SendEnd);
    }

    #[test]
    fn a_click_does_not_open_the_gate() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Vad, ms(150), ms(200));
        g.set_attack(ms(40));
        assert_eq!(g.update(t0, true), Decision::Silent, "arming");
        assert!(!g.is_open());
        assert_eq!(g.update(t0 + ms(20), false), Decision::Silent);
        assert_eq!(g.update(t0 + ms(40), false), Decision::Silent);
        assert!(!g.is_open(), "one voiced frame never opens it");
    }

    #[test]
    fn voice_lasting_the_attack_opens_and_reports_the_held_back_frames() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Vad, ms(150), ms(200));
        g.set_attack(ms(40));
        assert_eq!(g.update(t0, true), Decision::Silent);
        assert_eq!(g.update(t0 + ms(20), true), Decision::Silent);
        assert_eq!(g.update(t0 + ms(40), true), Decision::Send);
        assert!(g.is_open());
        assert_eq!(g.withheld(), 2);
    }

    #[test]
    fn a_gap_restarts_the_attack() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Vad, ms(150), ms(200));
        g.set_attack(ms(40));
        g.update(t0, true);
        g.update(t0 + ms(20), true);
        assert_eq!(g.update(t0 + ms(40), false), Decision::Silent);
        assert_eq!(g.update(t0 + ms(60), true), Decision::Silent, "starts over");
        assert_eq!(g.update(t0 + ms(80), true), Decision::Silent);
        assert_eq!(g.update(t0 + ms(100), true), Decision::Send);
    }

    #[test]
    fn voice_in_the_hangover_resumes_without_an_attack() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Vad, ms(150), ms(200));
        g.set_attack(ms(40));
        g.update(t0, true);
        g.update(t0 + ms(40), true);
        assert_eq!(g.update(t0 + ms(60), false), Decision::Send);
        assert_eq!(g.update(t0 + ms(80), true), Decision::Send, "no re-arming");
    }

    #[test]
    fn zero_attack_opens_on_the_first_voiced_frame() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Vad, ms(150), ms(200));
        g.set_attack(ms(0));
        assert_eq!(g.update(t0, true), Decision::Send);
        assert_eq!(g.withheld(), 0);
    }

    #[test]
    fn the_attack_does_not_delay_push_to_talk() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Ptt, ms(150), ms(200));
        g.set_attack(ms(40));
        g.set_ptt(true);
        assert_eq!(g.update(t0, false), Decision::Send);
    }

    #[test]
    fn mute_ends_arming() {
        let t0 = Instant::now();
        let mut g = Gate::new(Mode::Vad, ms(150), ms(200));
        g.set_attack(ms(40));
        g.update(t0, true);
        g.set_muted(true);
        assert_eq!(g.update(t0 + ms(20), true), Decision::Silent);
        g.set_muted(false);
        assert_eq!(g.update(t0 + ms(40), true), Decision::Silent, "starts over");
    }
}
