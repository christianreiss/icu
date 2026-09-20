//! Short cue tones rendered on the fly (join/leave, tx, mute).

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
    let n = (48_000 * dur_ms / 1000) as usize;
    let mut out = Vec::with_capacity(n * notes.len());
    for f in notes {
        for i in 0..n {
            let t = i as f32 / 48_000.0;
            let env = (i as f32 / n as f32 * std::f32::consts::PI).sin();
            out.push((t * f * std::f32::consts::TAU).sin() * 0.125 * env);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
