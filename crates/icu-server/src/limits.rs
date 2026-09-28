//! Token buckets per client and per IP (`docs/reference/server.md` §6.6).

use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};

/// Control frames queued per client before it counts as not reading
/// (`Room`'s bounded out queue). Steady state is a `UserRtt` every 2 s plus
/// the odd `UserState`; a join storm of 16 users is well under 100 frames.
/// Reaching this means the client's control stream has stalled for tens of
/// seconds, and it is closed with `TIMEOUT` instead of buffered for.
pub const CONTROL_QUEUE_FRAMES: usize = 256;

/// Most source addresses the per-IP handshake table tracks. Spoofed Initials
/// can name any address, so the table has to have a ceiling; past it,
/// unknown addresses are sent through a Retry (or refused) instead of being
/// added (`endpoint::accept_loop`).
pub const MAX_TRACKED_IPS: usize = 4096;

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

    /// A bucket allowing `per_hour` events per hour, burst `per_hour`.
    pub fn per_hour(per_hour: u32) -> TokenBucket {
        let per_hour = f64::from(per_hour.max(1));
        TokenBucket::new(per_hour / 3600.0, per_hour)
    }

    /// A byte bucket for `kbit_s`, smoothed over `window`
    /// (`docs/reference/protocol.md` §7: video bitrate, sliding over 2 s).
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
        self.refill(now);
        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }

    /// Credits the tokens earned since the last call, up to the capacity.
    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.rate_per_s).min(self.capacity);
    }

    /// Takes one token.
    pub fn take_one(&mut self) -> bool {
        self.take(1.0)
    }

    /// Gives back a token taken for something that then did not happen, up
    /// to the capacity.
    pub fn refund_one(&mut self) {
        self.tokens = (self.tokens + 1.0).min(self.capacity);
    }

    /// Takes `n` tokens whenever the bucket is not in debt, even if that
    /// drives it below zero; the debt is then paid off by the refill before
    /// anything else passes. For byte budgets, where a single item (one
    /// video frame, up to 512 KiB) may be larger than the whole burst
    /// (100 KB at 400 kbit/s over 2 s) and would otherwise never fit.
    pub fn take_debt(&mut self, n: f64) -> bool {
        self.take_debt_at(n, Instant::now())
    }

    /// Whether the bucket is not in debt: what [`TokenBucket::take_debt`]
    /// would decide, without taking anything. Lets a byte budget refuse an
    /// item before reading it, and [`TokenBucket::charge`] it as it arrives.
    pub fn has_credit(&mut self) -> bool {
        self.has_credit_at(Instant::now())
    }

    /// [`TokenBucket::has_credit`] with an explicit clock, for tests.
    pub fn has_credit_at(&mut self, now: Instant) -> bool {
        self.refill(now);
        self.tokens > 0.0
    }

    /// Takes `n` tokens unconditionally, into debt if need be: bytes that
    /// were already admitted and have now arrived.
    pub fn charge(&mut self, n: f64) {
        self.charge_at(n, Instant::now());
    }

    /// [`TokenBucket::charge`] with an explicit clock, for tests.
    pub fn charge_at(&mut self, n: f64, now: Instant) {
        self.refill(now);
        self.tokens -= n;
    }

    /// [`TokenBucket::take_debt`] with an explicit clock, for tests.
    pub fn take_debt_at(&mut self, n: f64, now: Instant) -> bool {
        self.refill(now);
        if self.tokens > 0.0 {
            self.tokens -= n;
            true
        } else {
            false
        }
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

/// The key a source address is rate limited under: IPv4 as is, IPv6 by its
/// /64 ([`icu_proto::limits::IPV6_LIMIT_PREFIX`]). One host commonly holds a
/// whole /64 and can pick any source address in it, so a per-address bucket
/// would give it 2^64 budgets. An IPv4-mapped address counts as the IPv4 one.
pub fn limit_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let prefix = u32::from(icu_proto::limits::IPV6_LIMIT_PREFIX);
                let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
                IpAddr::V6((u128::from(v6) & mask).into())
            }
        },
    }
}

