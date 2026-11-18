//! Shared monotonic capture time and bounded audio-led video presentation.
//! Device and callback clocks never replace the audio jitter sample counter.
use std::{
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Capture timestamp carried unchanged through the media pipelines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stamp {
    pub micros: u64,
    pub epoch: u32,
}
impl Stamp {
    pub fn before_samples(self, samples: usize) -> Self {
        Self {
            micros: self
                .micros
                .saturating_sub(samples as u64 * 1_000_000 / 48_000),
            ..self
        }
    }
}

/// Exact app-ring span consumed by one output callback. Mapping is never
/// extrapolated across the silence of an underrun.
#[derive(Clone, Copy, Debug)]
pub struct Rendered {
    pub version: u64,
    pub start: u64,
    pub count: u64,
    pub micros: u64,
    pub gap: u64,
    pub complete: bool,
    pub accurate: bool,
}
impl Rendered {
    pub fn sample_time(self, position: u64) -> Option<u64> {
        if !self.complete || position < self.start || position - self.start >= self.count {
            return None;
        }
        Some(
            self.micros
                .saturating_add((position - self.start) * 1_000_000 / 48_000),
        )
    }
}
/// All payload fields are atomics: no Rust data races, even on a failed read.
/// Single callback writer; readers never spin or hold up a real-time thread.
#[derive(Default)]
pub struct Position {
    version: AtomicU64,
    frames: AtomicU64,
    micros: AtomicU64,
    count: AtomicU64,
    gap: AtomicU64,
    complete: AtomicBool,
    accurate: AtomicBool,
}
impl Position {
    pub fn store(&self, report: Rendered) {
        self.version.fetch_add(1, Ordering::AcqRel);
        self.frames.store(report.start, Ordering::Relaxed);
        self.micros.store(report.micros, Ordering::Relaxed);
        self.count.store(report.count, Ordering::Relaxed);
        self.gap.store(report.gap, Ordering::Relaxed);
        self.complete.store(report.complete, Ordering::Relaxed);
        self.accurate.store(report.accurate, Ordering::Relaxed);
        self.version.fetch_add(1, Ordering::Release);
    }
    pub fn load(&self) -> Option<Rendered> {
        let v = self.version.load(Ordering::Acquire);
        if v & 1 != 0 {
            return None;
        }
        let report = Rendered {
            version: v,
            start: self.frames.load(Ordering::Relaxed),
            micros: self.micros.load(Ordering::Relaxed),
            count: self.count.load(Ordering::Relaxed),
            gap: self.gap.load(Ordering::Relaxed),
            complete: self.complete.load(Ordering::Relaxed),
            accurate: self.accurate.load(Ordering::Relaxed),
        };
        std::sync::atomic::fence(Ordering::Acquire);
        (v == self.version.load(Ordering::Relaxed) && report.micros > 0).then_some(report)
    }
}

/// Timing travels with each captured sample, so a concurrent callback cannot
/// change the timestamp of a block already waiting in the DSP ring.
#[derive(Clone, Copy, Debug, Default)]
pub struct CaptureSample {
    pub value: f32,
    pub stamp: Stamp,
    pub accurate: bool,
}

/// One clock for the engine, including camera stop/start and silent periods.
pub struct Clock {
    origin: Instant,
    pub epoch: AtomicU32,
    pub playback: Position,
    pub consumed: AtomicU64,
    pub playback_gap: AtomicU64,
}
impl Default for Clock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            epoch: AtomicU32::new(1),
            playback: Position::default(),
            consumed: AtomicU64::new(0),
            playback_gap: AtomicU64::new(0),
        }
    }
}
impl Clock {
    pub fn micros(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.origin)
            .as_micros()
            .min(u64::MAX as u128) as u64
            + 1
    }
    pub fn instant(&self, micros: u64) -> Instant {
        self.origin + Duration::from_micros(micros.saturating_sub(1))
    }
    pub fn stamp(&self, at: Instant) -> Stamp {
        Stamp {
            micros: self.micros(at),
            epoch: self.epoch.load(Ordering::Acquire),
        }
    }
    pub fn reset(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

/// Convert the camera library's wall-clock-derived capture report once, at
/// acquisition. Implausible/stepped reports cannot become future deadlines.
#[derive(Default)]
pub struct CameraTime {
    last: Option<(Duration, Instant)>,
}
impl CameraTime {
    pub fn convert(
        &mut self,
        reported: Option<Duration>,
        now: Instant,
        wall: SystemTime,
    ) -> (Instant, bool) {
        let Some(reported) = reported else {
            return (now, false);
        };
        let valid_delta = self.last.is_none_or(|(old, local)| {
            reported.checked_sub(old).is_some_and(|d| {
                d.abs_diff(now.saturating_duration_since(local)) < Duration::from_millis(250)
            })
        });
        self.last = Some((reported, now));
        let age = wall
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|w| w.checked_sub(reported));
        match age {
            Some(age) if valid_delta && age <= Duration::from_secs(1) => {
                (now.checked_sub(age).unwrap_or(now), true)
            }
            _ => (now, false),
        }
    }
}

