//! When a key was last pressed on any keyboard — a timestamp and nothing
//! else, never which key. The voice gate leans on it: a click of the
//! keyboard is the commonest false voice, so for a moment after a key press
//! voice has to last longer to open the gate (`Gate::set_attack`).
//!
//! Only the backends that see every key feed it (evdev, Raw Input); with the
//! XDG portal it stays silent and the gate's plain attack time does the job.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
    time::{Duration, Instant},
};

/// How long after a key press voice counts as "while typing".
pub const WINDOW: Duration = Duration::from_millis(150);

/// The time of the last key press.
#[derive(Debug)]
pub struct KeyActivity {
    /// Milliseconds after `origin` of the last press, plus one; 0 = never.
    last: AtomicU64,
    origin: Instant,
}

impl KeyActivity {
    /// Nothing pressed yet; times count from `origin`.
    pub fn new(origin: Instant) -> KeyActivity {
        KeyActivity {
            last: AtomicU64::new(0),
            origin,
        }
    }

    /// A key went down at `now`.
    pub fn note_at(&self, now: Instant) {
        let ms = now.saturating_duration_since(self.origin).as_millis() as u64;
        self.last.store(ms + 1, Ordering::Relaxed);
    }

    /// Whether a key went down at most `within` before `now`.
    pub fn recent_at(&self, now: Instant, within: Duration) -> bool {
        match self.last.load(Ordering::Relaxed) {
            0 => false,
            v => {
                let at = self.origin + Duration::from_millis(v - 1);
                now.saturating_duration_since(at) <= within
            }
        }
    }
}

fn global() -> &'static KeyActivity {
    static G: OnceLock<KeyActivity> = OnceLock::new();
    G.get_or_init(|| KeyActivity::new(Instant::now()))
}

/// A key went down.
pub fn note() {
    global().note_at(Instant::now());
}

/// Whether a key went down within [`WINDOW`].
pub fn recent() -> bool {
    global().recent_at(Instant::now(), WINDOW)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_until_the_window_has_passed() {
        let t0 = Instant::now();
        let k = KeyActivity::new(t0);
        assert!(!k.recent_at(t0, WINDOW), "never pressed");
        k.note_at(t0 + Duration::from_millis(100));
        assert!(k.recent_at(t0 + Duration::from_millis(100), WINDOW));
        assert!(k.recent_at(t0 + Duration::from_millis(250), WINDOW));
        assert!(!k.recent_at(t0 + Duration::from_millis(251), WINDOW));
        k.note_at(t0 + Duration::from_millis(400));
        assert!(
            k.recent_at(t0 + Duration::from_millis(500), WINDOW),
            "again"
        );
    }

    #[test]
    fn a_press_at_the_origin_counts() {
        let t0 = Instant::now();
        let k = KeyActivity::new(t0);
        k.note_at(t0);
        assert!(k.recent_at(t0, WINDOW));
    }
}
