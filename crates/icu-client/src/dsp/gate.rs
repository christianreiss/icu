//! PTT/VAD gate state machine (`Client/IMPLEMENTATION.md` §6).
//!
//! Evaluated once per Opus frame. The gate sits behind AEC and denoise; both
//! keep running while the gate is closed.

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

    /// Whether the gate currently lets audio through.
    pub fn is_open(&self) -> bool {
        !matches!(self.state, State::Closed)
    }

    /// One frame: `voice` is the VAD verdict for this frame.
    pub fn update(&mut self, now: Instant, voice: bool) -> Decision {
        let want_open = !self.muted
            && match self.mode {
                Mode::Ptt => self.ptt_down,
                Mode::Vad => voice,
            };
        let hold = match self.mode {
            Mode::Ptt => self.release,
            Mode::Vad => self.hangover,
        };
        match (self.state, want_open) {
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
}
