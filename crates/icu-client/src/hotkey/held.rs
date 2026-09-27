//! The keys the Raw Input reader holds down (Windows), per device, and the
//! rules for letting go of them without a release. Windows does not deliver
//! every key-up: a window of a higher integrity level in the foreground
//! (UIPI), the secure desktop (UAC, Win+L) and a device that drops out while
//! a key is down all swallow it, and push-to-talk would stay open. The
//! reader fails closed: on each of these it lets go itself
//! (`super::rawinput`). Kept free of Win32 so it is tested on every host.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// A Raw Input device (`RAWINPUTHEADER::hDevice`); 0 for input that has
/// none (injected).
pub(super) type Device = isize;

/// Longest a key counts as held without a sign of life: the last resort
/// for a release lost some other way (a keyboard hook that eats it, a
/// notification Windows did not send). Auto-repeat counts as a sign of
/// life, so a key held for real and still repeating is never cut; a mouse
/// button or a key whose repeat another key took over ends after this long
/// and has to be pressed again.
pub(super) const MAX_HOLD: Duration = Duration::from_secs(5 * 60);

/// Keys down, by device: the same key on two keyboards is one key to the
/// listeners, down while either holds it.
#[derive(Debug, Default)]
pub(super) struct HeldKeys {
    /// When each key last went down (auto-repeat included).
    keys: HashMap<(Device, u16), Instant>,
}

impl HeldKeys {
    /// `code` went down on `device`. True when no device held it before —
    /// a press to pass on; false for auto-repeat and for a key another
    /// device already holds.
    pub fn press(&mut self, device: Device, code: u16, now: Instant) -> bool {
        let new = !self.is_down(code);
        self.keys.insert((device, code), now);
        new
    }

    /// `code` went up on `device`. True when no device holds it any more —
    /// a release to pass on; false for a key this device did not hold.
    pub fn release(&mut self, device: Device, code: u16) -> bool {
        self.keys.remove(&(device, code)).is_some() && !self.is_down(code)
    }

    /// `device` is gone: its keys are up. Returns those no other device
    /// holds.
    pub fn remove_device(&mut self, device: Device) -> Vec<u16> {
        self.drop_where(|(d, _), _| *d == device)
    }

    /// Lets go of every key; returns them.
    pub fn release_all(&mut self) -> Vec<u16> {
        self.drop_where(|_, _| true)
    }

