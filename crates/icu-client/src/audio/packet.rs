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

#[cfg(test)]
mod tests {
    use super::*;

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
