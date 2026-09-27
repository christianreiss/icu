//! Portal-only bindings imported from another desktop, read by Raw Input.
//! A saved physical key remains authoritative when both forms are present.

use std::collections::HashSet;

use super::keys;
use crate::config::KeyBinding;

#[derive(Debug)]
pub(super) struct Chord {
    key: u16,
    modifiers: Vec<[u16; 2]>,
}

impl Chord {
    pub fn parse(binding: &KeyBinding) -> Option<Self> {
        if !binding.key.trim().is_empty() {
            return keys::parse_key(&binding.key).map(|key| Self {
                key,
                modifiers: vec![],
            });
        }
        let mut parts: Vec<_> = binding
            .trigger
            .as_deref()?
            .split('+')
            .map(str::trim)
            .collect();
        let name = parts.pop()?.to_ascii_uppercase();
        let name = match name.as_str() {
            "RETURN" => "ENTER",
            "ESCAPE" => "ESC",
            "PAGE_UP" => "PAGEUP",
            "PAGE_DOWN" => "PAGEDOWN",
            other => other,
        };
        let key = keys::parse_key(&format!("KEY_{name}"))?;
        let modifiers = parts
            .into_iter()
            .map(|m| match m.to_ascii_uppercase().as_str() {
                "CTRL" | "CONTROL" => Some([29, 97]),
                "SHIFT" => Some([42, 54]),
                "ALT" => Some([56, 100]),
                "LOGO" | "SUPER" | "META" => Some([125, 126]),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self { key, modifiers })
    }

    pub fn held(&self, held: &HashSet<u16>) -> bool {
        held.contains(&self.key)
            && self
                .modifiers
                .iter()
                .all(|pair| pair.iter().any(|k| held.contains(k)))
    }

    /// Whether the key is a mouse button: only then does Raw Input need
    /// the mice.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn uses_mouse(&self) -> bool {
        is_mouse_button(self.key)
    }
}

/// The modifiers a trigger names, in the order a recorded one lists them,
/// with their left and right keys.
const MODIFIERS: [(&str, [u16; 2]); 4] = [
    ("CTRL", [29, 97]),
    ("ALT", [56, 100]),
    ("SHIFT", [42, 54]),
    ("LOGO", [125, 126]),
];

fn is_modifier(code: u16) -> bool {
    MODIFIERS.iter().any(|(_, keys)| keys.contains(&code))
}

fn is_mouse_button(code: u16) -> bool {
    (keys::BTN_LEFT..=keys::BTN_EXTRA).contains(&code)
}

/// „Aufnehmen“ with Raw Input: follows key transitions until a binding is
/// complete. The first key that is not a modifier ends it — as a trigger
/// (`CTRL+SHIFT+F12`, what [`Chord::parse`] reads) when modifiers are held,
/// else by itself. A modifier pressed and released without another key is
/// bound by itself (Right Ctrl as push-to-talk). Left and right click
/// operate the window the recording runs in and do not count; a mouse
/// button or a key without a trigger name is bound by itself.
#[derive(Debug, Default)]
pub(super) struct Recorder {
    /// Modifiers pressed since the recording started, still held.
    held: Vec<u16>,
}

