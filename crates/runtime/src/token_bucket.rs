use std::fmt;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenBucket {
    size: u64,
    refill_time_nanos: u64,
    budget: u64,
    one_time_burst: u64,
    last_update: Instant,
    elapsed_refill_credit_nanos: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenBucketReduction {
    Allowed,
    Throttled { retry_after: Duration },
}

impl TokenBucketReduction {
    pub(crate) const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }

    pub(crate) const fn retry_after(self) -> Option<Duration> {
        match self {
            Self::Allowed => None,
            Self::Throttled { retry_after } => Some(retry_after),
        }
    }
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
            elapsed_refill_credit_nanos: 0,
        })
    }

    pub(crate) fn reduce(&mut self, tokens: u64) -> bool {
        self.reduce_at(tokens, Instant::now())
    }

    pub(crate) fn reduce_at(&mut self, tokens: u64, now: Instant) -> bool {
        self.reduce_with_retry_at(tokens, now).is_allowed()
    }

    pub(crate) fn reduce_with_retry_at(
        &mut self,
        tokens: u64,
        now: Instant,
    ) -> TokenBucketReduction {
        if tokens == 0 {
            return TokenBucketReduction::Allowed;
        }
        if self.one_time_burst >= tokens {
            self.one_time_burst -= tokens;
            self.last_update = now;
            self.elapsed_refill_credit_nanos = 0;
            return TokenBucketReduction::Allowed;
        }

        let tokens = tokens.saturating_sub(self.one_time_burst);
        self.one_time_burst = 0;
        self.replenish_at(now);

        if tokens > self.size {
            self.budget = 0;
            return TokenBucketReduction::Throttled {
                retry_after: self.retry_after_for_tokens(self.size, now),
            };
        }
        if tokens > self.budget {
            return TokenBucketReduction::Throttled {
                retry_after: self.retry_after_for_tokens(tokens, now),
            };
        }

        self.budget -= tokens;
        TokenBucketReduction::Allowed
    }

    pub(crate) fn reduce_allow_overconsumption_with_retry_at(
        &mut self,
        tokens: u64,
        now: Instant,
    ) -> TokenBucketReduction {
        if tokens == 0 || tokens <= self.size {
            return self.reduce_with_retry_at(tokens, now);
        }
        if self.one_time_burst >= tokens {
            self.one_time_burst -= tokens;
            self.last_update = now;
            self.elapsed_refill_credit_nanos = 0;
            return TokenBucketReduction::Allowed;
        }

        let tokens = tokens.saturating_sub(self.one_time_burst);
        self.one_time_burst = 0;
        self.replenish_at(now);

        if tokens <= self.size {
            return self.reduce_with_retry_at(tokens, now);
        }
        if self.budget < self.size {
            return TokenBucketReduction::Throttled {
                retry_after: self.retry_after_for_tokens(self.size, now),
            };
        }

        self.budget = 0;
        TokenBucketReduction::Allowed
    }

    pub(crate) const fn snapshot(&self) -> TokenBucketSnapshot {
        TokenBucketSnapshot {
            budget: self.budget,
            one_time_burst: self.one_time_burst,
            last_update: self.last_update,
            elapsed_refill_credit_nanos: self.elapsed_refill_credit_nanos,
        }
    }

    pub(crate) fn restore(&mut self, snapshot: TokenBucketSnapshot) {
        self.budget = snapshot.budget;
        self.one_time_burst = snapshot.one_time_burst;
        self.last_update = snapshot.last_update;
        self.elapsed_refill_credit_nanos = snapshot.elapsed_refill_credit_nanos;
    }

    pub(crate) fn persisted_state_at(
        &self,
        config: TokenBucketConfig,
        now: Instant,
    ) -> Result<PersistedTokenBucketState, PersistedTokenBucketStateError> {
        let refill_time_nanos = config
            .refill_time()
            .checked_mul(NANOS_PER_MILLISECOND)
            .ok_or(PersistedTokenBucketStateError::DisabledConfiguration)?;
        if !config.is_enabled() {
            return Err(PersistedTokenBucketStateError::DisabledConfiguration);
        }
        if self.size != config.size() || self.refill_time_nanos != refill_time_nanos {
            return Err(PersistedTokenBucketStateError::ConfigurationMismatch);
        }
        if self.budget > self.size {
            return Err(PersistedTokenBucketStateError::BudgetOutOfBounds);
        }
        if self.one_time_burst > config.one_time_burst().unwrap_or(0) {
            return Err(PersistedTokenBucketStateError::BurstOutOfBounds);
        }

        let physical_age = now
            .checked_duration_since(self.last_update)
            .ok_or(PersistedTokenBucketStateError::CaptureTimeBeforeLastUpdate)?;
        let age_nanos = physical_age
            .as_nanos()
            .checked_add(u128::from(self.elapsed_refill_credit_nanos))
            .and_then(|age| u64::try_from(age).ok())
            .ok_or(PersistedTokenBucketStateError::AgeOutOfBounds)?;

        Ok(PersistedTokenBucketState::new(
            config,
            self.budget,
            self.one_time_burst,
            age_nanos,
        ))
    }

    pub(crate) fn from_persisted_state_at(
        state: PersistedTokenBucketState,
        now: Instant,
    ) -> Result<Self, PersistedTokenBucketStateError> {
        let config = state.config();
        let mut bucket = Self::new_at(config, now)
            .ok_or(PersistedTokenBucketStateError::DisabledConfiguration)?;
        if state.budget() > config.size() {
            return Err(PersistedTokenBucketStateError::BudgetOutOfBounds);
        }
        if state.one_time_burst() > config.one_time_burst().unwrap_or(0) {
            return Err(PersistedTokenBucketStateError::BurstOutOfBounds);
        }
        bucket.budget = state.budget();
        bucket.one_time_burst = state.one_time_burst();
        bucket.last_update = now;
        bucket.elapsed_refill_credit_nanos = state.age_nanos();
        Ok(bucket)
    }

    fn replenish_at(&mut self, now: Instant) {
        let Some(physical_elapsed) = now.checked_duration_since(self.last_update) else {
            return;
        };

        let elapsed_nanos = physical_elapsed
            .as_nanos()
            .saturating_add(u128::from(self.elapsed_refill_credit_nanos));
        let adjusted_nanos = match token_bucket_refill(
            self.size,
            self.budget,
            self.refill_time_nanos,
            elapsed_nanos,
        ) {
            TokenBucketRefill::Unchanged => return,
            TokenBucketRefill::Full => {
                self.budget = self.size;
                self.last_update = now;
                self.elapsed_refill_credit_nanos = 0;
                return;
            }
            TokenBucketRefill::Partial {
                budget,
                adjusted_nanos,
            } => {
                self.budget = budget;
                adjusted_nanos
            }
        };
        let consumed_credit = self.elapsed_refill_credit_nanos.min(adjusted_nanos);
        self.elapsed_refill_credit_nanos -= consumed_credit;
        let consumed_physical = adjusted_nanos - consumed_credit;
        if consumed_physical != 0 {
            self.last_update = self
                .last_update
                .checked_add(Duration::from_nanos(consumed_physical))
                .unwrap_or(now);
        }
    }

    fn retry_after_for_tokens(&self, tokens: u64, now: Instant) -> Duration {
        let target_budget = tokens.min(self.size);
        let token_deficit = target_budget.saturating_sub(self.budget);
        if token_deficit == 0 {
            return Duration::ZERO;
        }

        let nanos_since_update = now
            .checked_duration_since(self.last_update)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
            .saturating_add(u128::from(self.elapsed_refill_credit_nanos));
        let nanos_until_budget = u128::from(token_deficit)
            .saturating_mul(u128::from(self.refill_time_nanos))
            .div_ceil(u128::from(self.size));
        let retry_after_nanos = nanos_until_budget.saturating_sub(nanos_since_update);
        duration_from_nanos_saturating(retry_after_nanos)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenBucketRefill {
    Unchanged,
    Full,
    Partial { budget: u64, adjusted_nanos: u64 },
}

fn token_bucket_refill(
    size: u64,
    budget: u64,
    refill_time_nanos: u64,
    elapsed_nanos: u128,
) -> TokenBucketRefill {
    if size == 0 || refill_time_nanos == 0 {
        return TokenBucketRefill::Unchanged;
    }

    let refill_time_nanos_u128 = u128::from(refill_time_nanos);
    if elapsed_nanos >= refill_time_nanos_u128 {
        return TokenBucketRefill::Full;
    }

    let elapsed_nanos_u64 = match u64::try_from(elapsed_nanos) {
        Ok(value) => value,
        Err(_) => return TokenBucketRefill::Full,
    };
    // Keep common refill calculations at native width while retaining the
    // original wide arithmetic for products that do not fit in u64.
    let tokens = match elapsed_nanos_u64.checked_mul(size) {
        Some(scaled_elapsed) => scaled_elapsed / refill_time_nanos,
        None => {
            let wide_tokens = elapsed_nanos * u128::from(size) / refill_time_nanos_u128;
            match u64::try_from(wide_tokens) {
                Ok(value) => value,
                Err(_) => size,
            }
        }
    };
    if tokens == 0 {
        return TokenBucketRefill::Unchanged;
    }

    let budget = budget.saturating_add(tokens).min(size);
    let adjusted_nanos = match tokens.checked_mul(refill_time_nanos) {
        Some(scaled_tokens) => scaled_tokens.div_ceil(size),
        None => {
            let wide_adjusted = u128::from(tokens)
                .saturating_mul(refill_time_nanos_u128)
                .div_ceil(u128::from(size));
            match u64::try_from(wide_adjusted) {
                Ok(value) => value,
                Err(_) => refill_time_nanos,
            }
        }
    };

    TokenBucketRefill::Partial {
        budget,
        adjusted_nanos,
    }
}

fn duration_from_nanos_saturating(nanos: u128) -> Duration {
    match u64::try_from(nanos) {
        Ok(value) => Duration::from_nanos(value),
        Err(_) => Duration::from_nanos(u64::MAX),
    }
}

#[cfg(kani)]
#[allow(dead_code)]
mod verification {
    use super::*;

    #[kani::proof]
    fn verify_token_bucket_refill_accounting() {
        let size_input: u16 = kani::any();
        let budget_input: u16 = kani::any();
        let refill_time_millis_input: u16 = kani::any();
        let elapsed_millis_input: u16 = kani::any();

        kani::assume((1..=4_096).contains(&size_input));
        kani::assume(budget_input <= size_input);
        kani::assume((1..=1_000).contains(&refill_time_millis_input));
        kani::assume(elapsed_millis_input <= refill_time_millis_input);
        let refill_time_nanos_input = u32::from(refill_time_millis_input) * 1_000_000;
        let elapsed_nanos_input = u32::from(elapsed_millis_input) * 1_000_000;

        // These widths exactly cover the bounded domain above while avoiding
        // unconstrained high bits before the values enter the production types.
        let size = u64::from(size_input);
        let budget = u64::from(budget_input);
        let refill_time_nanos = u64::from(refill_time_nanos_input);
        let elapsed_nanos = u128::from(elapsed_nanos_input);

        let invariant_holds =
            match token_bucket_refill(size, budget, refill_time_nanos, elapsed_nanos) {
                TokenBucketRefill::Unchanged => {
                    let tokens = u64::from(elapsed_nanos_input) * size / refill_time_nanos;
                    tokens == 0
                }
                TokenBucketRefill::Full => elapsed_nanos == u128::from(refill_time_nanos),
                TokenBucketRefill::Partial {
                    budget: new_budget,
                    adjusted_nanos,
                } => {
                    new_budget >= budget
                        && new_budget <= size
                        && adjusted_nanos != 0
                        && u128::from(adjusted_nanos) <= elapsed_nanos
                        && adjusted_nanos < refill_time_nanos
                }
            };
        assert!(invariant_holds);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenBucketSnapshot {
    budget: u64,
    one_time_burst: u64,
    last_update: Instant,
    elapsed_refill_credit_nanos: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct PersistedTokenBucketState {
    config: TokenBucketConfig,
    budget: u64,
    one_time_burst: u64,
    age_nanos: u64,
}

impl PersistedTokenBucketState {
    pub(crate) const fn new(
        config: TokenBucketConfig,
        budget: u64,
        one_time_burst: u64,
        age_nanos: u64,
    ) -> Self {
        Self {
            config,
            budget,
            one_time_burst,
            age_nanos,
        }
    }

    pub(crate) const fn config(self) -> TokenBucketConfig {
        self.config
    }

    pub(crate) const fn budget(self) -> u64 {
        self.budget
    }

    pub(crate) const fn one_time_burst(self) -> u64 {
        self.one_time_burst
    }

    pub(crate) const fn age_nanos(self) -> u64 {
        self.age_nanos
    }
}

impl fmt::Debug for PersistedTokenBucketState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersistedTokenBucketState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PersistedTokenBucketStateError {
    DisabledConfiguration,
    ConfigurationMismatch,
    BudgetOutOfBounds,
    BurstOutOfBounds,
    CaptureTimeBeforeLastUpdate,
    AgeOutOfBounds,
}

impl fmt::Display for PersistedTokenBucketStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DisabledConfiguration => {
                f.write_str("persisted token bucket configuration is disabled")
            }
            Self::ConfigurationMismatch => {
                f.write_str("persisted token bucket configuration does not match runtime state")
            }
            Self::BudgetOutOfBounds => {
                f.write_str("persisted token bucket budget is out of bounds")
            }
            Self::BurstOutOfBounds => f.write_str("persisted token bucket burst is out of bounds"),
            Self::CaptureTimeBeforeLastUpdate => {
                f.write_str("persisted token bucket capture time precedes its last update")
            }
            Self::AgeOutOfBounds => f.write_str("persisted token bucket age is out of bounds"),
        }
    }
}

impl std::error::Error for PersistedTokenBucketStateError {}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        PersistedTokenBucketState, PersistedTokenBucketStateError, TokenBucket, TokenBucketConfig,
        TokenBucketReduction, TokenBucketRefill, token_bucket_refill,
    };

    #[test]
    fn refill_calculation_classifies_exact_boundaries() {
        assert_eq!(
            token_bucket_refill(0, 0, 100, 100),
            TokenBucketRefill::Unchanged
        );
        assert_eq!(
            token_bucket_refill(4, 0, 0, 100),
            TokenBucketRefill::Unchanged
        );
        assert_eq!(
            token_bucket_refill(4, 1, 100, 24),
            TokenBucketRefill::Unchanged
        );
        assert_eq!(
            token_bucket_refill(4, 1, 100, 25),
            TokenBucketRefill::Partial {
                budget: 2,
                adjusted_nanos: 25,
            }
        );
        assert_eq!(
            token_bucket_refill(4, 3, 100, 99),
            TokenBucketRefill::Partial {
                budget: 4,
                adjusted_nanos: 75,
            }
        );
        assert_eq!(token_bucket_refill(4, 0, 100, 100), TokenBucketRefill::Full);
    }

    #[test]
    fn refill_calculation_preserves_wide_arithmetic() {
        assert_eq!(
            token_bucket_refill(u64::MAX, 0, u64::MAX, u128::from(u64::MAX - 1)),
            TokenBucketRefill::Partial {
                budget: u64::MAX - 1,
                adjusted_nanos: u64::MAX - 1,
            }
        );
        assert_eq!(
            token_bucket_refill(u64::MAX, u64::MAX, u64::MAX, u128::from(u64::MAX - 1)),
            TokenBucketRefill::Partial {
                budget: u64::MAX,
                adjusted_nanos: u64::MAX - 1,
            }
        );
    }

    #[test]
    fn consumes_burst_budget_and_refills_by_time() {
        let now = Instant::now();
        let mut bucket = TokenBucket::new_at(TokenBucketConfig::new(2, Some(1), 100), now)
            .expect("bucket should be enabled");

        assert!(bucket.reduce_at(1, now));
        assert!(bucket.reduce_at(1, now));
        assert!(bucket.reduce_at(1, now));
        assert_eq!(
            bucket.reduce_with_retry_at(1, now),
            TokenBucketReduction::Throttled {
                retry_after: Duration::from_millis(50),
            }
        );
        assert!(bucket.reduce_at(1, now + Duration::from_millis(50)));
        assert_eq!(
            bucket.reduce_with_retry_at(1, now + Duration::from_millis(50)),
            TokenBucketReduction::Throttled {
                retry_after: Duration::from_millis(50),
            }
        );
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

        assert!(
            bucket
                .reduce_allow_overconsumption_with_retry_at(8, now)
                .is_allowed()
        );
        assert_eq!(
            bucket.reduce_allow_overconsumption_with_retry_at(8, now),
            TokenBucketReduction::Throttled {
                retry_after: Duration::from_millis(100),
            }
        );
        assert_eq!(
            bucket.reduce_allow_overconsumption_with_retry_at(8, now + Duration::from_millis(50)),
            TokenBucketReduction::Throttled {
                retry_after: Duration::from_millis(50),
            }
        );
        assert!(
            bucket
                .reduce_allow_overconsumption_with_retry_at(8, now + Duration::from_millis(100))
                .is_allowed()
        );
    }

    #[test]
    fn persisted_state_freezes_bucket_age_across_restore_downtime() {
        let capture_origin = Instant::now();
        let config = TokenBucketConfig::new(4, Some(2), 100);
        let mut source = TokenBucket::new_at(config, capture_origin)
            .expect("persisted source bucket should be enabled");
        assert!(source.reduce_at(2, capture_origin));
        assert!(source.reduce_at(3, capture_origin));
        let capture_now = capture_origin + Duration::from_millis(25);

        let state = source
            .persisted_state_at(config, capture_now)
            .expect("persisted state should capture");
        assert_eq!(state.budget(), 1);
        assert_eq!(state.one_time_burst(), 0);
        assert_eq!(state.age_nanos(), 25_000_000);

        let restore_now = capture_origin + Duration::from_secs(10);
        let mut restored = TokenBucket::from_persisted_state_at(state, restore_now)
            .expect("persisted state should restore");
        let recaptured = restored
            .persisted_state_at(config, restore_now)
            .expect("restored state should recapture");
        assert_eq!(recaptured, state);
        assert!(restored.reduce_at(2, restore_now));
        assert_eq!(
            restored.reduce_with_retry_at(1, restore_now),
            TokenBucketReduction::Throttled {
                retry_after: Duration::from_millis(25),
            }
        );
    }

    #[test]
    fn logical_restore_credit_matches_a_representable_physical_anchor() {
        let now = Instant::now();
        let age = Duration::from_millis(25);
        let config = TokenBucketConfig::new(4, None, 100);
        let mut physical = TokenBucket::new_at(
            config,
            now.checked_sub(age)
                .expect("short physical anchor should be representable"),
        )
        .expect("physical bucket should build");
        physical.budget = 1;
        let state = PersistedTokenBucketState::new(
            config,
            1,
            0,
            u64::try_from(age.as_nanos()).expect("test age should fit"),
        );
        let mut logical = TokenBucket::from_persisted_state_at(state, now)
            .expect("logical bucket should restore");

        assert_eq!(
            logical.reduce_with_retry_at(2, now),
            physical.reduce_with_retry_at(2, now)
        );
        assert_eq!(
            logical
                .persisted_state_at(config, now)
                .expect("logical bucket should recapture"),
            physical
                .persisted_state_at(config, now)
                .expect("physical bucket should recapture")
        );
    }

    #[test]
    fn logical_restore_credit_does_not_depend_on_destination_uptime() {
        let now = Instant::now();
        let config = TokenBucketConfig::new(4, None, 100);
        let state = PersistedTokenBucketState::new(config, 0, 0, u64::MAX);
        let restored = TokenBucket::from_persisted_state_at(state, now)
            .expect("maximum logical age should not subtract from destination time");

        assert_eq!(
            restored
                .persisted_state_at(config, now)
                .expect("restore-time recapture should retain exact age"),
            state
        );
        assert_eq!(
            restored
                .persisted_state_at(config, now + Duration::from_nanos(1))
                .expect_err("age beyond the persisted representation should fail"),
            PersistedTokenBucketStateError::AgeOutOfBounds
        );
    }

    #[test]
    fn bucket_snapshot_restores_logical_credit_after_time_reset() {
        let now = Instant::now();
        let config = TokenBucketConfig::new(4, Some(2), 100);
        let state = PersistedTokenBucketState::new(config, 1, 2, 25_000_000);
        let mut restored = TokenBucket::from_persisted_state_at(state, now)
            .expect("logical bucket should restore");
        let snapshot = restored.snapshot();

        assert!(restored.reduce_at(1, now));
        assert_ne!(
            restored
                .persisted_state_at(config, now)
                .expect("mutated bucket should capture"),
            state,
            "one-time burst consumption resets elapsed progress"
        );

        restored.restore(snapshot);
        assert_eq!(
            restored
                .persisted_state_at(config, now)
                .expect("rolled-back bucket should capture"),
            state
        );
    }

    #[test]
    fn persisted_state_rejects_invalid_budget_burst_and_time() {
        let now = Instant::now();
        let config = TokenBucketConfig::new(4, Some(2), 100);

        for (state, expected) in [
            (
                PersistedTokenBucketState::new(config, 5, 0, 0),
                PersistedTokenBucketStateError::BudgetOutOfBounds,
            ),
            (
                PersistedTokenBucketState::new(config, 4, 3, 0),
                PersistedTokenBucketStateError::BurstOutOfBounds,
            ),
        ] {
            assert_eq!(
                TokenBucket::from_persisted_state_at(state, now)
                    .expect_err("invalid persisted bucket should fail"),
                expected
            );
        }

        let future_bucket = TokenBucket::new_at(config, now + Duration::from_nanos(1))
            .expect("future bucket should build");
        assert_eq!(
            future_bucket
                .persisted_state_at(config, now)
                .expect_err("capture before last update should fail"),
            PersistedTokenBucketStateError::CaptureTimeBeforeLastUpdate
        );
    }
}
