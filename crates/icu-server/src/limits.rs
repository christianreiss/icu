//! Token buckets per client and per IP (`Server/IMPLEMENTATION.md` §6.6).

use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};

/// A classic token bucket with fractional tokens.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    tokens: f64,
    capacity: f64,
    rate_per_s: f64,
    last: Instant,
}

impl TokenBucket {
    /// A bucket refilling at `rate_per_s` with a burst of `capacity` tokens,
    /// starting full.
    pub fn new(rate_per_s: f64, capacity: f64) -> TokenBucket {
        TokenBucket {
            tokens: capacity,
            capacity,
            rate_per_s,
            last: Instant::now(),
        }
    }

    /// A bucket allowing `per_s` events per second with a one second burst.
    pub fn per_second(per_s: u32) -> TokenBucket {
        let per_s = f64::from(per_s.max(1));
        TokenBucket::new(per_s, per_s)
    }

    /// A bucket allowing `per_min` events per minute, burst `per_min`.
    pub fn per_minute(per_min: u32) -> TokenBucket {
        let per_min = f64::from(per_min.max(1));
        TokenBucket::new(per_min / 60.0, per_min)
    }

    /// A byte bucket for `kbit_s`, smoothed over `window`
    /// (`PROTOCOL.md` §7: video bitrate, sliding over 2 s).
    pub fn bitrate(kbit_s: u32, window: Duration) -> TokenBucket {
        let bytes_per_s = f64::from(kbit_s) * 1000.0 / 8.0;
        TokenBucket::new(bytes_per_s, bytes_per_s * window.as_secs_f64())
    }

    /// Takes `n` tokens if available. Returns `false` when the limit is hit.
    pub fn take(&mut self, n: f64) -> bool {
        self.take_at(n, Instant::now())
    }

    /// [`TokenBucket::take`] with an explicit clock, for tests.
    pub fn take_at(&mut self, n: f64, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate_per_s).min(self.capacity);
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }

    /// Takes one token.
    pub fn take_one(&mut self) -> bool {
        self.take(1.0)
    }
}

/// Counts rate limit violations in a sliding window (§6.6: the third violation
/// within 60 s is fatal).
#[derive(Debug)]
pub struct ViolationCounter {
    window: Duration,
    limit: u32,
    hits: Vec<Instant>,
}

impl ViolationCounter {
    /// A counter that turns fatal at `limit` violations within `window`.
    pub fn new(limit: u32, window: Duration) -> ViolationCounter {
        ViolationCounter {
            window,
            limit,
            hits: Vec::new(),
        }
    }

    /// Records a violation. Returns `true` when it is the fatal one.
    pub fn record(&mut self) -> bool {
        self.record_at(Instant::now())
    }

    /// [`ViolationCounter::record`] with an explicit clock, for tests.
    pub fn record_at(&mut self, now: Instant) -> bool {
        self.hits
            .retain(|t| now.saturating_duration_since(*t) < self.window);
        self.hits.push(now);
        self.hits.len() as u32 >= self.limit
    }
}

/// Per-IP buckets, shared across connections.
#[derive(Debug)]
pub struct IpLimits {
    handshakes: HashMap<IpAddr, TokenBucket>,
    enrollments: HashMap<IpAddr, TokenBucket>,
    global_enroll: TokenBucket,
    handshakes_per_s: u32,
    enroll_per_min: u32,
}

impl IpLimits {
    /// Builds the per-IP limiter from the configured limits.
    pub fn new(limits: &crate::config::LimitsSection) -> IpLimits {
        IpLimits {
            handshakes: HashMap::new(),
            enrollments: HashMap::new(),
            global_enroll: TokenBucket::per_minute(limits.enroll_global_per_min),
            handshakes_per_s: limits.handshakes_per_ip_per_s,
            enroll_per_min: limits.enroll_per_ip_per_min,
        }
    }

    /// Whether this IP may start another handshake.
    pub fn allow_handshake(&mut self, ip: IpAddr) -> bool {
        let per_s = self.handshakes_per_s;
        self.handshakes
            .entry(ip)
            .or_insert_with(|| TokenBucket::per_second(per_s))
            .take_one()
    }