/// The IPv6 /48 an address belongs to, `None` for IPv4 (and IPv4-mapped):
/// one customer usually holds a whole /48, so its /64s share a ceiling.
fn prefix_48(ip: IpAddr) -> Option<IpAddr> {
    match limit_key(ip) {
        IpAddr::V4(_) => None,
        IpAddr::V6(v6) => {
            let mask = u128::MAX << (128 - 48);
            Some(IpAddr::V6((u128::from(v6) & mask).into()))
        }
    }
}

/// Per-IP buckets, shared across connections. Keyed by [`limit_key`].
#[derive(Debug)]
pub struct IpLimits {
    handshakes: HashMap<IpAddr, TokenBucket>,
    enrollments: HashMap<IpAddr, TokenBucket>,
    global_enroll: TokenBucket,
    global_handshakes: TokenBucket,
    handshakes_per_s: u32,
    enroll_per_min: u32,
    /// Validated handshakes in flight per [`limit_key`]. Entries go when
    /// their count drops to zero, so the map holds at most one entry per
    /// handshake in flight.
    in_flight: HashMap<IpAddr, u32>,
    /// The same per IPv6 /48 ([`prefix_48`]).
    in_flight_48: HashMap<IpAddr, u32>,
}

impl IpLimits {
    /// Builds the per-IP limiter from the configured limits.
    pub fn new(limits: &crate::config::LimitsSection) -> IpLimits {
        IpLimits {
            handshakes: HashMap::new(),
            enrollments: HashMap::new(),
            global_enroll: TokenBucket::per_minute(limits.enroll_global_per_min),
            global_handshakes: TokenBucket::per_second(limits.handshakes_global_per_s),
            handshakes_per_s: limits.handshakes_per_ip_per_s,
            enroll_per_min: limits.enroll_per_ip_per_min,
            in_flight: HashMap::new(),
            in_flight_48: HashMap::new(),
        }
    }

    /// Whether a validated source may start one more handshake: fewer than
    /// [`HANDSHAKES_IN_FLIGHT_PER_IP`](icu_proto::limits::HANDSHAKES_IN_FLIGHT_PER_IP)
    /// of its own are in flight, and for IPv6 fewer than
    /// [`HANDSHAKES_IN_FLIGHT_PER_48`](icu_proto::limits::HANDSHAKES_IN_FLIGHT_PER_48)
    /// from its /48. The rate bucket alone lets a source that answers the
    /// Retry and then stalls pile up handshakes for as long as each one may
    /// take; this caps what it holds at any moment.
    ///
    /// Only for addresses that passed the Retry: an unvalidated one may be
    /// spoofed, and counting it would let anybody use up a victim's slots.
    pub fn handshake_slot_free(&self, ip: IpAddr) -> bool {
        use icu_proto::limits::{HANDSHAKES_IN_FLIGHT_PER_48, HANDSHAKES_IN_FLIGHT_PER_IP};
        let own = self.in_flight.get(&limit_key(ip)).copied().unwrap_or(0);
        let wide = prefix_48(ip)
            .and_then(|p| self.in_flight_48.get(&p).copied())
            .unwrap_or(0);
        own < HANDSHAKES_IN_FLIGHT_PER_IP && wide < HANDSHAKES_IN_FLIGHT_PER_48
    }

    /// Books a validated handshake from `ip` as in flight; pair with
    /// [`IpLimits::end_handshake`].
    pub fn start_handshake(&mut self, ip: IpAddr) {
        *self.in_flight.entry(limit_key(ip)).or_default() += 1;
        if let Some(p) = prefix_48(ip) {
            *self.in_flight_48.entry(p).or_default() += 1;
        }
    }

