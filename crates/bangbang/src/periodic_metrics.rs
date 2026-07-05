use std::time::{Duration, Instant};

pub(crate) const FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub(crate) struct PeriodicMetricsScheduler {
    period: Duration,
    next_deadline: Instant,
}

impl PeriodicMetricsScheduler {
    pub(crate) fn new(now: Instant) -> Self {
        Self::with_period(now, FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL)
    }

    #[cfg(test)]
    pub(crate) fn due_now(now: Instant) -> Self {
        Self {
            period: FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL,
            next_deadline: now,
        }
    }

    fn with_period(now: Instant, period: Duration) -> Self {
        let period = if period.is_zero() {
            Duration::from_millis(1)
        } else {
            period
        };
        let next_deadline = now.checked_add(period).unwrap_or(now);

        Self {
            period,
            next_deadline,
        }
    }

    pub(crate) fn poll_timeout_ms(&self, now: Instant) -> i32 {
        if now >= self.next_deadline {
            return 0;
        }

        let remaining = self.next_deadline.duration_since(now);
        let millis = remaining.as_millis();
        if millis == 0 {
            1
        } else if millis > i32::MAX as u128 {
            i32::MAX
        } else {
            millis as i32
        }
    }

    pub(crate) fn schedule_next(&mut self, now: Instant) {
        self.next_deadline = now.checked_add(self.period).unwrap_or(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_uses_firecracker_interval_by_default() {
        let now = Instant::now();
        let scheduler = PeriodicMetricsScheduler::new(now);

        assert_eq!(
            scheduler.poll_timeout_ms(now),
            FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL.as_millis() as i32
        );
    }

    #[test]
    fn scheduler_reports_due_deadline_without_waiting() {
        let now = Instant::now();
        let scheduler = PeriodicMetricsScheduler::due_now(now);

        assert_eq!(scheduler.poll_timeout_ms(now), 0);
    }

    #[test]
    fn scheduler_rounds_submillisecond_timeout_up() {
        let now = Instant::now();
        let scheduler = PeriodicMetricsScheduler::with_period(now, Duration::from_nanos(1));

        assert_eq!(scheduler.poll_timeout_ms(now), 1);
    }

    #[test]
    fn scheduler_schedules_next_deadline_after_flush() {
        let now = Instant::now();
        let mut scheduler = PeriodicMetricsScheduler::due_now(now);
        scheduler.schedule_next(now);

        assert_eq!(
            scheduler.poll_timeout_ms(now),
            FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL.as_millis() as i32
        );
    }
}