impl Recorder {
    /// Takes one transition; the binding (a key name such as `KEY_F12` or
    /// `BTN_SIDE`, or a trigger such as `CTRL+F12`) once it is complete.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn feed(&mut self, code: u16, pressed: bool) -> Option<String> {
        if is_modifier(code) {
            if pressed {
                if !self.held.contains(&code) {
                    self.held.push(code);
                }
                return None;
            }
            // Released: bound alone, unless it was down before recording.
            let was_held = self.held.contains(&code);
            self.held.retain(|k| *k != code);
            return was_held.then(|| keys::key_name(code));
        }
        if !pressed || code == keys::BTN_LEFT || code == keys::BTN_RIGHT {
            return None;
        }
        let key = keys::key_name(code);
        let trigger = self.trigger(&key);
        Some(trigger.unwrap_or(key))
    }

    /// `key` with the modifiers held, when there are any and a trigger can
    /// name it.
    fn trigger(&self, key: &str) -> Option<String> {
        let name = key.strip_prefix("KEY_")?;
        let mut parts: Vec<&str> = MODIFIERS
            .iter()
            .filter(|(_, keys)| keys.iter().any(|k| self.held.contains(k)))
            .map(|(name, _)| *name)
            .collect();
        if parts.is_empty() {
            return None;
        }
        parts.push(name);
        let trigger = parts.join("+");
        Chord::parse(&KeyBinding {
            key: String::new(),
            trigger: Some(trigger.clone()),
        })
        .map(|_| trigger)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_chord_requires_modifiers_and_releases_with_either_key() {
        let chord = Chord::parse(&KeyBinding {
            key: String::new(),
            trigger: Some("CTRL+SHIFT+F12".into()),
        })
        .unwrap();
        let mut held = HashSet::from([88]);
        assert!(!chord.held(&held));
        held.extend([97, 42]);
        assert!(chord.held(&held));
        held.remove(&97);
        assert!(!chord.held(&held));
        held.insert(29);
        assert!(chord.held(&held));
        held.remove(&88);
        assert!(!chord.held(&held));
    }

    fn record(transitions: &[(u16, bool)]) -> Option<String> {
        let mut recorder = Recorder::default();
        transitions
            .iter()
            .find_map(|(code, pressed)| recorder.feed(*code, *pressed))
    }

    /// Held modifiers and the first other key make a combination, which
    /// reads back as the same chord; a key alone stays a key.
    #[test]
    fn recording_takes_combinations() {
        let ctrl_shift_f12 = record(&[(97, true), (42, true), (88, true)]).unwrap();
        assert_eq!(ctrl_shift_f12, "CTRL+SHIFT+F12");
        let chord = Chord::parse(&KeyBinding {
            key: String::new(),
            trigger: Some(ctrl_shift_f12),
        })
        .unwrap();
        assert!(chord.held(&HashSet::from([29, 54, 88])));
        assert_eq!(
            record(&[(56, true), (125, true), (30, true)]).unwrap(),
            "ALT+LOGO+A"
        );
        assert_eq!(record(&[(88, true)]).unwrap(), "KEY_F12");
        // A modifier let go before the key does not count.
        assert_eq!(
            record(&[(29, true), (42, true), (42, false)]).unwrap(),
            "KEY_LEFTSHIFT"
        );
        let mut recorder = Recorder::default();
        assert_eq!(recorder.feed(29, true), None);
        assert_eq!(recorder.feed(88, false), None, "a release is no key");
        assert_eq!(recorder.feed(88, true).as_deref(), Some("CTRL+F12"));
    }

    /// A lone modifier, pressed and released, is bound by itself; one that
    /// was down before recording started is not taken on its release.
    #[test]
    fn recording_takes_a_lone_modifier_on_release() {
        assert_eq!(record(&[(97, true), (97, false)]).unwrap(), "KEY_RIGHTCTRL");
        assert_eq!(record(&[(97, false)]), None);
        assert_eq!(record(&[(97, false), (88, true)]).unwrap(), "KEY_F12");
    }

    /// Clicks in the window do not count; other mouse buttons are bound by
    /// themselves (a trigger has no name for them), as are keys without a
    /// trigger name.
    #[test]
    fn recording_takes_mouse_buttons_alone() {
        assert_eq!(
            record(&[(keys::BTN_LEFT, true), (keys::BTN_RIGHT, true)]),
            None
        );
        assert_eq!(
            record(&[(29, true), (keys::BTN_SIDE, true)]).unwrap(),
            "BTN_SIDE"
        );
        assert_eq!(record(&[(29, true), (700, true)]).unwrap(), "code:700");
    }

    /// Only a binding on a mouse button needs the mice (Raw Input registers
    /// them only then).
    #[test]
    fn only_mouse_bindings_use_the_mouse() {
        let chord = |key: &str, trigger: Option<&str>| {
            Chord::parse(&KeyBinding {
                key: key.into(),
                trigger: trigger.map(Into::into),
            })
            .unwrap()
        };
        assert!(chord("BTN_SIDE", None).uses_mouse());
        assert!(chord("BTN_MIDDLE", None).uses_mouse());
        assert!(!chord("KEY_RIGHTCTRL", None).uses_mouse());
        assert!(!chord("", Some("CTRL+F12")).uses_mouse());
    }

    #[test]
    fn physical_binding_wins_and_unknown_chords_do_not_become_bare_keys() {
        let mut binding = KeyBinding {
            key: "BTN_SIDE".into(),
            trigger: Some("CTRL+F12".into()),
        };
        assert!(Chord::parse(&binding)
            .unwrap()
            .held(&HashSet::from([keys::BTN_SIDE])));
        binding.key.clear();
        binding.trigger = Some("UNKNOWN+F12".into());
        assert!(Chord::parse(&binding).is_none());
        binding.trigger = Some("CTRL+".into());
        assert!(Chord::parse(&binding).is_none());
    }
}
