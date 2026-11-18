//! Capture packet timeline shared with the portable WASAPI regressions.

/// Up to 20 ms of known capture loss can be represented as silence without
/// interrupting the render clock. Larger or unmeasurable gaps start a new epoch.
pub(super) const MAX_GAP: usize = super::SAMPLE_RATE as usize / 50;

#[derive(Default)]
pub(super) struct CaptureTimeline {
    started: bool,
    next: Option<u64>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct PacketRepair {
    pub silence: usize,
    pub skip: usize,
}

impl CaptureTimeline {
    pub(super) fn begin_packet(
        &mut self,
        position: Option<u64>,
        frames: u32,
        discontinuous: bool,
    ) -> Option<PacketRepair> {
        let expected = self.next;
        self.next = position.and_then(|start| start.checked_add(u64::from(frames)));
        let started = std::mem::replace(&mut self.started, true);
        if !started || !discontinuous {
            return Some(PacketRepair::default());
        }
        let delta = i128::from(position?) - i128::from(expected?);
        if delta.unsigned_abs() > MAX_GAP as u128 || delta < 0 && -delta >= i128::from(frames) {
            return None;
        }
        Some(PacketRepair {
            silence: delta.max(0) as usize,
            skip: (-delta).max(0) as usize,
        })
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
    fn repair_small_measurable_capture_glitches_without_resetting_render() {
        let mut timeline = CaptureTimeline::default();
        let normal = Some(PacketRepair::default());
        assert_eq!(timeline.begin_packet(Some(0), 256, true), normal);
        assert_eq!(timeline.begin_packet(Some(256), 256, true), normal);
        assert_eq!(
            timeline.begin_packet(Some(768), 488, true),
            Some(PacketRepair {
                silence: 256,
                skip: 0
            })
        );
        assert_eq!(
            timeline.begin_packet(Some(1251), 480, true),
            Some(PacketRepair {
                silence: 0,
                skip: 5
            })
        );
        assert_eq!(timeline.begin_packet(Some(1731), 480, true), normal);
    }

    #[test]
    fn start_a_new_epoch_for_unbounded_replayed_or_unmeasurable_gaps() {
        for (first, second) in [
            (Some(0), Some(480 + MAX_GAP as u64 + 1)),
            (Some(0), Some(0)),
            (Some(0), None),
            (None, Some(480)),
            (Some(u64::MAX), Some(0)),
        ] {
            let mut timeline = CaptureTimeline::default();
            assert!(timeline.begin_packet(first, 480, true).is_some());
            assert!(timeline.begin_packet(second, 480, true).is_none());
        }
    }
}
