//! Retry policy for idempotent HTTP GETs (release downloads, latest-version queries).
//!
//! Only transient failures are retried: connection errors, timeouts, HTTP 429 and 5xx. A 403
//! from GitHub is the rate limit; retrying it only spends more of the budget.

use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct Retry {
    pub attempts: u32,
    pub base: Duration,
}

impl Retry {
    /// Downloads during installs: worth waiting a few seconds for.
    pub const DOWNLOAD: Retry = Retry {
        attempts: 3,
        base: Duration::from_secs(2),
    };
    /// Latest-version queries: many run per refresh, so give up quickly.
    pub const QUERY: Retry = Retry {
        attempts: 2,
        base: Duration::from_secs(1),
    };

    /// Delay before the next try after attempt `n` (1-based) failed transiently, or `None` when
    /// no attempt is left or the server asks to wait longer than is worth it.
    pub fn delay(&self, n: u32, retry_after: Option<Duration>) -> Option<Duration> {
        if n >= self.attempts {
            return None;
        }
        match retry_after {
            Some(d) if d > MAX_RETRY_AFTER => None,
            Some(d) => Some(d),
            None => Some(self.base * 2u32.pow(n - 1)),
        }
    }
}

const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

pub fn transient_status(code: u16) -> bool {
    code == 429 || (500..600).contains(&code)
}

pub fn transient_error(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request() || e.is_body()
}

/// `Retry-After` in seconds (the HTTP-date form is not used by the services envira queries).
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_stops() {
        let r = Retry::DOWNLOAD;
        assert_eq!(r.delay(1, None), Some(Duration::from_secs(2)));
        assert_eq!(r.delay(2, None), Some(Duration::from_secs(4)));
        assert_eq!(r.delay(3, None), None);
        assert_eq!(Retry::QUERY.delay(2, None), None);
    }

    #[test]
    fn retry_after_is_honoured_up_to_a_cap() {
        let r = Retry::DOWNLOAD;
        assert_eq!(
            r.delay(1, Some(Duration::from_secs(5))),
            Some(Duration::from_secs(5))
        );
        assert_eq!(r.delay(1, Some(Duration::from_secs(120))), None);
    }

    #[test]
    fn only_transient_statuses_retry() {
        for code in [429, 500, 502, 503, 504] {
            assert!(transient_status(code), "{code}");
        }
        for code in [200, 301, 401, 403, 404, 422] {
            assert!(!transient_status(code), "{code}");
        }
    }

    #[test]
    fn parses_retry_after_seconds() {
        let mut h = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after(&h), None);
        h.insert(reqwest::header::RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(retry_after(&h), Some(Duration::from_secs(7)));
        h.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&h), None);
    }
}