    /// Lets go of every key down for longer than `max` without auto-repeat;
    /// returns those no device holds any more.
    pub fn expire(&mut self, now: Instant, max: Duration) -> Vec<u16> {
        self.drop_where(|_, since| now.saturating_duration_since(*since) > max)
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    fn is_down(&self, code: u16) -> bool {
        self.keys.keys().any(|&(_, c)| c == code)
    }

    /// Removes the keys `gone` picks; returns their codes that no device
    /// holds afterwards, each once.
    fn drop_where(&mut self, gone: impl Fn(&(Device, u16), &Instant) -> bool) -> Vec<u16> {
        let mut codes = Vec::new();
        self.keys.retain(|key, since| {
            let drop = gone(key, since);
            if drop {
                codes.push(key.1);
            }
            !drop
        });
        codes.sort_unstable();
        codes.dedup();
        codes.retain(|&code| !self.is_down(code));
        codes
    }
}

/// Whether a window of integrity level `theirs` in the foreground hides
/// input from a process of level `ours` (UIPI: key-ups included).
/// Levels are the mandatory-label RIDs (`SECURITY_MANDATORY_*_RID`); a
/// level Windows would not tell counts as higher — an elevated process
/// may refuse to be queried at all.
pub(super) fn foreground_hides_input(ours: Option<u32>, theirs: Option<u32>) -> bool {
    match (ours, theirs) {
        (Some(ours), Some(theirs)) => theirs > ours,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYBOARD: Device = 0x1001;
    const OTHER: Device = 0x2002;
    const F12: u16 = 88;
    const SIDE: u16 = 0x113;

    #[test]
    fn auto_repeat_is_not_a_new_press() {
        let t = Instant::now();
        let mut held = HeldKeys::default();
        assert!(held.press(KEYBOARD, F12, t));
        assert!(!held.press(KEYBOARD, F12, t));
        assert!(held.release(KEYBOARD, F12));
        assert!(!held.release(KEYBOARD, F12), "released twice");
        assert!(held.press(KEYBOARD, F12, t), "pressed again");
    }

    #[test]
    fn a_key_held_on_two_devices_is_up_when_both_let_go() {
        let t = Instant::now();
        let mut held = HeldKeys::default();
        assert!(held.press(KEYBOARD, F12, t));
        assert!(!held.press(OTHER, F12, t));
        assert!(!held.release(KEYBOARD, F12));
        assert!(held.release(OTHER, F12));
        assert!(held.is_empty());
    }

    #[test]
    fn a_removed_device_lets_go_of_its_own_keys_only() {
        let t = Instant::now();
        let mut held = HeldKeys::default();
        held.press(KEYBOARD, F12, t);
        held.press(OTHER, SIDE, t);
        held.press(OTHER, F12, t);
        // F12 is still down on the keyboard, the side button was only the
        // mouse's.
        assert_eq!(held.remove_device(OTHER), vec![SIDE]);
        assert_eq!(held.remove_device(OTHER), Vec::<u16>::new());
        assert!(!held.press(KEYBOARD, F12, t), "F12 was let go");
        assert!(
            held.press(OTHER, SIDE, t),
            "next press was taken for a repeat"
        );
    }

    #[test]
    fn release_all_names_every_key_once_and_starts_over() {
        let t = Instant::now();
        let mut held = HeldKeys::default();
        held.press(KEYBOARD, F12, t);
        held.press(OTHER, F12, t);
        held.press(OTHER, SIDE, t);
        assert_eq!(held.release_all(), vec![F12, SIDE]);
        assert!(held.is_empty());
        // The release that went astray arrives late: nothing to pass on.
        assert!(!held.release(KEYBOARD, F12));
        assert!(
            held.press(KEYBOARD, F12, t),
            "next press was taken for a repeat"
        );
    }

    #[test]
    fn a_key_without_sign_of_life_expires_after_the_limit() {
        let t = Instant::now();
        let mut held = HeldKeys::default();
        held.press(OTHER, SIDE, t);
        held.press(KEYBOARD, F12, t);
        // F12 still repeats; the mouse button cannot.
        held.press(KEYBOARD, F12, t + MAX_HOLD);
        assert_eq!(held.expire(t + MAX_HOLD, MAX_HOLD), Vec::<u16>::new());
        let later = t + MAX_HOLD + Duration::from_secs(1);
        assert_eq!(held.expire(later, MAX_HOLD), vec![SIDE]);
        assert!(!held.press(KEYBOARD, F12, later), "F12 was let go");
        assert!(held.press(OTHER, SIDE, later));
    }

    #[test]
    fn an_expired_device_key_stays_down_while_another_device_holds_it() {
        let t = Instant::now();
        let mut held = HeldKeys::default();
        held.press(KEYBOARD, F12, t);
        let later = t + MAX_HOLD + Duration::from_secs(1);
        held.press(OTHER, F12, later);
        assert_eq!(held.expire(later, MAX_HOLD), Vec::<u16>::new());
        assert!(held.release(OTHER, F12));
    }

    #[test]
    fn only_a_higher_or_unknown_foreground_hides_input() {
        const LOW: u32 = 0x1000;
        const MEDIUM: u32 = 0x2000;
        const HIGH: u32 = 0x3000;
        const SYSTEM: u32 = 0x4000;
        assert!(foreground_hides_input(Some(MEDIUM), Some(HIGH)));
        assert!(foreground_hides_input(Some(MEDIUM), Some(SYSTEM)));
        assert!(foreground_hides_input(Some(HIGH), Some(SYSTEM)));
        assert!(!foreground_hides_input(Some(MEDIUM), Some(MEDIUM)));
        assert!(!foreground_hides_input(Some(MEDIUM), Some(LOW)));
        assert!(!foreground_hides_input(Some(HIGH), Some(MEDIUM)));
        // An elevated process refused the query; our own level unknown.
        assert!(foreground_hides_input(Some(MEDIUM), None));
        assert!(foreground_hides_input(None, Some(LOW)));
    }
}
