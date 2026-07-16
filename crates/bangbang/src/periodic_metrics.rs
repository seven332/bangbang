use std::time::{Duration, Instant};

pub(crate) const FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub(crate) struct PeriodicMetricsScheduler {
    deadline: Option<PeriodicDeadline>,
}

impl PeriodicMetricsScheduler {
    pub(crate) const fn new() -> Self {
        Self { deadline: None }
    }

    #[cfg(test)]
    pub(crate) fn due_now(now: Instant) -> Self {
        Self {
            deadline: Some(PeriodicDeadline::due_now(
                now,
                FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL,
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_period(now: Instant, period: Duration) -> Self {
        Self {
            deadline: Some(PeriodicDeadline::new(now, period)),
        }
    }

    pub(crate) fn poll_timeout_ms(
        &mut self,
        now: Instant,
        session_epoch: Option<Instant>,
    ) -> Option<i32> {
        self.sync_session_epoch(session_epoch);
        self.deadline
            .as_ref()
            .map(|deadline| deadline.poll_timeout_ms(now))
    }

    pub(crate) fn is_due(&mut self, now: Instant, session_epoch: Option<Instant>) -> bool {
        self.sync_session_epoch(session_epoch);
        self.deadline
            .as_ref()
            .is_some_and(|deadline| deadline.is_due(now))
    }

    pub(crate) fn schedule_next(&mut self, now: Instant, session_epoch: Option<Instant>) {
        self.sync_session_epoch(session_epoch);
        if let Some(deadline) = self.deadline.as_mut() {
            deadline.schedule_next(now);
        }
    }

    fn sync_session_epoch(&mut self, session_epoch: Option<Instant>) {
        match (self.deadline.as_mut(), session_epoch) {
            (Some(_), Some(_)) | (None, None) => {}
            (Some(_), None) => self.deadline = None,
            (None, Some(epoch)) => {
                self.deadline = Some(PeriodicDeadline::new(
                    epoch,
                    FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL,
                ));
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PeriodicBalloonStatisticsScheduler {
    deadline: Option<PeriodicDeadline>,
}

impl PeriodicBalloonStatisticsScheduler {
    pub(crate) fn new(now: Instant, interval: Option<Duration>) -> Self {
        let mut scheduler = Self { deadline: None };
        scheduler.sync_interval(now, interval);
        scheduler
    }

    #[cfg(test)]
    pub(crate) fn due_now(now: Instant, interval: Duration) -> Self {
        Self {
            deadline: Some(PeriodicDeadline::due_now(now, interval)),
        }
    }

    pub(crate) fn poll_timeout_ms(
        &mut self,
        now: Instant,
        interval: Option<Duration>,
    ) -> Option<i32> {
        self.sync_interval(now, interval);
        self.deadline
            .as_ref()
            .map(|deadline| deadline.poll_timeout_ms(now))
    }

    pub(crate) fn is_due(&mut self, now: Instant, interval: Option<Duration>) -> bool {
        self.sync_interval(now, interval);
        self.deadline
            .as_ref()
            .is_some_and(|deadline| deadline.is_due(now))
    }

    pub(crate) fn schedule_next(&mut self, now: Instant, interval: Option<Duration>) {
        self.sync_interval(now, interval);
        if let Some(deadline) = self.deadline.as_mut() {
            deadline.schedule_next(now);
        }
    }

    fn sync_interval(&mut self, now: Instant, interval: Option<Duration>) {
        let Some(interval) = interval.filter(|interval| !interval.is_zero()) else {
            self.deadline = None;
            return;
        };

        match self.deadline.as_mut() {
            Some(deadline) if deadline.period() == interval => {}
            Some(deadline) => *deadline = PeriodicDeadline::new(now, interval),
            None => self.deadline = Some(PeriodicDeadline::new(now, interval)),
        }
    }
}

#[derive(Debug, Clone)]
struct PeriodicDeadline {
    period: Duration,
    next_deadline: Instant,
}

impl PeriodicDeadline {
    fn new(now: Instant, period: Duration) -> Self {
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

    #[cfg(test)]
    fn due_now(now: Instant, period: Duration) -> Self {
        let mut deadline = Self::new(now, period);
        deadline.next_deadline = now;
        deadline
    }

    const fn period(&self) -> Duration {
        self.period
    }

    fn poll_timeout_ms(&self, now: Instant) -> i32 {
        if now >= self.next_deadline {
            return 0;
        }

        let remaining = self.next_deadline.duration_since(now);
        let millis =
            remaining.as_millis() + u128::from(!remaining.subsec_nanos().is_multiple_of(1_000_000));
        if millis == 0 {
            1
        } else if millis > i32::MAX as u128 {
            i32::MAX
        } else {
            millis as i32
        }
    }

    fn is_due(&self, now: Instant) -> bool {
        now >= self.next_deadline
    }

    fn schedule_next(&mut self, now: Instant) {
        self.next_deadline = now.checked_add(self.period).unwrap_or(now);
    }
}

pub(crate) fn min_poll_timeout_ms(first: Option<i32>, second: Option<i32>) -> Option<i32> {
    match (first, second) {
        (Some(first), Some(second)) => Some(first.min(second)),
        (Some(first), None) => Some(first),
        (None, Some(second)) => Some(second),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_scheduler_is_dormant_without_session_epoch() {
        let now = Instant::now();
        let mut scheduler = PeriodicMetricsScheduler::new();

        assert_eq!(scheduler.poll_timeout_ms(now, None), None);
        assert!(!scheduler.is_due(now, None));
    }

    #[test]
    fn metrics_scheduler_uses_firecracker_interval_from_session_epoch() {
        let now = Instant::now();
        let mut scheduler = PeriodicMetricsScheduler::new();

        assert_eq!(
            scheduler.poll_timeout_ms(now, Some(now)),
            Some(FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL.as_millis() as i32)
        );
    }

    #[test]
    fn metrics_scheduler_preserves_elapsed_time_before_epoch_observation() {
        let epoch = Instant::now();
        let observed_at = epoch + Duration::from_secs(17);
        let mut scheduler = PeriodicMetricsScheduler::new();

        assert_eq!(
            scheduler.poll_timeout_ms(observed_at, Some(epoch)),
            Some(43_000)
        );
    }

    #[test]
    fn metrics_scheduler_reports_late_epoch_observation_due_immediately() {
        let epoch = Instant::now();
        let observed_at = epoch + FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL;
        let mut scheduler = PeriodicMetricsScheduler::new();

        assert_eq!(scheduler.poll_timeout_ms(observed_at, Some(epoch)), Some(0));
        assert!(scheduler.is_due(observed_at, Some(epoch)));
    }

    #[test]
    fn metrics_scheduler_does_not_reset_deadline_on_later_observation() {
        let epoch = Instant::now();
        let mut scheduler = PeriodicMetricsScheduler::new();
        assert_eq!(
            scheduler.poll_timeout_ms(epoch + Duration::from_secs(5), Some(epoch)),
            Some(55_000)
        );

        assert_eq!(
            scheduler.poll_timeout_ms(epoch + Duration::from_secs(11), Some(epoch)),
            Some(49_000)
        );
    }

    #[test]
    fn scheduler_reports_due_deadline_without_waiting() {
        let now = Instant::now();
        let mut scheduler = PeriodicMetricsScheduler::due_now(now);

        assert_eq!(scheduler.poll_timeout_ms(now, Some(now)), Some(0));
        assert!(scheduler.is_due(now, Some(now)));
    }

    #[test]
    fn scheduler_rounds_submillisecond_timeout_up() {
        let now = Instant::now();
        let mut scheduler = PeriodicMetricsScheduler::with_period(now, Duration::from_nanos(1));

        assert_eq!(scheduler.poll_timeout_ms(now, Some(now)), Some(1));
    }

    #[test]
    fn scheduler_rounds_partial_millisecond_timeout_up() {
        let now = Instant::now();
        let mut scheduler = PeriodicMetricsScheduler::with_period(
            now,
            Duration::from_millis(1) + Duration::from_nanos(1),
        );

        assert_eq!(scheduler.poll_timeout_ms(now, Some(now)), Some(2));
    }

    #[test]
    fn scheduler_schedules_next_deadline_after_flush() {
        let now = Instant::now();
        let mut scheduler = PeriodicMetricsScheduler::due_now(now);
        scheduler.schedule_next(now, Some(now));

        assert_eq!(
            scheduler.poll_timeout_ms(now, Some(now)),
            Some(FIRECRACKER_PERIODIC_METRICS_FLUSH_INTERVAL.as_millis() as i32)
        );
    }

    #[test]
    fn balloon_scheduler_is_disabled_without_interval() {
        let now = Instant::now();
        let mut scheduler = PeriodicBalloonStatisticsScheduler::new(now, None);

        assert_eq!(scheduler.poll_timeout_ms(now, None), None);
        assert!(!scheduler.is_due(now, None));
    }

    #[test]
    fn balloon_scheduler_uses_configured_interval() {
        let now = Instant::now();
        let mut scheduler =
            PeriodicBalloonStatisticsScheduler::new(now, Some(Duration::from_secs(3)));

        assert_eq!(
            scheduler.poll_timeout_ms(now, Some(Duration::from_secs(3))),
            Some(3000)
        );
        assert!(!scheduler.is_due(now, Some(Duration::from_secs(3))));
        assert_eq!(
            scheduler.poll_timeout_ms(
                now.checked_add(Duration::from_secs(3))
                    .expect("deadline should advance"),
                Some(Duration::from_secs(3))
            ),
            Some(0)
        );
    }

    #[test]
    fn balloon_scheduler_resets_when_interval_changes() {
        let now = Instant::now();
        let mut scheduler =
            PeriodicBalloonStatisticsScheduler::new(now, Some(Duration::from_secs(60)));
        let later = now
            .checked_add(Duration::from_secs(10))
            .expect("later instant should build");

        assert_eq!(
            scheduler.poll_timeout_ms(later, Some(Duration::from_secs(30))),
            Some(30_000)
        );
    }

    #[test]
    fn balloon_scheduler_disables_after_interval_is_removed() {
        let now = Instant::now();
        let mut scheduler =
            PeriodicBalloonStatisticsScheduler::new(now, Some(Duration::from_secs(1)));

        assert_eq!(scheduler.poll_timeout_ms(now, None), None);
        assert!(!scheduler.is_due(now, None));
    }

    #[test]
    fn min_poll_timeout_prefers_shorter_enabled_timeout() {
        assert_eq!(min_poll_timeout_ms(Some(60_000), Some(1_000)), Some(1_000));
        assert_eq!(min_poll_timeout_ms(Some(60_000), None), Some(60_000));
        assert_eq!(min_poll_timeout_ms(None, Some(1_000)), Some(1_000));
        assert_eq!(min_poll_timeout_ms(None, None), None);
    }
}
