//! Key names without evdev: the bindings in the settings are evdev names
//! (`KEY_F12`, `BTN_SIDE`, `code:<n>`) on every platform, so a settings
//! file and a passport mean the same keys on Linux and Windows. This table
//! covers the keys a push-to-talk or toggle is put on; any other code
//! reads and writes as `code:<n>`, like the evdev backend does.

/// evdev codes (`linux/input-event-codes.h`) and their names.
const NAMES: &[(u16, &str)] = &[
    (1, "KEY_ESC"),
    (2, "KEY_1"),
    (3, "KEY_2"),
    (4, "KEY_3"),
    (5, "KEY_4"),
    (6, "KEY_5"),
    (7, "KEY_6"),
    (8, "KEY_7"),
    (9, "KEY_8"),
    (10, "KEY_9"),
    (11, "KEY_0"),
    (12, "KEY_MINUS"),
    (13, "KEY_EQUAL"),
    (14, "KEY_BACKSPACE"),
    (15, "KEY_TAB"),
    (16, "KEY_Q"),
    (17, "KEY_W"),
    (18, "KEY_E"),
    (19, "KEY_R"),
    (20, "KEY_T"),
    (21, "KEY_Y"),
    (22, "KEY_U"),
    (23, "KEY_I"),
    (24, "KEY_O"),
    (25, "KEY_P"),
    (26, "KEY_LEFTBRACE"),
    (27, "KEY_RIGHTBRACE"),
    (28, "KEY_ENTER"),
    (29, "KEY_LEFTCTRL"),
    (30, "KEY_A"),
    (31, "KEY_S"),
    (32, "KEY_D"),
    (33, "KEY_F"),
    (34, "KEY_G"),
    (35, "KEY_H"),
    (36, "KEY_J"),
    (37, "KEY_K"),
    (38, "KEY_L"),
    (39, "KEY_SEMICOLON"),
    (40, "KEY_APOSTROPHE"),
    (41, "KEY_GRAVE"),
    (42, "KEY_LEFTSHIFT"),
    (43, "KEY_BACKSLASH"),
    (44, "KEY_Z"),
    (45, "KEY_X"),
    (46, "KEY_C"),
    (47, "KEY_V"),
    (48, "KEY_B"),
    (49, "KEY_N"),
    (50, "KEY_M"),
    (51, "KEY_COMMA"),
    (52, "KEY_DOT"),
    (53, "KEY_SLASH"),
    (54, "KEY_RIGHTSHIFT"),
    (55, "KEY_KPASTERISK"),
    (56, "KEY_LEFTALT"),
    (57, "KEY_SPACE"),
    (58, "KEY_CAPSLOCK"),
    (59, "KEY_F1"),
    (60, "KEY_F2"),
    (61, "KEY_F3"),
    (62, "KEY_F4"),
    (63, "KEY_F5"),
    (64, "KEY_F6"),
    (65, "KEY_F7"),
    (66, "KEY_F8"),
    (67, "KEY_F9"),
    (68, "KEY_F10"),
    (69, "KEY_NUMLOCK"),
    (70, "KEY_SCROLLLOCK"),
    (71, "KEY_KP7"),
    (72, "KEY_KP8"),
    (73, "KEY_KP9"),
    (74, "KEY_KPMINUS"),
    (75, "KEY_KP4"),
    (76, "KEY_KP5"),
    (77, "KEY_KP6"),
    (78, "KEY_KPPLUS"),
    (79, "KEY_KP1"),
    (80, "KEY_KP2"),
    (81, "KEY_KP3"),
    (82, "KEY_KP0"),
    (83, "KEY_KPDOT"),
    (86, "KEY_102ND"),
    (87, "KEY_F11"),
    (88, "KEY_F12"),
    (96, "KEY_KPENTER"),
    (97, "KEY_RIGHTCTRL"),
    (98, "KEY_KPSLASH"),
    (99, "KEY_SYSRQ"),
    (100, "KEY_RIGHTALT"),
    (102, "KEY_HOME"),
    (103, "KEY_UP"),
    (104, "KEY_PAGEUP"),
    (105, "KEY_LEFT"),
    (106, "KEY_RIGHT"),
    (107, "KEY_END"),
    (108, "KEY_DOWN"),
    (109, "KEY_PAGEDOWN"),
    (110, "KEY_INSERT"),
    (111, "KEY_DELETE"),
    (113, "KEY_MUTE"),
    (114, "KEY_VOLUMEDOWN"),
    (115, "KEY_VOLUMEUP"),
    (119, "KEY_PAUSE"),
    (125, "KEY_LEFTMETA"),
    (126, "KEY_RIGHTMETA"),
    (127, "KEY_COMPOSE"),
    (163, "KEY_NEXTSONG"),
    (164, "KEY_PLAYPAUSE"),
    (165, "KEY_PREVIOUSSONG"),
    (166, "KEY_STOPCD"),
    (183, "KEY_F13"),
    (184, "KEY_F14"),
    (185, "KEY_F15"),
    (186, "KEY_F16"),
    (187, "KEY_F17"),
    (188, "KEY_F18"),
    (189, "KEY_F19"),
    (190, "KEY_F20"),
    (191, "KEY_F21"),
    (192, "KEY_F22"),
    (193, "KEY_F23"),
    (194, "KEY_F24"),
    (BTN_LEFT, "BTN_LEFT"),
    (BTN_RIGHT, "BTN_RIGHT"),
    (BTN_MIDDLE, "BTN_MIDDLE"),
    (BTN_SIDE, "BTN_SIDE"),
    (BTN_EXTRA, "BTN_EXTRA"),
];

pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
pub const BTN_SIDE: u16 = 0x113;
pub const BTN_EXTRA: u16 = 0x114;

/// Parses `KEY_F12`, `BTN_SIDE` or `code:<n>` into an evdev code.
pub fn parse_key(name: &str) -> Option<u16> {
    let name = name.trim();
    if let Some(code) = name.strip_prefix("code:") {
        return code.parse().ok();
    }
    NAMES.iter().find(|(_, n)| *n == name).map(|(c, _)| *c)
}

/// The binding name for `code`, in the form [`parse_key`] reads back.
pub fn key_name(code: u16) -> String {
    NAMES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| n.to_string())
        .unwrap_or_else(|| format!("code:{code}"))
}

/// The evdev code for a PC scan code (set 1) as Windows reports it: the
/// make code and whether it came with the `E0` prefix. Linux numbers the
/// plain keys of set 1 after their scan codes, so those map one to one;
/// the `E0` keys and F13–F24 go through a table. `None` for the fake
/// shifts (`E0 2A`, `E0 36`) the keyboard wraps around some keys, and for
/// codes without an evdev key.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn from_scan_code(make: u16, e0: bool) -> Option<u16> {
    if !e0 {
        return match make {
            0x01..=0x53 | 0x56..=0x58 => Some(make),
            0x64..=0x6e => Some(183 + (make - 0x64)),
            0x76 => Some(194),
            _ => None,
        };
    }
    Some(match make {
        0x1c => 96,
        0x1d => 97,
        0x35 => 98,
        0x37 => 99,
        0x38 => 100,
        0x47 => 102,
        0x48 => 103,
        0x49 => 104,
        0x4b => 105,
        0x4d => 106,
        0x4f => 107,
        0x50 => 108,
        0x51 => 109,
        0x52 => 110,
        0x53 => 111,
        0x20 => 113,
        0x2e => 114,
        0x30 => 115,
        0x5b => 125,
        0x5c => 126,
        0x5d => 127,
        0x19 => 163,
        0x22 => 164,
        0x10 => 165,
        0x24 => 166,
        _ => return None,
    })
}

/// The key code for Pause, which Windows reports as `E1 1D` and a virtual
/// key rather than a make code of its own.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub const KEY_PAUSE: u16 = 119;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_read_back() {
        for (code, name) in NAMES {
            assert_eq!(parse_key(name), Some(*code), "{name}");
            assert_eq!(key_name(*code), *name);
        }
        assert_eq!(key_name(752), "code:752");
        assert_eq!(parse_key("code:752"), Some(752));
        assert_eq!(parse_key("KEY_NOPE"), None);
        assert_eq!(parse_key(""), None);
    }

    /// The table says what evdev says.
    #[cfg(target_os = "linux")]
    #[test]
    fn names_are_evdev_names() {
        for (code, name) in NAMES {
            assert_eq!(
                crate::hotkey::evdev::parse_key(name).map(|k| k.code()),
                Some(*code),
                "{name}"
            );
        }
    }

    #[test]
    fn scan_codes_map_to_evdev_keys() {
        assert_eq!(from_scan_code(0x58, false), Some(88), "F12");
        assert_eq!(from_scan_code(0x1d, false), Some(29), "left ctrl");
        assert_eq!(from_scan_code(0x1d, true), Some(97), "right ctrl");
        assert_eq!(from_scan_code(0x38, true), Some(100), "AltGr");
        assert_eq!(from_scan_code(0x1c, true), Some(96), "keypad enter");
        assert_eq!(from_scan_code(0x64, false), Some(183), "F13");
        assert_eq!(from_scan_code(0x76, false), Some(194), "F24");
        assert_eq!(from_scan_code(0x2a, true), None, "fake shift");
        for make in 0..0x80u16 {
            for e0 in [false, true] {
                if let Some(code) = from_scan_code(make, e0) {
                    assert!(
                        !key_name(code).starts_with("code:"),
                        "{make:#x} {e0}: {code} has no name"
                    );
                }
            }
        }
    }
}