    /// A handshake booked by [`IpLimits::start_handshake`] is over, however
    /// it ended.
    pub fn end_handshake(&mut self, ip: IpAddr) {
        fn release(map: &mut HashMap<IpAddr, u32>, key: IpAddr) {
            if let Some(n) = map.get_mut(&key) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    map.remove(&key);
                }
            }
        }
        release(&mut self.in_flight, limit_key(ip));
        if let Some(p) = prefix_48(ip) {
            release(&mut self.in_flight_48, p);
        }
    }

    /// Whether this IP may start another handshake: its own bucket first,
    /// then the server wide one. `None` when the IP is not tracked yet and
    /// the table is full even after pruning — the caller then decides
    /// without a per-IP budget, see [`IpLimits::allow_untracked_handshake`].
    pub fn allow_handshake(&mut self, ip: IpAddr) -> Option<bool> {
        let ip = limit_key(ip);
        if !self.handshakes.contains_key(&ip) && self.handshakes.len() >= MAX_TRACKED_IPS {
            self.prune();
            if self.handshakes.len() >= MAX_TRACKED_IPS {
                return None;
            }
        }
        let per_s = self.handshakes_per_s;
        let ok = self
            .handshakes
            .entry(ip)
            .or_insert_with(|| TokenBucket::per_second(per_s))
            .take_one();
        Some(ok && self.global_handshakes.take_one())
    }

    /// A handshake from an address the full table could not take: only the
    /// server wide budget applies.
    pub fn allow_untracked_handshake(&mut self) -> bool {
        self.global_handshakes.take_one()
    }

    /// Whether this IP may start another enrollment. Checks the per-IP bucket
    /// first, then the global one.
    pub fn allow_enroll(&mut self, ip: IpAddr) -> bool {
        let per_min = self.enroll_per_min;
        let ok = self
            .enrollments
            .entry(limit_key(ip))
            .or_insert_with(|| TokenBucket::per_minute(per_min))
            .take_one();
        ok && self.global_enroll.take_one()
    }

    /// Drops buckets that have been full again for a while, so the maps do not
    /// grow without bound. A handshake bucket refills within a second, so it
    /// goes after 10 s; an enrollment bucket needs its full minute.
    pub fn prune(&mut self) {
        let now = Instant::now();
        let stale = Duration::from_secs(300);
        self.handshakes
            .retain(|_, b| now.saturating_duration_since(b.last) < Duration::from_secs(10));
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
    fn a_debt_bucket_passes_an_item_larger_than_its_capacity_then_pays_it_off() {
        let t0 = Instant::now();
        // 400 kbit/s: 50_000 byte/s, 100_000 byte burst.
        let mut b = TokenBucket::bitrate(400, Duration::from_secs(2));
        assert!(b.take_debt_at(150_000.0, t0), "bigger than the burst");
        assert!(!b.take_debt_at(1.0, t0), "in debt");
        assert!(!b.take_debt_at(1.0, t0 + Duration::from_millis(900)));
        assert!(b.take_debt_at(1.0, t0 + Duration::from_millis(1100)));
    }

    #[test]
    fn charged_bytes_run_into_debt_that_refuses_the_next_item() {
        let t0 = Instant::now();
        let mut b = TokenBucket::bitrate(400, Duration::from_secs(2));
        assert!(b.has_credit_at(t0));
        b.charge_at(60_000.0, t0);
        assert!(b.has_credit_at(t0), "within the burst");
        b.charge_at(90_000.0, t0);
        assert!(!b.has_credit_at(t0), "50 000 bytes in debt");
        assert!(!b.has_credit_at(t0 + Duration::from_millis(900)));
        assert!(b.has_credit_at(t0 + Duration::from_millis(1100)));
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
    fn ipv6_is_limited_per_64() {
        let mut limits = IpLimits::new(&crate::config::LimitsSection::default());
        for i in 0..10u16 {
            let ip: IpAddr = format!("2001:db8:1:2::{i:x}").parse().unwrap();
            assert_eq!(limits.allow_handshake(ip), Some(true), "handshake {i}");
        }
        let same_64: IpAddr = "2001:db8:1:2:ffff::1".parse().unwrap();
        assert_eq!(
            limits.allow_handshake(same_64),
            Some(false),
            "11th from the /64"
        );
        let other_64: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(limits.allow_handshake(other_64), Some(true));
    }

    #[test]
    fn a_mapped_ipv4_address_shares_the_ipv4_bucket() {
        let v4: IpAddr = "192.0.2.1".parse().unwrap();
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        assert_eq!(limit_key(mapped), v4);
    }

    #[test]
    fn the_global_handshake_limit_bites_across_addresses() {
        let mut limits = IpLimits::new(&crate::config::LimitsSection {
            handshakes_global_per_s: 25,
            ..Default::default()
        });
        let allowed = (0..50u32)
            .filter(|i| {
                let ip = IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000 + i));
                limits.allow_handshake(ip) == Some(true)
            })
            .count();
        assert_eq!(allowed, 25);
        assert!(
            !limits.allow_untracked_handshake(),
            "shared with untracked ones"
        );
    }

    #[test]
    fn the_handshake_table_has_a_ceiling() {
        let mut limits = IpLimits::new(&crate::config::LimitsSection {
            handshakes_global_per_s: u32::MAX,
            ..Default::default()
        });
        for i in 0..MAX_TRACKED_IPS as u32 {
            let ip = IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000 + i));
            assert_eq!(limits.allow_handshake(ip), Some(true));
        }
        let fresh: IpAddr = "192.0.2.1".parse().unwrap();
        assert_eq!(
            limits.allow_handshake(fresh),
            None,
            "full, and nothing stale"
        );
        let known = IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000));
        assert!(
            limits.allow_handshake(known).is_some(),
            "known IPs keep their budget"
        );
    }

    #[test]
    fn a_source_holds_four_handshakes_at_a_time() {
        let mut limits = IpLimits::new(&crate::config::LimitsSection::default());
        let a: IpAddr = "192.0.2.1".parse().unwrap();
        let b: IpAddr = "192.0.2.2".parse().unwrap();
        for _ in 0..4 {
            assert!(limits.handshake_slot_free(a));
            limits.start_handshake(a);
        }
        assert!(
            !limits.handshake_slot_free(a),
            "fifth from the same address"
        );
        assert!(
            limits.handshake_slot_free(b),
            "another address is not affected"
        );
        limits.end_handshake(a);
        assert!(
            limits.handshake_slot_free(a),
            "a finished one frees its slot"
        );
        for _ in 0..3 {
            limits.end_handshake(a);
        }
        assert!(
            limits.in_flight.is_empty(),
            "no entry outlives its handshakes"
        );
    }

    #[test]
    fn ipv6_handshakes_in_flight_count_per_64_and_per_48() {
        let mut limits = IpLimits::new(&crate::config::LimitsSection::default());
        let one_64 = |i: u16| -> IpAddr { format!("2001:db8:1:2::{i:x}").parse().unwrap() };
        for i in 0..4 {
            assert!(limits.handshake_slot_free(one_64(i)));
            limits.start_handshake(one_64(i));
        }
        assert!(
            !limits.handshake_slot_free(one_64(9)),
            "the /64 is one source"
        );
        // Three more /64s of the same /48 fill it up to 16.
        for net in 3..6u16 {
            for i in 0..4u16 {
                let ip: IpAddr = format!("2001:db8:1:{net:x}::{i:x}").parse().unwrap();
                assert!(limits.handshake_slot_free(ip), "{ip}");
                limits.start_handshake(ip);
            }
        }
        let fresh_64: IpAddr = "2001:db8:1:ff::1".parse().unwrap();
        assert!(!limits.handshake_slot_free(fresh_64), "the /48 is full");
        let other_48: IpAddr = "2001:db8:2::1".parse().unwrap();
        assert!(limits.handshake_slot_free(other_48));
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        limits.start_handshake(mapped);
        assert_eq!(
            limits
                .in_flight
                .get(&"192.0.2.1".parse::<IpAddr>().unwrap()),
            Some(&1),
            "a mapped address counts as IPv4, without a /48"
        );
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
