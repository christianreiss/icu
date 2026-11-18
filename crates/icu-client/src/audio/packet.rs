//! Capture packet timeline shared with the portable WASAPI regressions.

/// The first WASAPI packet may carry DATA_DISCONTINUITY as a startup marker.
/// A later marker breaks the clock shared by capture and echo reference.
#[derive(Default)]
pub(super) struct CaptureTimeline {
    started: bool,
}

impl CaptureTimeline {
    /// Discard pre-gap samples and ask the caller to stop this stream. The
    /// engine reopens its whole audio group, so no post-gap samples reach a
    /// DSP chain still holding the old capture/reference timeline.
    pub(super) fn begin_packet(&mut self, pending: &mut Vec<f32>, discontinuous: bool) -> bool {
        let reset = self.started && discontinuous;
        self.started = true;
        if reset {
            pending.clear();
        }
        reset
    }
}

/// Delay from the endpoint's correlated clock reading to the first frame
/// of the next write. WASAPI positions are stream-relative, in units of its
/// reported clock frequency. Reject startup, underruns and inconsistent clocks.
pub(super) fn render_lead(
    written: u64,
    position: u64,
    frequency: u64,
) -> Option<std::time::Duration> {
    if frequency == 0 || position == 0 {
        return None;
    }
    let submitted = u128::from(written) * 1_000_000_000 / u128::from(super::SAMPLE_RATE);
    let played = u128::from(position) * 1_000_000_000 / u128::from(frequency);
    let lead = submitted.checked_sub(played)?;
    (lead <= 1_000_000_000).then(|| std::time::Duration::from_nanos(lead as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_clock_maps_frame_positions_across_native_frequencies() {
        use std::time::Duration;
        for frequency in [48_000, 384_000, 10_000_000] {
            // 300 ms submitted beyond the device position after two hours.
            let written = (7200 * 48_000) + 14_400;
            assert_eq!(
                render_lead(written, 7200 * frequency, frequency),
                Some(Duration::from_millis(300))
            );
        }
        assert_eq!(render_lead(480, 0, 48_000), None);
        assert_eq!(render_lead(480, 400, 0), None);
        assert_eq!(render_lead(480, 481, 48_000), None);
        assert_eq!(render_lead(u64::MAX, 1, 1), None);
    }

    #[test]
    fn a_driver_gap_discards_the_pre_gap_partial_frame() {
        let mut timeline = CaptureTimeline::default();
        let mut pending = Vec::new();
        assert!(!timeline.begin_packet(&mut pending, false));
        pending.extend([0.75; 120]);
        assert!(timeline.begin_packet(&mut pending, true));
        assert!(
            pending.is_empty(),
            "old half-frame cannot join post-gap audio"
        );
    }

    #[test]
    fn startup_flag_does_not_cause_a_reopen_loop() {
        for _ in 0..3 {
            let mut timeline = CaptureTimeline::default();
            let mut pending = Vec::new();
            assert!(!timeline.begin_packet(&mut pending, true));
            pending.extend([0.25; 120]);
            assert!(!timeline.begin_packet(&mut pending, false));
            assert_eq!(pending, [0.25; 120]);
        }
    }
}
