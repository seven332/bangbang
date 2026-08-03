use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const LOG_RATE_LIMIT_BURST: u64 = 10;
const LOG_RATE_LIMIT_REFILL_MS: u64 = 5_000;
const LOG_RATE_LIMIT_PERIOD_MS: u64 = LOG_RATE_LIMIT_REFILL_MS / LOG_RATE_LIMIT_BURST;
const LOG_RATE_LIMIT_CAS_ATTEMPTS: usize = 16;

pub(super) trait LogRateLimiterClock: fmt::Debug + Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug)]
struct SystemLogRateLimiterClock {
    epoch: Instant,
}

impl Default for SystemLogRateLimiterClock {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }
}

impl LogRateLimiterClock for SystemLogRateLimiterClock {
    fn now_ms(&self) -> u64 {
        let elapsed = self.epoch.elapsed();
        elapsed
            .as_secs()
            .saturating_mul(1_000)
            .saturating_add(u64::from(elapsed.subsec_millis()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoggerRateLimitIdentity {
    BootTimer,
    ApiRequest,
}

impl LoggerRateLimitIdentity {
    const fn index(self) -> usize {
        match self {
            Self::BootTimer => 0,
            Self::ApiRequest => 1,
        }
    }
}

const LOGGER_RATE_LIMIT_IDENTITY_COUNT: usize = LoggerRateLimitIdentity::ApiRequest.index() + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LogRateLimitDecision {
    Admitted { suppressed: u64 },
    Denied,
}

#[derive(Debug, Default)]
struct AtomicGcraState {
    theoretical_arrival_time_ms: AtomicU64,
    suppressed: AtomicU64,
    #[cfg(test)]
    forced_cas_failures: AtomicU64,
}

impl AtomicGcraState {
    fn record_suppressed(&self) {
        let mut current = self.suppressed.load(Ordering::Acquire);
        while current != u64::MAX {
            match self.suppressed.compare_exchange_weak(
                current,
                current.saturating_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    #[cfg(test)]
    fn should_force_cas_failure(&self) -> bool {
        self.forced_cas_failures
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
    }
}

#[derive(Debug)]
struct LoggerRateLimitersInner {
    clock: Arc<dyn LogRateLimiterClock>,
    states: [AtomicGcraState; LOGGER_RATE_LIMIT_IDENTITY_COUNT],
}

#[derive(Debug, Clone)]
pub(super) struct LoggerRateLimiters {
    inner: Arc<LoggerRateLimitersInner>,
}

impl Default for LoggerRateLimiters {
    fn default() -> Self {
        Self::with_clock(Arc::new(SystemLogRateLimiterClock::default()))
    }
}

impl LoggerRateLimiters {
    pub(super) fn with_clock(clock: Arc<dyn LogRateLimiterClock>) -> Self {
        Self {
            inner: Arc::new(LoggerRateLimitersInner {
                clock,
                states: std::array::from_fn(|_| AtomicGcraState::default()),
            }),
        }
    }

    pub(super) fn check(&self, identity: LoggerRateLimitIdentity) -> LogRateLimitDecision {
        let Some(state) = self.inner.states.get(identity.index()) else {
            return LogRateLimitDecision::Denied;
        };
        let now_ms = self.inner.clock.now_ms();
        let mut current = state.theoretical_arrival_time_ms.load(Ordering::Acquire);

        for _ in 0..LOG_RATE_LIMIT_CAS_ATTEMPTS {
            let next = current.max(now_ms).saturating_add(LOG_RATE_LIMIT_PERIOD_MS);
            if next.saturating_sub(now_ms) > LOG_RATE_LIMIT_REFILL_MS {
                state.record_suppressed();
                return LogRateLimitDecision::Denied;
            }

            #[cfg(test)]
            if state.should_force_cas_failure() {
                current = state.theoretical_arrival_time_ms.load(Ordering::Acquire);
                continue;
            }

            match state.theoretical_arrival_time_ms.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return LogRateLimitDecision::Admitted {
                        suppressed: state.suppressed.swap(0, Ordering::AcqRel),
                    };
                }
                Err(actual) => current = actual,
            }
        }

        state.record_suppressed();
        LogRateLimitDecision::Denied
    }

    #[cfg(test)]
    pub(super) fn force_cas_failures(&self, identity: LoggerRateLimitIdentity, failures: u64) {
        if let Some(state) = self.inner.states.get(identity.index()) {
            state.forced_cas_failures.store(failures, Ordering::Release);
        }
    }

    #[cfg(test)]
    pub(super) fn set_suppressed_for_test(
        &self,
        identity: LoggerRateLimitIdentity,
        suppressed: u64,
    ) {
        if let Some(state) = self.inner.states.get(identity.index()) {
            state.suppressed.store(suppressed, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::{
        LOG_RATE_LIMIT_CAS_ATTEMPTS, LogRateLimitDecision, LogRateLimiterClock,
        LoggerRateLimitIdentity, LoggerRateLimiters,
    };

    #[derive(Debug, Default)]
    struct TestClock(AtomicU64);

    impl TestClock {
        fn set(&self, now_ms: u64) {
            self.0.store(now_ms, Ordering::Release);
        }
    }

    impl LogRateLimiterClock for TestClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::Acquire)
        }
    }

    #[test]
    fn admits_ten_then_refills_at_exact_boundary() {
        let clock = Arc::new(TestClock::default());
        let limiter = LoggerRateLimiters::with_clock(clock.clone());

        for _ in 0..10 {
            assert_eq!(
                limiter.check(LoggerRateLimitIdentity::BootTimer),
                LogRateLimitDecision::Admitted { suppressed: 0 }
            );
        }
        assert_eq!(
            limiter.check(LoggerRateLimitIdentity::BootTimer),
            LogRateLimitDecision::Denied
        );

        clock.set(499);
        assert_eq!(
            limiter.check(LoggerRateLimitIdentity::BootTimer),
            LogRateLimitDecision::Denied
        );
        clock.set(500);
        assert_eq!(
            limiter.check(LoggerRateLimitIdentity::BootTimer),
            LogRateLimitDecision::Admitted { suppressed: 2 }
        );
    }

    #[test]
    fn backward_time_does_not_refill_budget() {
        let clock = Arc::new(TestClock::default());
        clock.set(5_000);
        let limiter = LoggerRateLimiters::with_clock(clock.clone());
        for _ in 0..10 {
            assert!(matches!(
                limiter.check(LoggerRateLimitIdentity::BootTimer),
                LogRateLimitDecision::Admitted { .. }
            ));
        }

        clock.set(4_000);
        assert_eq!(
            limiter.check(LoggerRateLimitIdentity::BootTimer),
            LogRateLimitDecision::Denied
        );
    }

    #[test]
    fn retry_exhaustion_denies_without_wait_and_recovers() {
        let clock = Arc::new(TestClock::default());
        let limiter = LoggerRateLimiters::with_clock(clock.clone());
        limiter.force_cas_failures(
            LoggerRateLimitIdentity::BootTimer,
            u64::try_from(LOG_RATE_LIMIT_CAS_ATTEMPTS).unwrap_or(u64::MAX),
        );

        assert_eq!(
            limiter.check(LoggerRateLimitIdentity::BootTimer),
            LogRateLimitDecision::Denied
        );
        assert_eq!(
            limiter.check(LoggerRateLimitIdentity::BootTimer),
            LogRateLimitDecision::Admitted { suppressed: 1 }
        );
    }

    #[test]
    fn suppression_saturates() {
        let clock = Arc::new(TestClock::default());
        let limiter = LoggerRateLimiters::with_clock(clock.clone());
        for _ in 0..10 {
            assert!(matches!(
                limiter.check(LoggerRateLimitIdentity::BootTimer),
                LogRateLimitDecision::Admitted { .. }
            ));
        }
        limiter.set_suppressed_for_test(LoggerRateLimitIdentity::BootTimer, u64::MAX);
        assert_eq!(
            limiter.check(LoggerRateLimitIdentity::BootTimer),
            LogRateLimitDecision::Denied
        );
        clock.set(500);
        assert_eq!(
            limiter.check(LoggerRateLimitIdentity::BootTimer),
            LogRateLimitDecision::Admitted {
                suppressed: u64::MAX
            }
        );
    }

    #[test]
    fn identities_and_controllers_have_independent_budgets() {
        let clock = Arc::new(TestClock::default());
        let first = LoggerRateLimiters::with_clock(clock.clone());
        let second = LoggerRateLimiters::with_clock(clock);
        for _ in 0..10 {
            assert!(matches!(
                first.check(LoggerRateLimitIdentity::BootTimer),
                LogRateLimitDecision::Admitted { .. }
            ));
        }

        assert_eq!(
            first.check(LoggerRateLimitIdentity::BootTimer),
            LogRateLimitDecision::Denied
        );
        assert_eq!(
            first.check(LoggerRateLimitIdentity::ApiRequest),
            LogRateLimitDecision::Admitted { suppressed: 0 }
        );
        assert_eq!(
            second.check(LoggerRateLimitIdentity::BootTimer),
            LogRateLimitDecision::Admitted { suppressed: 0 }
        );
    }

    #[test]
    fn concurrent_checks_conserve_every_denial() {
        const CHECKS: usize = 32;

        let clock = Arc::new(TestClock::default());
        let limiter = LoggerRateLimiters::with_clock(clock.clone());
        let start = Arc::new(Barrier::new(CHECKS));
        let mut workers = Vec::with_capacity(CHECKS);
        for _ in 0..CHECKS {
            let limiter = limiter.clone();
            let start = start.clone();
            workers.push(thread::spawn(move || {
                start.wait();
                limiter.check(LoggerRateLimitIdentity::BootTimer)
            }));
        }

        let mut admitted = 0;
        let mut denied = 0;
        let mut recovered = 0_u64;
        for worker in workers {
            match worker.join().expect("limiter worker should finish") {
                LogRateLimitDecision::Admitted { suppressed } => {
                    admitted += 1;
                    recovered = recovered.saturating_add(suppressed);
                }
                LogRateLimitDecision::Denied => denied += 1,
            }
        }
        assert!(admitted <= 10);
        assert_eq!(admitted + denied, CHECKS);

        clock.set(500);
        let LogRateLimitDecision::Admitted { suppressed } =
            limiter.check(LoggerRateLimitIdentity::BootTimer)
        else {
            panic!("one exact refill should admit after concurrent checks");
        };
        assert_eq!(
            recovered.saturating_add(suppressed),
            u64::try_from(denied).unwrap_or(u64::MAX)
        );
    }
}