/// A real source sample and its estimated hardware presentation time.
#[derive(Clone, Copy, Debug)]
pub struct Playout {
    pub stamp: Stamp,
    pub at: Instant,
}
impl Playout {
    pub fn target(self, stamp: Stamp, now: Instant) -> Option<Instant> {
        if stamp.epoch == 0
            || stamp.epoch != self.stamp.epoch
            || now.saturating_duration_since(self.at) > Duration::from_secs(2)
        {
            return None;
        }
        let delta = i128::from(stamp.micros) - i128::from(self.stamp.micros);
        if delta.abs() > 2_000_000 {
            return None;
        }
        if delta >= 0 {
            self.at.checked_add(Duration::from_micros(delta as u64))
        } else {
            self.at.checked_sub(Duration::from_micros((-delta) as u64))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_and_total_underruns_cannot_date_future_audio() {
        let mut report = Rendered {
            version: 0,
            start: 100,
            count: 240,
            micros: 1_000_000,
            gap: 0,
            complete: true,
            accurate: true,
        };
        assert_eq!(report.sample_time(100), Some(1_000_000));
        assert_eq!(report.sample_time(220), Some(1_002_500));
        assert_eq!(
            report.sample_time(340),
            None,
            "queued but unconsumed sample has no mapping"
        );
        assert_eq!(report.sample_time(99), None);
        report.complete = false;
        report.gap = 1;
        assert_eq!(
            report.sample_time(100),
            None,
            "partial underrun invalidates extrapolation"
        );
        report.count = 0;
        assert_eq!(report.sample_time(100), None);
    }
    #[test]
    fn camera_age_and_wall_steps_are_not_processing_time() {
        let mut bridge = CameraTime::default();
        let now = Instant::now();
        let wall = UNIX_EPOCH + Duration::from_secs(10);
        assert_eq!(
            bridge.convert(Some(Duration::from_millis(9960)), now, wall),
            (now - Duration::from_millis(40), true)
        );
        assert_eq!(
            bridge.convert(
                Some(Duration::from_secs(30)),
                now + Duration::from_millis(66),
                wall
            ),
            (now + Duration::from_millis(66), false)
        );
        assert!(!bridge.convert(None, now, wall).1);
    }
    #[test]
    fn separate_startup_and_epoch_changes_cannot_align_wrong_media() {
        let now = Instant::now();
        let p = Playout {
            stamp: Stamp {
                micros: 9_000_000,
                epoch: 7,
            },
            at: now,
        };
        assert_eq!(
            p.target(
                Stamp {
                    micros: 9_040_000,
                    epoch: 7
                },
                now
            ),
            Some(now + Duration::from_millis(40))
        );
        assert!(p
            .target(
                Stamp {
                    micros: 9_040_000,
                    epoch: 8
                },
                now
            )
            .is_none());
        assert!(p.target(p.stamp, now + Duration::from_secs(3)).is_none());
        assert_eq!(
            Stamp {
                micros: 1_000_000,
                epoch: 7
            }
            .before_samples(960)
            .micros,
            980_000
        );
    }
    #[test]
    fn long_running_shared_clock_does_not_use_wrapping_sample_counters() {
        let now = Instant::now();
        for minute in 0..60 {
            let t = minute * 60_000_000u64;
            let p = Playout {
                stamp: Stamp {
                    micros: t,
                    epoch: 1,
                },
                at: now,
            };
            assert_eq!(
                p.target(
                    Stamp {
                        micros: t + 20_000,
                        epoch: 1
                    },
                    now
                ),
                Some(now + Duration::from_millis(20))
            );
        }
    }
}
