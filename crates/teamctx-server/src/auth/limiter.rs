//! In-memory limit on failed authentications per client address.
//!
//! Numbers (see [`FailureLimiter::default`]):
//! - **10 failures per 60-second window** per client. The window starts at the client's
//!   first failure. From the 10th failure until the window ends, every request from that
//!   client gets `429 Too Many Requests` with `Retry-After`, valid token or not, so a
//!   guesser makes at most 10 attempts a minute per address.
//! - **Attempts in flight count against the limit.** Each request reserves an attempt
//!   before its token is looked up; a success refunds it, a failure keeps it. A client
//!   with `f` failures in its window may therefore have at most `10 - f` lookups pending
//!   at once, so a concurrent burst against a slow database cannot get more than 10
//!   guesses checked. A request beyond that gets `429` with `Retry-After: 1` (pending
//!   lookups finish quickly). An attempt stays reserved until its lookup has finished,
//!   even if the client disconnects first.
//! - **At most 4,096 clients tracked.** When the table is full, expired windows without
//!   pending attempts are dropped first, then the window that started earliest
//!   (preferring one without pending attempts). Memory stays bounded (a few hundred KiB)
//!   whatever an attacker sends; the cost is that an attacker with more than 4,096
//!   addresses can push older windows out, which still leaves 256-bit tokens out of
//!   reach. A client whose attempts all succeed is not tracked once they finish.
//!
//! A client is an IPv4 address or an IPv6 `/64` (one host usually owns a whole `/64`, so
//! rotating addresses inside it does not reset the count). `X-Forwarded-For` is never
//! trusted: behind a reverse proxy every request shares the proxy's address, so one bad
//! client can lock out everyone for a minute. Requests served without connection info
//! share a single bucket for the same reason (fail closed, not open).

use std::{
    collections::HashMap,
    fmt,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

/// Who a failure is counted against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ClientKey {
    /// The server was not given connection info; all such requests share one bucket.
    Unknown,
    /// An IPv4 client (including IPv4-mapped IPv6).
    V4(Ipv4Addr),
    /// The upper 64 bits of an IPv6 client address.
    V6Prefix(u64),
}

impl ClientKey {
    pub(crate) fn from_ip(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => Self::V4(v4),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self::V4(v4),
                None => Self::V6Prefix((v6.to_bits() >> 64) as u64),
            },
        }
    }
}

