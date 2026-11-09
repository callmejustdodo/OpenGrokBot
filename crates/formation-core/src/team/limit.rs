//! Rate limit for bad join attempts on the creator's Mac.
//!
//! Every join with an unknown, used, or expired code counts against the caller's
//! Tailscale login and against all callers together, over a sliding window. Once
//! either is full, joins get `rate_limited` before the code is looked at, so a
//! correct guess during the lockout does not get in. Counts live in memory and
//! start over when formationd restarts.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, SystemTime};

use formation_proto::common::{ApiError, ErrorCode, Timestamp};

use crate::ports::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinLimit {
    /// Bad attempts one Tailscale login may make within `window`.
    pub per_caller: usize,
    /// Bad attempts all callers together may make within `window`.
    pub total: usize,
    pub window: Duration,
}

impl Default for JoinLimit {
    fn default() -> Self {
        Self {
            per_caller: 5,
            total: 20,
            window: Duration::from_secs(10 * 60),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct JoinLimiter {
    limit: JoinLimit,
    by_caller: HashMap<String, VecDeque<Timestamp>>,
    all: VecDeque<Timestamp>,
}

impl JoinLimiter {
    pub(crate) fn new(limit: JoinLimit) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }

    /// `rate_limited` while the caller, or everyone, has used up the window.
    pub(crate) fn check(&mut self, caller: &str, now: Timestamp) -> Result<()> {
        self.prune(now);
        let mine = self.by_caller.get(caller).map_or(0, VecDeque::len);
        if mine >= self.limit.per_caller || self.all.len() >= self.limit.total {
            return Err(ApiError::new(
                ErrorCode::RateLimited,
                "too many invalid invitation codes; try again later",
            ));
        }
        Ok(())
    }

    pub(crate) fn record_failure(&mut self, caller: &str, now: Timestamp) {
        self.by_caller
            .entry(caller.to_owned())
            .or_default()
            .push_back(now);
        self.all.push_back(now);
    }

    fn prune(&mut self, now: Timestamp) {
        let window = self.limit.window;
        let fresh = |at: &Timestamp| elapsed(*at, now) < window;
        let drop_stale = |times: &mut VecDeque<Timestamp>| {
            while times.front().is_some_and(|at| !fresh(at)) {
                times.pop_front();
            }
        };
        drop_stale(&mut self.all);
        self.by_caller.retain(|_, times| {
            drop_stale(times);
            !times.is_empty()
        });
    }
}

/// Time from `from` to `to`; zero when `to` is earlier (the clock went back).
pub(crate) fn elapsed(from: Timestamp, to: Timestamp) -> Duration {
    SystemTime::from(to)
        .duration_since(SystemTime::from(from))
        .unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minutes: u64) -> Timestamp {
        let t0: Timestamp = "2026-11-09T09:00:00Z".parse().unwrap();
        Timestamp::from(SystemTime::from(t0) + Duration::from_secs(minutes * 60))
    }

    fn limiter(per_caller: usize, total: usize) -> JoinLimiter {
        JoinLimiter::new(JoinLimit {
            per_caller,
            total,
            window: Duration::from_secs(10 * 60),
        })
    }

    #[test]
    fn a_caller_is_limited_after_its_bad_attempts_and_released_after_the_window() {
        let mut limiter = limiter(3, 100);
        for minute in 0..3 {
            limiter.check("eve@example.com", at(minute)).unwrap();
            limiter.record_failure("eve@example.com", at(minute));
        }
        let error = limiter.check("eve@example.com", at(3)).unwrap_err();
        assert_eq!(error.code, ErrorCode::RateLimited);
        // Others are not affected by one caller's failures.
        limiter.check("minsu@example.com", at(3)).unwrap();
        // The first failure leaves the window at minute 10.
        assert!(limiter.check("eve@example.com", at(9)).is_err());
        limiter.check("eve@example.com", at(10)).unwrap();
    }

    #[test]
    fn all_callers_together_are_limited() {
        let mut limiter = limiter(100, 4);
        for (i, caller) in ["a", "b", "c", "d"].into_iter().enumerate() {
            limiter.record_failure(caller, at(i as u64));
        }
        assert!(limiter.check("e", at(4)).is_err());
        limiter.check("e", at(10)).unwrap();
        assert_eq!(limiter.by_caller.len(), 3, "stale callers are forgotten");
    }

    #[test]
    fn a_clock_going_back_does_not_release_early() {
        let mut limiter = limiter(1, 100);
        limiter.record_failure("eve", at(5));
        assert!(limiter.check("eve", at(1)).is_err());
    }
}
