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