    /// Whether this IP may start another enrollment. Checks the per-IP bucket
    /// first, then the global one.
    pub fn allow_enroll(&mut self, ip: IpAddr) -> bool {
        let per_min = self.enroll_per_min;
        let ok = self
            .enrollments
            .entry(ip)
            .or_insert_with(|| TokenBucket::per_minute(per_min))
            .take_one();
        ok && self.global_enroll.take_one()
    }

    /// Drops buckets that have been full again for a while, so the maps do not
    /// grow without bound.
    pub fn prune(&mut self) {
        let now = Instant::now();
        let stale = Duration::from_secs(300);
        self.handshakes
            .retain(|_, b| now.saturating_duration_since(b.last) < stale);
        self.enrollments
            .retain(|_, b| now.saturating_duration_since(b.last) < stale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_allows_a_burst_then_refills() {
        let t0 = Instant::now();
        let mut b = TokenBucket::per_second(110);
        for i in 0..110 {
            assert!(b.take_at(1.0, t0), "packet {i} of the burst");
        }
        assert!(!b.take_at(1.0, t0), "111th packet in the same instant");
        // Half a second later, 55 more fit.
        let t1 = t0 + Duration::from_millis(500);
        for _ in 0..55 {
            assert!(b.take_at(1.0, t1));
        }
        assert!(!b.take_at(1.0, t1));
    }

    #[test]
    fn a_bucket_does_not_accumulate_beyond_its_capacity() {
        let t0 = Instant::now();
        let mut b = TokenBucket::per_second(10);
        assert!(b.take_at(1.0, t0));
        let later = t0 + Duration::from_secs(3600);
        for _ in 0..10 {
            assert!(b.take_at(1.0, later));
        }
        assert!(!b.take_at(1.0, later), "capacity is one second worth");
    }

    #[test]
    fn a_bitrate_bucket_smooths_over_the_window() {
        let t0 = Instant::now();
        // 400 kbit/s = 50_000 byte/s, 2 s window = 100_000 byte burst.
        let mut b = TokenBucket::bitrate(400, Duration::from_secs(2));
        assert!(b.take_at(100_000.0, t0));
        assert!(!b.take_at(1.0, t0));
        assert!(b.take_at(50_000.0, t0 + Duration::from_secs(1)));
    }

    #[test]
    fn the_third_violation_in_the_window_is_fatal() {
        let t0 = Instant::now();
        let mut v = ViolationCounter::new(3, Duration::from_secs(60));
        assert!(!v.record_at(t0));
        assert!(!v.record_at(t0 + Duration::from_secs(10)));
        assert!(v.record_at(t0 + Duration::from_secs(20)), "third in 60 s");
    }

    #[test]
    fn violations_age_out_of_the_window() {
        let t0 = Instant::now();
        let mut v = ViolationCounter::new(3, Duration::from_secs(60));
        assert!(!v.record_at(t0));
        assert!(!v.record_at(t0 + Duration::from_secs(30)));
        // The first one is 61 s old by now and no longer counts.
        assert!(!v.record_at(t0 + Duration::from_secs(61)));
        assert!(v.record_at(t0 + Duration::from_secs(62)));
    }

    #[test]
    fn per_ip_limits_are_independent() {
        let mut limits = IpLimits::new(&crate::config::LimitsSection::default());
        let a: IpAddr = "192.0.2.1".parse().unwrap();
        let b: IpAddr = "192.0.2.2".parse().unwrap();
        for _ in 0..5 {
            assert!(limits.allow_enroll(a));
        }
        assert!(!limits.allow_enroll(a), "5/min per IP");
        assert!(limits.allow_enroll(b), "another IP has its own bucket");
    }

    #[test]
    fn the_global_enroll_limit_bites_after_20() {
        let mut limits = IpLimits::new(&crate::config::LimitsSection::default());
        let mut allowed = 0;
        for i in 0..10 {
            let ip: IpAddr = format!("192.0.2.{i}").parse().unwrap();
            for _ in 0..5 {
                if limits.allow_enroll(ip) {
                    allowed += 1;
                }
            }
        }
        assert_eq!(allowed, 20, "20/min server wide");
    }
}