impl fmt::Display for ClientKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("unknown"),
            Self::V4(ip) => write!(f, "{ip}"),
            Self::V6Prefix(prefix) => {
                let groups = [48, 32, 16, 0].map(|shift| (prefix >> shift) & 0xffff);
                write!(
                    f,
                    "{:x}:{:x}:{:x}:{:x}::/64",
                    groups[0], groups[1], groups[2], groups[3]
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Window {
    /// Time of the first failure in this window (or of the first reservation while
    /// `failures` is zero; it is reset by the first failure).
    started: Instant,
    failures: u32,
    /// Attempts admitted and not yet settled.
    pending: u32,
}

impl Window {
    fn expired(&self, now: Instant, length: Duration) -> bool {
        now.saturating_duration_since(self.started) >= length
    }
}

/// Counts failed authentications per client in fixed windows. Thread-safe.
#[derive(Debug)]
pub struct FailureLimiter {
    windows: Mutex<HashMap<ClientKey, Window>>,
    max_failures: u32,
    window: Duration,
    capacity: usize,
}

impl Default for FailureLimiter {
    /// 10 failures per 60 seconds, 4,096 clients (see the module docs).
    fn default() -> Self {
        Self::new(10, Duration::from_secs(60), 4096)
    }
}

impl FailureLimiter {
    /// A limiter allowing `max_failures` per `window` for up to `capacity` clients.
    /// Zero values are raised to one.
    #[must_use]
    pub fn new(max_failures: u32, window: Duration, capacity: usize) -> Self {
        Self {
            windows: Mutex::new(HashMap::new()),
            max_failures: max_failures.max(1),
            window: window.max(Duration::from_millis(1)),
            capacity: capacity.max(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ClientKey, Window>> {
        // The map holds plain counters, so a panic elsewhere cannot leave it
        // inconsistent; keep limiting rather than failing every request.
        self.windows.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserves one attempt for `client` at `now`, checking and counting under one lock.
    ///
    /// # Errors
    ///
    /// How long the client should wait: the rest of its window when it is locked out,
    /// or zero when attempts already in flight use up what is left of its limit.
    pub(crate) fn admit(
        self: &Arc<Self>,
        client: ClientKey,
        now: Instant,
    ) -> Result<Attempt, Duration> {
        let mut windows = self.lock();
        let window = self.window_for(&mut windows, client, now);
        if window.failures >= self.max_failures {
            let elapsed = now.saturating_duration_since(window.started);
            return Err(self.window.saturating_sub(elapsed));
        }
        if window.failures.saturating_add(window.pending) >= self.max_failures {
            return Err(Duration::ZERO);
        }
        window.pending = window.pending.saturating_add(1);
        Ok(Attempt {
            limiter: Arc::clone(self),
            client,
            settled: false,
        })
    }

    /// The live window for `client`, created (making room first) if it is not tracked
    /// and restarted if it has expired.
    fn window_for<'a>(
        &self,
        windows: &'a mut HashMap<ClientKey, Window>,
        client: ClientKey,
        now: Instant,
    ) -> &'a mut Window {
        if !windows.contains_key(&client) && windows.len() >= self.capacity {
            windows.retain(|_, w| w.pending > 0 || !w.expired(now, self.window));
            if windows.len() >= self.capacity {
                let oldest = windows
                    .iter()
                    .min_by_key(|(_, w)| (w.pending > 0, w.started))
                    .map(|(key, _)| *key);
                if let Some(key) = oldest {
                    windows.remove(&key);
                }
            }
        }
        let window = windows.entry(client).or_insert(Window {
            started: now,
            failures: 0,
            pending: 0,
        });
        if window.expired(now, self.window) {
            window.started = now;
            window.failures = 0;
        }
        window
    }

    /// Settles one attempt of `client`: refunds it, or counts it as a failure at `now`.
    /// Returns `true` when this failure reached the limit.
    fn settle(&self, client: ClientKey, failed_at: Option<Instant>) -> bool {
        let mut windows = self.lock();
        let Some(now) = failed_at else {
            if let Some(window) = windows.get_mut(&client) {
                window.pending = window.pending.saturating_sub(1);
                if window.pending == 0 && window.failures == 0 {
                    windows.remove(&client);
                }
            }
            return false;
        };
        // The window may have been evicted while the attempt was pending; the failure
        // then starts a new one, as it would have before the reservation existed.
        let window = self.window_for(&mut windows, client, now);
        window.pending = window.pending.saturating_sub(1);
        if window.failures == 0 {
            window.started = now;
        }
        window.failures = window.failures.saturating_add(1);
        window.failures == self.max_failures
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.lock().len()
    }
}

/// One admitted authentication attempt. [`Attempt::fail`] counts it as a failure;
/// dropping it in any other way (success, storage error, panic) refunds it.
#[derive(Debug)]
pub(crate) struct Attempt {
    limiter: Arc<FailureLimiter>,
    client: ClientKey,
    settled: bool,
}

impl Attempt {
    /// The client this attempt is counted against.
    pub(crate) fn client(&self) -> ClientKey {
        self.client
    }

    /// Counts the attempt as a failure at `now`. Returns `true` when this failure
    /// reached the limit, so the client is now locked out.
    pub(crate) fn fail(mut self, now: Instant) -> bool {
        self.settled = true;
        self.limiter.settle(self.client, Some(now))
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.settled {
            self.limiter.settle(self.client, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;

    use super::*;

    const A: ClientKey = ClientKey::V4(Ipv4Addr::new(192, 0, 2, 1));
    const B: ClientKey = ClientKey::V4(Ipv4Addr::new(192, 0, 2, 2));

    fn limiter(max_failures: u32, capacity: usize) -> Arc<FailureLimiter> {
        Arc::new(FailureLimiter::new(
            max_failures,
            Duration::from_secs(60),
            capacity,
        ))
    }

    /// One attempt that fails; returns whether it reached the limit.
    fn fail(limiter: &Arc<FailureLimiter>, client: ClientKey, now: Instant) -> bool {
        limiter.admit(client, now).expect("admitted").fail(now)
    }

    /// How long `client` must wait at `now`, if it is refused. An admitted probe is
    /// refunded at once.
    fn blocked(limiter: &Arc<FailureLimiter>, client: ClientKey, now: Instant) -> Option<Duration> {
        limiter.admit(client, now).err()
    }

    #[test]
    fn blocks_after_the_limit_until_the_window_ends() {
        let limiter = limiter(3, 16);
        let t0 = Instant::now();
        assert!(!fail(&limiter, A, t0));
        assert!(!fail(&limiter, A, t0));
        assert_eq!(blocked(&limiter, A, t0), None);
        assert!(fail(&limiter, A, t0 + Duration::from_secs(10)));
        assert_eq!(
            blocked(&limiter, A, t0 + Duration::from_secs(10)),
            Some(Duration::from_secs(50))
        );
        // Other clients are unaffected.
        assert_eq!(blocked(&limiter, B, t0), None);
        // The lockout ends with the window, and counting starts again.
        assert_eq!(blocked(&limiter, A, t0 + Duration::from_secs(60)), None);
        assert!(!fail(&limiter, A, t0 + Duration::from_secs(61)));
        assert_eq!(blocked(&limiter, A, t0 + Duration::from_secs(61)), None);
    }

    #[test]
    fn pending_attempts_count_toward_the_limit() {
        let limiter = limiter(3, 16);
        let t0 = Instant::now();
        let first = limiter.admit(A, t0).expect("first");
        let second = limiter.admit(A, t0).expect("second");
        let third = limiter.admit(A, t0).expect("third");
        // Three lookups in flight use up the limit: a fourth is refused, not queued.
        assert_eq!(blocked(&limiter, A, t0), Some(Duration::ZERO));
        // Other clients are unaffected.
        assert_eq!(blocked(&limiter, B, t0), None);
        // A success refunds its attempt, so one more may start.
        drop(first);
        let fourth = limiter.admit(A, t0).expect("refunded slot");
        // Failures keep theirs: three failures lock the client out for the window.
        assert!(!second.fail(t0));
        assert!(!third.fail(t0));
        assert!(fourth.fail(t0 + Duration::from_secs(5)));
        assert_eq!(
            blocked(&limiter, A, t0 + Duration::from_secs(5)),
            Some(Duration::from_secs(55))
        );
    }

    #[test]
    fn successful_clients_are_not_tracked() {
        let limiter = limiter(3, 16);
        let t0 = Instant::now();
        drop(limiter.admit(A, t0).expect("admitted"));
        assert_eq!(limiter.tracked(), 0);
        // A client with failures keeps its window after a later success.
        assert!(!fail(&limiter, B, t0));
        drop(limiter.admit(B, t0).expect("admitted"));
        assert_eq!(limiter.tracked(), 1);
    }

    #[test]
    fn the_window_starts_at_the_first_failure_not_the_first_attempt() {
        let limiter = limiter(1, 16);
        let t0 = Instant::now();
        let attempt = limiter.admit(A, t0).expect("admitted");
        assert!(attempt.fail(t0 + Duration::from_secs(30)));
        assert_eq!(
            blocked(&limiter, A, t0 + Duration::from_secs(80)),
            Some(Duration::from_secs(10))
        );
    }

    #[test]
    fn memory_stays_bounded_under_many_clients() {
        let limiter = limiter(2, 8);
        let t0 = Instant::now();
        for i in 0..1000_u32 {
            let key = ClientKey::V4(Ipv4Addr::from(i));
            fail(&limiter, key, t0 + Duration::from_millis(u64::from(i)));
            assert!(limiter.tracked() <= 8);
        }
        // The most recent clients are still tracked; the earliest were evicted.
        let last = ClientKey::V4(Ipv4Addr::from(999));
        let first = ClientKey::V4(Ipv4Addr::from(0));
        let now = t0 + Duration::from_secs(1);
        fail(&limiter, last, now);
        assert!(blocked(&limiter, last, now).is_some());
        assert_eq!(blocked(&limiter, first, now), None);
    }

    #[test]
    fn eviction_spares_clients_with_pending_attempts() {
        let limiter = limiter(2, 2);
        let t0 = Instant::now();
        let pending = limiter.admit(A, t0).expect("admitted");
        assert!(!fail(&limiter, B, t0 + Duration::from_secs(1)));
        // The table is full; C pushes out B (no pending attempts), not the older A.
        let c = ClientKey::V4(Ipv4Addr::new(192, 0, 2, 3));
        assert!(!fail(&limiter, c, t0 + Duration::from_secs(2)));
        assert_eq!(limiter.tracked(), 2);
        assert!(!pending.fail(t0 + Duration::from_secs(3)));
        let a = limiter
            .admit(A, t0 + Duration::from_secs(3))
            .expect("admitted");
        // A's earlier failure was kept: one more reaches the limit of two.
        assert!(a.fail(t0 + Duration::from_secs(3)));
    }

    #[test]
    fn expired_windows_are_dropped_before_live_ones() {
        let limiter = limiter(2, 2);
        let t0 = Instant::now();
        fail(&limiter, A, t0);
        fail(&limiter, A, t0);
        let late = t0 + Duration::from_secs(100);
        fail(&limiter, B, late);
        fail(&limiter, B, late);
        // A's window expired, so C replaces it and B stays locked out.
        let c = ClientKey::V4(Ipv4Addr::new(192, 0, 2, 3));
        fail(&limiter, c, late);
        assert!(blocked(&limiter, B, late).is_some());
        assert_eq!(limiter.tracked(), 2);
    }

    #[test]
    fn ipv6_clients_are_grouped_by_slash_64() {
        let one: Ipv6Addr = "2001:db8:1:2::1".parse().expect("ip");
        let two: Ipv6Addr = "2001:db8:1:2:ffff::9".parse().expect("ip");
        let other: Ipv6Addr = "2001:db8:1:3::1".parse().expect("ip");
        let mapped: Ipv6Addr = "::ffff:192.0.2.1".parse().expect("ip");
        assert_eq!(
            ClientKey::from_ip(one.into()),
            ClientKey::from_ip(two.into())
        );
        assert_ne!(
            ClientKey::from_ip(one.into()),
            ClientKey::from_ip(other.into())
        );
        assert_eq!(ClientKey::from_ip(mapped.into()), A);
        assert_eq!(
            ClientKey::from_ip(one.into()).to_string(),
            "2001:db8:1:2::/64"
        );
        assert_eq!(A.to_string(), "192.0.2.1");
    }
}
