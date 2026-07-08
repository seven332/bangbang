use std::time::{Duration, Instant};

const NANOS_PER_MILLISECOND: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenBucketConfig {
    size: u64,
    one_time_burst: Option<u64>,
    refill_time: u64,
}

impl TokenBucketConfig {
    pub(crate) const fn new(size: u64, one_time_burst: Option<u64>, refill_time: u64) -> Self {
        Self {
            size,
            one_time_burst,
            refill_time,
        }
    }

    pub(crate) const fn size(self) -> u64 {
        self.size
    }

    pub(crate) const fn one_time_burst(self) -> Option<u64> {
        self.one_time_burst
    }

    pub(crate) const fn refill_time(self) -> u64 {
        self.refill_time
    }

    pub(crate) const fn is_enabled(self) -> bool {
        if self.size == 0 {
            return false;
        }

        match self.refill_time.checked_mul(NANOS_PER_MILLISECOND) {
            Some(refill_time_nanos) => refill_time_nanos != 0,
            None => false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TokenBucket {
    size: u64,
    refill_time_nanos: u64,
    budget: u64,
    one_time_burst: u64,
    last_update: Instant,
}

impl TokenBucket {
    pub(crate) fn new(config: TokenBucketConfig) -> Option<Self> {
        Self::new_at(config, Instant::now())
    }

    pub(crate) fn new_at(config: TokenBucketConfig, now: Instant) -> Option<Self> {
        let refill_time_nanos = config.refill_time().checked_mul(NANOS_PER_MILLISECOND)?;
        if !config.is_enabled() {
            return None;
        }

        Some(Self {
            size: config.size(),
            refill_time_nanos,
            budget: config.size(),
            one_time_burst: config.one_time_burst().unwrap_or(0),
            last_update: now,
        })
    }

    pub(crate) fn reduce(&mut self, tokens: u64) -> bool {
        self.reduce_at(tokens, Instant::now())
    }

    pub(crate) fn reduce_at(&mut self, tokens: u64, now: Instant) -> bool {
        if tokens == 0 {
            return true;
        }
        if self.one_time_burst >= tokens {
            self.one_time_burst -= tokens;
            self.last_update = now;
            return true;
        }

        let tokens = tokens.saturating_sub(self.one_time_burst);
        self.one_time_burst = 0;
        self.replenish_at(now);

        if tokens > self.size {
            self.budget = 0;
            return false;
        }
        if tokens > self.budget {
            return false;
        }

        self.budget -= tokens;
        true
    }

    pub(crate) fn reduce_allow_overconsumption_at(&mut self, tokens: u64, now: Instant) -> bool {
        if tokens == 0 || tokens <= self.size {
            return self.reduce_at(tokens, now);
        }
        if self.one_time_burst >= tokens {
            self.one_time_burst -= tokens;
            self.last_update = now;
            return true;
        }

        let tokens = tokens.saturating_sub(self.one_time_burst);
        self.one_time_burst = 0;
        self.replenish_at(now);

        if tokens <= self.size {
            return self.reduce_at(tokens, now);
        }
        if self.budget < self.size {
            return false;
        }

        self.budget = 0;
        true
    }

    pub(crate) const fn snapshot(&self) -> TokenBucketSnapshot {
        TokenBucketSnapshot {
            budget: self.budget,
            one_time_burst: self.one_time_burst,
            last_update: self.last_update,
        }
    }

    pub(crate) fn restore(&mut self, snapshot: TokenBucketSnapshot) {
        self.budget = snapshot.budget;
        self.one_time_burst = snapshot.one_time_burst;
        self.last_update = snapshot.last_update;
    }

    fn replenish_at(&mut self, now: Instant) {
        if now <= self.last_update {
            return;
        }

        let elapsed = now.duration_since(self.last_update);
        let elapsed_nanos = elapsed.as_nanos();
        let refill_time_nanos = u128::from(self.refill_time_nanos);
        if elapsed_nanos >= refill_time_nanos {
            self.budget = self.size;
            self.last_update = now;
            return;
        }

        let tokens = elapsed_nanos * u128::from(self.size) / refill_time_nanos;
        if tokens == 0 {
            return;
        }

        let budget = u128::from(self.budget)
            .saturating_add(tokens)
            .min(u128::from(self.size));
        self.budget = match u64::try_from(budget) {
            Ok(value) => value,
            Err(_) => self.size,
        };

        let adjusted_nanos = tokens
            .saturating_mul(refill_time_nanos)
            .div_ceil(u128::from(self.size));
        let adjusted_nanos = match u64::try_from(adjusted_nanos) {
            Ok(value) => value,
            Err(_) => self.refill_time_nanos,
        };
        self.last_update += Duration::from_nanos(adjusted_nanos);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenBucketSnapshot {
    budget: u64,
    one_time_burst: u64,
    last_update: Instant,
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{TokenBucket, TokenBucketConfig};

    #[test]
    fn consumes_burst_budget_and_refills_by_time() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new_at(TokenBucketConfig::new(2, Some(1), 100), now)
            .expect("bucket should be enabled");

        assert!(bucket.reduce_at(1, now));
        assert!(bucket.reduce_at(1, now));
        assert!(bucket.reduce_at(1, now));
        assert!(!bucket.reduce_at(1, now));
        assert!(bucket.reduce_at(1, now + Duration::from_millis(50)));
        assert!(!bucket.reduce_at(1, now + Duration::from_millis(50)));
        assert!(bucket.reduce_at(1, now + Duration::from_millis(100)));
    }

    #[test]
    fn disables_zero_or_overflowing_configs() {
        let now = Instant::now();

        for config in [
            TokenBucketConfig::new(0, None, 1),
            TokenBucketConfig::new(1, None, 0),
            TokenBucketConfig::new(1, None, u64::MAX),
        ] {
            assert!(TokenBucket::new_at(config, now).is_none());
        }
    }

    #[test]
    fn restores_consumed_state_from_snapshot() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new_at(TokenBucketConfig::new(2, Some(1), 100), now)
            .expect("bucket should be enabled");
        let snapshot = bucket.snapshot();

        assert!(bucket.reduce_at(1, now));
        assert!(bucket.reduce_at(1, now));
        assert!(bucket.reduce_at(1, now));
        assert!(!bucket.reduce_at(1, now));

        bucket.restore(snapshot);

        assert!(bucket.reduce_at(1, now));
        assert!(bucket.reduce_at(1, now));
        assert!(bucket.reduce_at(1, now));
        assert!(!bucket.reduce_at(1, now));
    }

    #[test]
    fn overconsumption_requires_full_budget_for_oversized_requests() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new_at(TokenBucketConfig::new(4, None, 100), now)
            .expect("bucket should be enabled");

        assert!(bucket.reduce_allow_overconsumption_at(8, now));
        assert!(!bucket.reduce_allow_overconsumption_at(8, now));
        assert!(!bucket.reduce_allow_overconsumption_at(8, now + Duration::from_millis(50)));
        assert!(bucket.reduce_allow_overconsumption_at(8, now + Duration::from_millis(100)));
    }
}
