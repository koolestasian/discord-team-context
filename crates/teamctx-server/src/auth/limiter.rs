//! In-memory limit on failed authentications per client address.
//!
//! Numbers (see [`FailureLimiter::default`]):
//! - **10 failures per 60-second window** per client. The window starts at the client's
//!   first failure. From the 10th failure until the window ends, every request from that
//!   client gets `429 Too Many Requests` with `Retry-After`, valid token or not, so a
//!   guesser makes at most 10 attempts a minute per address.
//! - **At most 4,096 clients tracked.** When the table is full, expired windows are
//!   dropped first, then the window that started earliest. Memory stays bounded (a few
//!   hundred KiB) whatever an attacker sends; the cost is that an attacker with more than
//!   4,096 addresses can push older windows out, which still leaves 256-bit tokens out of
//!   reach.
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
    sync::{Mutex, PoisonError},
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
    started: Instant,
    failures: u32,
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

    /// If `client` is locked out at `now`, how long until it may try again.
    pub(crate) fn blocked(&self, client: ClientKey, now: Instant) -> Option<Duration> {
        let windows = self.lock();
        let window = windows.get(&client)?;
        let remaining = self
            .window
            .checked_sub(now.saturating_duration_since(window.started))?;
        (window.failures >= self.max_failures && !remaining.is_zero()).then_some(remaining)
    }

    /// Counts one failure. Returns `true` when this failure reached the limit.
    pub(crate) fn record_failure(&self, client: ClientKey, now: Instant) -> bool {
        let mut windows = self.lock();
        if !windows.contains_key(&client) && windows.len() >= self.capacity {
            windows.retain(|_, w| now.saturating_duration_since(w.started) < self.window);
            if windows.len() >= self.capacity {
                let oldest = windows
                    .iter()
                    .min_by_key(|(_, w)| w.started)
                    .map(|(key, _)| *key);
                if let Some(key) = oldest {
                    windows.remove(&key);
                }
            }
        }
        let window = windows.entry(client).or_insert(Window {
            started: now,
            failures: 0,
        });
        if now.saturating_duration_since(window.started) >= self.window {
            *window = Window {
                started: now,
                failures: 0,
            };
        }
        window.failures = window.failures.saturating_add(1);
        window.failures == self.max_failures
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;

    use super::*;

    const A: ClientKey = ClientKey::V4(Ipv4Addr::new(192, 0, 2, 1));
    const B: ClientKey = ClientKey::V4(Ipv4Addr::new(192, 0, 2, 2));

    #[test]
    fn blocks_after_the_limit_until_the_window_ends() {
        let limiter = FailureLimiter::new(3, Duration::from_secs(60), 16);
        let t0 = Instant::now();
        assert!(!limiter.record_failure(A, t0));
        assert!(!limiter.record_failure(A, t0));
        assert_eq!(limiter.blocked(A, t0), None);
        assert!(limiter.record_failure(A, t0 + Duration::from_secs(10)));
        assert_eq!(
            limiter.blocked(A, t0 + Duration::from_secs(10)),
            Some(Duration::from_secs(50))
        );
        // Other clients are unaffected.
        assert_eq!(limiter.blocked(B, t0), None);
        // The lockout ends with the window, and counting starts again.
        assert_eq!(limiter.blocked(A, t0 + Duration::from_secs(60)), None);
        assert!(!limiter.record_failure(A, t0 + Duration::from_secs(61)));
        assert_eq!(limiter.blocked(A, t0 + Duration::from_secs(61)), None);
    }

    #[test]
    fn memory_stays_bounded_under_many_clients() {
        let limiter = FailureLimiter::new(2, Duration::from_secs(60), 8);
        let t0 = Instant::now();
        for i in 0..1000_u32 {
            let key = ClientKey::V4(Ipv4Addr::from(i));
            limiter.record_failure(key, t0 + Duration::from_millis(u64::from(i)));
            assert!(limiter.tracked() <= 8);
        }
        // The most recent clients are still tracked; the earliest were evicted.
        let last = ClientKey::V4(Ipv4Addr::from(999));
        let first = ClientKey::V4(Ipv4Addr::from(0));
        let now = t0 + Duration::from_secs(1);
        limiter.record_failure(last, now);
        assert!(limiter.blocked(last, now).is_some());
        assert_eq!(limiter.blocked(first, now), None);
    }

    #[test]
    fn expired_windows_are_dropped_before_live_ones() {
        let limiter = FailureLimiter::new(2, Duration::from_secs(60), 2);
        let t0 = Instant::now();
        limiter.record_failure(A, t0);
        limiter.record_failure(A, t0);
        let late = t0 + Duration::from_secs(100);
        limiter.record_failure(B, late);
        limiter.record_failure(B, late);
        // A's window expired, so C replaces it and B stays locked out.
        let c = ClientKey::V4(Ipv4Addr::new(192, 0, 2, 3));
        limiter.record_failure(c, late);
        assert!(limiter.blocked(B, late).is_some());
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
