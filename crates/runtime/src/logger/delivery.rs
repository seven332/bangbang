use std::fmt;
use std::io::Write;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread;
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

use super::SharedLoggerMetrics;
use super::event::LogBatch;

const LOGGER_DELIVERY_QUEUE_CAPACITY: usize = 256;
const LOGGER_DELIVERY_TIMEOUT: Duration = Duration::from_secs(1);

const RECEIPT_PENDING: u8 = 0;
const RECEIPT_WRITING: u8 = 1;
const RECEIPT_TIMED_OUT: u8 = 2;
const RECEIPT_COMPLETE_BASE: u8 = 3;

const REPLACEMENT_PENDING: u8 = 0;
const REPLACEMENT_COMMITTED: u8 = 1;
const REPLACEMENT_CANCELLED: u8 = 2;

pub(super) struct PreparedLoggerWriter(Box<dyn Write + Send>);

impl fmt::Debug for PreparedLoggerWriter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedLoggerWriter")
            .finish_non_exhaustive()
    }
}

impl PreparedLoggerWriter {
    pub(super) fn new(writer: impl Write + Send + 'static) -> Self {
        Self(Box::new(writer))
    }

    fn write(&mut self, bytes: &[u8]) -> bool {
        match self.0.write(bytes) {
            Ok(written) if written == bytes.len() => self.0.flush().is_ok(),
            Ok(_) | Err(_) => false,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct LoggerDeliveryConfig {
    queue_capacity: usize,
    receipt_timeout: Duration,
    replacement_timeout: Duration,
    #[cfg(test)]
    fail_spawn: bool,
    #[cfg(test)]
    worker_observer: Option<Arc<WorkerObserver>>,
}

impl Default for LoggerDeliveryConfig {
    fn default() -> Self {
        Self {
            queue_capacity: LOGGER_DELIVERY_QUEUE_CAPACITY,
            receipt_timeout: LOGGER_DELIVERY_TIMEOUT,
            replacement_timeout: LOGGER_DELIVERY_TIMEOUT,
            #[cfg(test)]
            fail_spawn: false,
            #[cfg(test)]
            worker_observer: None,
        }
    }
}

impl LoggerDeliveryConfig {
    #[cfg(test)]
    pub(super) const fn for_test(queue_capacity: usize, timeout: Duration) -> Self {
        Self {
            queue_capacity,
            receipt_timeout: timeout,
            replacement_timeout: timeout,
            fail_spawn: false,
            worker_observer: None,
        }
    }

    #[cfg(test)]
    pub(super) const fn with_failed_spawn(mut self) -> Self {
        self.fail_spawn = true;
        self
    }

    #[cfg(test)]
    pub(super) fn with_worker_observer(mut self, observer: Arc<WorkerObserver>) -> Self {
        self.worker_observer = Some(observer);
        self
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(super) struct WorkerObserver {
    started: AtomicU64,
    active: AtomicU64,
}

#[cfg(test)]
impl WorkerObserver {
    pub(super) fn started(&self) -> u64 {
        self.started.load(Ordering::Acquire)
    }

    pub(super) fn active(&self) -> u64 {
        self.active.load(Ordering::Acquire)
    }
}

#[cfg(test)]
struct WorkerGuard(Arc<WorkerObserver>);

#[cfg(test)]
impl WorkerGuard {
    fn start(observer: Arc<WorkerObserver>) -> Self {
        observer.started.fetch_add(1, Ordering::AcqRel);
        observer.active.fetch_add(1, Ordering::AcqRel);
        Self(observer)
    }
}

#[cfg(test)]
impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
pub(super) struct LoggerProducer {
    sender: SyncSender<WorkerMessage>,
    metrics: SharedLoggerMetrics,
    receipt_timeout: Duration,
}

impl fmt::Debug for LoggerProducer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoggerProducer")
            .field("receipt_timeout", &self.receipt_timeout)
            .finish_non_exhaustive()
    }
}

impl LoggerProducer {
    pub(super) fn deliver_guest(&self, batch: LogBatch) -> bool {
        let record_count = batch.len();
        match self.sender.try_send(WorkerMessage::batch(batch, None)) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.metrics.record_missed_logs(record_count);
                false
            }
        }
    }

    pub(super) fn deliver_host(&self, batch: LogBatch) -> bool {
        let record_count = batch.len();
        let (worker_receipt, waiter) = DeliveryReceipt::new();
        match self
            .sender
            .try_send(WorkerMessage::batch(batch, Some(worker_receipt)))
        {
            Ok(()) => waiter.wait(self.receipt_timeout, record_count, &self.metrics),
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.metrics.record_missed_logs(record_count);
                false
            }
        }
    }

    #[cfg(test)]
    pub(super) fn wait_for_idle_for_test(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut message = WorkerMessage::barrier(sender);
        loop {
            match self.sender.try_send(message) {
                Ok(()) => {
                    return receiver
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .is_ok();
                }
                Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                    message = returned;
                    thread::yield_now();
                }
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => return false,
            }
        }
    }
}

pub(super) struct LoggerDelivery {
    producer: LoggerProducer,
    replacement_timeout: Duration,
}

impl fmt::Debug for LoggerDelivery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoggerDelivery")
            .field("producer", &self.producer)
            .field("replacement_timeout", &self.replacement_timeout)
            .finish_non_exhaustive()
    }
}

impl LoggerDelivery {
    pub(super) fn spawn(
        writer: PreparedLoggerWriter,
        metrics: SharedLoggerMetrics,
        config: LoggerDeliveryConfig,
    ) -> Result<Self, std::io::ErrorKind> {
        #[cfg(test)]
        if config.fail_spawn {
            return Err(std::io::ErrorKind::Other);
        }

        let (sender, receiver) = mpsc::sync_channel(config.queue_capacity);
        let worker_metrics = metrics.clone();
        #[cfg(test)]
        let worker_observer = config.worker_observer.clone();
        thread::Builder::new()
            .name("bangbang-logger".to_owned())
            .spawn(move || {
                #[cfg(test)]
                let _worker_guard = worker_observer.map(WorkerGuard::start);
                run_worker(receiver, writer, worker_metrics);
            })
            .map_err(|error| error.kind())?;

        Ok(Self {
            producer: LoggerProducer {
                sender,
                metrics,
                receipt_timeout: config.receipt_timeout,
            },
            replacement_timeout: config.replacement_timeout,
        })
    }

    pub(super) fn producer(&self) -> LoggerProducer {
        self.producer.clone()
    }

    pub(super) fn replace_writer(
        &self,
        writer: PreparedLoggerWriter,
    ) -> Result<(), ReplaceWriterError> {
        let (worker_token, waiter) = ReplacementToken::new();
        let message = WorkerMessage::replacement(writer, worker_token);
        match self.producer.sender.try_send(message) {
            Ok(()) => waiter.wait(self.replacement_timeout),
            Err(TrySendError::Full(message)) => {
                let Some(writer) = message.into_writer() else {
                    return Err(ReplaceWriterError::TimedOut);
                };
                Err(ReplaceWriterError::Full(writer))
            }
            Err(TrySendError::Disconnected(message)) => {
                let Some(writer) = message.into_writer() else {
                    return Err(ReplaceWriterError::TimedOut);
                };
                Err(ReplaceWriterError::Disconnected(writer))
            }
        }
    }

    #[cfg(test)]
    pub(super) fn disconnect_for_test(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        let (wake_sender, wake_receiver) = mpsc::sync_channel(1);
        let mut message = WorkerMessage::disconnect(wake_sender);
        loop {
            match self.producer.sender.try_send(message) {
                Ok(()) => break,
                Err(TrySendError::Full(returned)) if Instant::now() < deadline => {
                    message = returned;
                    thread::yield_now();
                }
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => return false,
            }
        }
        if wake_receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_err()
        {
            return false;
        }

        let (barrier_sender, barrier_receiver) = mpsc::sync_channel(1);
        match self
            .producer
            .sender
            .try_send(WorkerMessage::barrier(barrier_sender))
        {
            Err(TrySendError::Disconnected(_)) => true,
            Ok(()) => barrier_receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .is_err(),
            Err(TrySendError::Full(_)) => false,
        }
    }
}

#[derive(Debug)]
pub(super) enum ReplaceWriterError {
    Full(PreparedLoggerWriter),
    Disconnected(PreparedLoggerWriter),
    TimedOut,
}

#[derive(Debug, Clone, Copy)]
enum WorkerMessageKind {
    Batch,
    Replace,
    #[cfg(test)]
    Barrier,
    #[cfg(test)]
    Disconnect,
}

struct WorkerMessage {
    kind: WorkerMessageKind,
    batch: Option<LogBatch>,
    receipt: Option<DeliveryReceipt>,
    writer: Option<PreparedLoggerWriter>,
    replacement: Option<ReplacementToken>,
    #[cfg(test)]
    signal: Option<SyncSender<()>>,
}

impl WorkerMessage {
    fn batch(batch: LogBatch, receipt: Option<DeliveryReceipt>) -> Self {
        Self {
            kind: WorkerMessageKind::Batch,
            batch: Some(batch),
            receipt,
            writer: None,
            replacement: None,
            #[cfg(test)]
            signal: None,
        }
    }

    fn replacement(writer: PreparedLoggerWriter, replacement: ReplacementToken) -> Self {
        Self {
            kind: WorkerMessageKind::Replace,
            batch: None,
            receipt: None,
            writer: Some(writer),
            replacement: Some(replacement),
            #[cfg(test)]
            signal: None,
        }
    }

    fn into_writer(mut self) -> Option<PreparedLoggerWriter> {
        self.writer.take()
    }

    #[cfg(test)]
    fn barrier(signal: SyncSender<()>) -> Self {
        Self {
            kind: WorkerMessageKind::Barrier,
            batch: None,
            receipt: None,
            writer: None,
            replacement: None,
            signal: Some(signal),
        }
    }

    #[cfg(test)]
    fn disconnect(signal: SyncSender<()>) -> Self {
        Self {
            kind: WorkerMessageKind::Disconnect,
            batch: None,
            receipt: None,
            writer: None,
            replacement: None,
            signal: Some(signal),
        }
    }
}

fn run_worker(
    receiver: Receiver<WorkerMessage>,
    mut writer: PreparedLoggerWriter,
    metrics: SharedLoggerMetrics,
) {
    while let Ok(mut message) = receiver.recv() {
        match message.kind {
            WorkerMessageKind::Batch => {
                if let Some(batch) = message.batch.take() {
                    deliver_batch(&mut writer, &batch, message.receipt.take(), &metrics);
                }
            }
            WorkerMessageKind::Replace => {
                if let (Some(replacement), Some(token)) =
                    (message.writer.take(), message.replacement.take())
                    && token.commit()
                {
                    let previous = std::mem::replace(&mut writer, replacement);
                    token.notify();
                    drop(previous);
                }
            }
            #[cfg(test)]
            WorkerMessageKind::Barrier => {
                if let Some(sender) = message.signal.take() {
                    let _ = sender.try_send(());
                }
            }
            #[cfg(test)]
            WorkerMessageKind::Disconnect => {
                if let Some(sender) = message.signal.take() {
                    let _ = sender.try_send(());
                }
                return;
            }
        }
    }
}

fn deliver_batch(
    writer: &mut PreparedLoggerWriter,
    batch: &LogBatch,
    receipt: Option<DeliveryReceipt>,
    metrics: &SharedLoggerMetrics,
) {
    let Some(receipt) = receipt else {
        let failures = batch
            .iter()
            .filter(|record| !writer.write(record.as_bytes()))
            .count();
        metrics.record_missed_logs(failures);
        return;
    };

    if !receipt.begin() {
        return;
    }

    let mut failures = 0;
    for record in batch.iter() {
        if receipt.has_timed_out() {
            return;
        }
        if !writer.write(record.as_bytes()) {
            failures += 1;
        }
    }

    if receipt.complete(failures) {
        metrics.record_missed_logs(failures);
        receipt.notify();
    }
}

struct DeliveryReceipt {
    state: Arc<AtomicU8>,
    wake: SyncSender<()>,
}

struct DeliveryReceiptWaiter {
    state: Arc<AtomicU8>,
    wake: Receiver<()>,
}

impl DeliveryReceipt {
    fn new() -> (Self, DeliveryReceiptWaiter) {
        let state = Arc::new(AtomicU8::new(RECEIPT_PENDING));
        let (sender, receiver) = mpsc::sync_channel(1);
        (
            Self {
                state: state.clone(),
                wake: sender,
            },
            DeliveryReceiptWaiter {
                state,
                wake: receiver,
            },
        )
    }

    fn begin(&self) -> bool {
        self.state
            .compare_exchange(
                RECEIPT_PENDING,
                RECEIPT_WRITING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn has_timed_out(&self) -> bool {
        self.state.load(Ordering::Acquire) == RECEIPT_TIMED_OUT
    }

    fn complete(&self, failures: usize) -> bool {
        let failures = u8::try_from(failures).unwrap_or(u8::MAX - RECEIPT_COMPLETE_BASE);
        self.state
            .compare_exchange(
                RECEIPT_WRITING,
                RECEIPT_COMPLETE_BASE.saturating_add(failures),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn notify(&self) {
        let _ = self.wake.try_send(());
    }
}

impl DeliveryReceiptWaiter {
    fn wait(self, timeout: Duration, record_count: usize, metrics: &SharedLoggerMetrics) -> bool {
        match self.wake.recv_timeout(timeout) {
            Ok(()) => self.completed_successfully(),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                self.finish_timeout(record_count, metrics)
            }
        }
    }

    fn completed_successfully(&self) -> bool {
        self.state.load(Ordering::Acquire) == RECEIPT_COMPLETE_BASE
    }

    fn finish_timeout(&self, record_count: usize, metrics: &SharedLoggerMetrics) -> bool {
        loop {
            let state = self.state.load(Ordering::Acquire);
            match state {
                RECEIPT_PENDING | RECEIPT_WRITING => {
                    if self
                        .state
                        .compare_exchange(
                            state,
                            RECEIPT_TIMED_OUT,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        metrics.record_missed_logs(record_count);
                        return false;
                    }
                }
                RECEIPT_TIMED_OUT => return false,
                completed if completed >= RECEIPT_COMPLETE_BASE => {
                    return completed == RECEIPT_COMPLETE_BASE;
                }
                _ => return false,
            }
        }
    }
}

struct ReplacementToken {
    state: Arc<AtomicU8>,
    wake: SyncSender<()>,
}

struct ReplacementWaiter {
    state: Arc<AtomicU8>,
    wake: Receiver<()>,
}

impl ReplacementToken {
    fn new() -> (Self, ReplacementWaiter) {
        let state = Arc::new(AtomicU8::new(REPLACEMENT_PENDING));
        let (sender, receiver) = mpsc::sync_channel(1);
        (
            Self {
                state: state.clone(),
                wake: sender,
            },
            ReplacementWaiter {
                state,
                wake: receiver,
            },
        )
    }

    fn commit(&self) -> bool {
        self.state
            .compare_exchange(
                REPLACEMENT_PENDING,
                REPLACEMENT_COMMITTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn notify(&self) {
        let _ = self.wake.try_send(());
    }
}

impl ReplacementWaiter {
    fn wait(self, timeout: Duration) -> Result<(), ReplaceWriterError> {
        match self.wake.recv_timeout(timeout) {
            Ok(()) => Ok(()),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                self.cancel_or_observe_commit()
            }
        }
    }

    fn cancel_or_observe_commit(&self) -> Result<(), ReplaceWriterError> {
        loop {
            match self.state.load(Ordering::Acquire) {
                REPLACEMENT_PENDING => {
                    if self
                        .state
                        .compare_exchange(
                            REPLACEMENT_PENDING,
                            REPLACEMENT_CANCELLED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Err(ReplaceWriterError::TimedOut);
                    }
                }
                REPLACEMENT_COMMITTED => return Ok(()),
                REPLACEMENT_CANCELLED => return Err(ReplaceWriterError::TimedOut),
                _ => return Err(ReplaceWriterError::TimedOut),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind, Write};
    use std::sync::mpsc;
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::{
        DeliveryReceipt, LoggerDelivery, LoggerDeliveryConfig, PreparedLoggerWriter,
        ReplaceWriterError, ReplacementToken,
    };
    use crate::logger::LoggerLevel;
    use crate::logger::SharedLoggerMetrics;
    use crate::logger::event::{LogBatch, LogOrigin, LogRecord, LoggerAction, LoggerEvent};

    fn record(action: LoggerAction) -> LogRecord {
        LogRecord::encode(
            false,
            false,
            LogOrigin::new("crates/runtime/src/logger.rs", 1),
            LoggerLevel::Info,
            LoggerEvent::Action(action),
        )
    }

    #[derive(Debug)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("writer lock should succeed")
                .extend(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct ShortWriter;

    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len().saturating_sub(1))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            panic!("short writes must not flush")
        }
    }

    #[derive(Debug)]
    struct ZeroWriter;

    impl Write for ZeroWriter {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Ok(0)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            panic!("zero writes must not flush")
        }
    }

    #[derive(Debug)]
    struct FlushFailingWriter;

    impl Write for FlushFailingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(Error::from(ErrorKind::BrokenPipe))
        }
    }

    #[derive(Debug)]
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(Error::from(ErrorKind::WouldBlock))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct WriterGateState {
        entered: bool,
        released: bool,
    }

    #[derive(Debug, Default)]
    struct WriterGate {
        state: Mutex<WriterGateState>,
        changed: Condvar,
    }

    impl WriterGate {
        fn wait_until_entered(&self) {
            let mut state = self.state.lock().expect("gate lock should succeed");
            while !state.entered {
                state = self.changed.wait(state).expect("gate wait should succeed");
            }
        }

        fn release(&self) {
            self.state
                .lock()
                .expect("gate lock should succeed")
                .released = true;
            self.changed.notify_all();
        }
    }

    #[derive(Debug)]
    struct HeldWriter {
        gate: Arc<WriterGate>,
    }

    impl Write for HeldWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let mut state = self.gate.state.lock().expect("gate lock should succeed");
            state.entered = true;
            self.gate.changed.notify_all();
            while !state.released {
                state = self
                    .gate
                    .changed
                    .wait(state)
                    .expect("gate wait should succeed");
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct DropSignalWriter(Option<mpsc::SyncSender<()>>);

    impl Write for DropSignalWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Drop for DropSignalWriter {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.try_send(());
            }
        }
    }

    #[test]
    fn receipt_timeout_wins_before_dequeue_and_accounts_once() {
        let metrics = SharedLoggerMetrics::default();
        let (receipt, waiter) = DeliveryReceipt::new();

        assert!(!waiter.finish_timeout(2, &metrics));
        assert!(receipt.has_timed_out());
        assert!(!receipt.begin());
        assert!(!receipt.complete(0));
        assert_eq!(metrics.missed_log_count(), 2);
    }

    #[test]
    fn receipt_completion_and_timeout_orders_have_one_accounting_owner() {
        let timeout_metrics = SharedLoggerMetrics::default();
        let (timed_out_receipt, timed_out_waiter) = DeliveryReceipt::new();
        assert!(timed_out_receipt.begin());
        assert!(!timed_out_waiter.finish_timeout(2, &timeout_metrics));
        assert!(!timed_out_receipt.complete(0));
        assert_eq!(timeout_metrics.missed_log_count(), 2);

        let success_metrics = SharedLoggerMetrics::default();
        let (successful_receipt, successful_waiter) = DeliveryReceipt::new();
        assert!(successful_receipt.begin());
        assert!(successful_receipt.complete(0));
        assert!(successful_waiter.wait(Duration::ZERO, 1, &success_metrics));
        assert_eq!(success_metrics.missed_log_count(), 0);

        let failure_metrics = SharedLoggerMetrics::default();
        let (failed_receipt, failed_waiter) = DeliveryReceipt::new();
        assert!(failed_receipt.begin());
        assert!(failed_receipt.complete(1));
        failure_metrics.record_missed_logs(1);
        assert!(!failed_waiter.wait(Duration::ZERO, 1, &failure_metrics));
        assert_eq!(failure_metrics.missed_log_count(), 1);
    }

    #[test]
    fn replacement_commit_and_cancel_orders_are_linearized() {
        let (cancelled_token, cancelled_waiter) = ReplacementToken::new();
        assert!(matches!(
            cancelled_waiter.cancel_or_observe_commit(),
            Err(ReplaceWriterError::TimedOut)
        ));
        assert!(!cancelled_token.commit());

        let (committed_token, committed_waiter) = ReplacementToken::new();
        assert!(committed_token.commit());
        assert!(committed_waiter.wait(Duration::ZERO).is_ok());
    }

    #[test]
    fn dropping_all_senders_stops_the_worker_and_drops_its_writer() {
        let (drop_sender, drop_receiver) = mpsc::sync_channel(1);
        let delivery = LoggerDelivery::spawn(
            PreparedLoggerWriter::new(DropSignalWriter(Some(drop_sender))),
            SharedLoggerMetrics::default(),
            LoggerDeliveryConfig::for_test(1, Duration::from_millis(100)),
        )
        .expect("worker should spawn");

        drop(delivery);
        drop_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("worker should exit and drop its writer");
    }

    #[test]
    fn host_delivery_waits_for_complete_record() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let metrics = SharedLoggerMetrics::default();
        let delivery = LoggerDelivery::spawn(
            PreparedLoggerWriter::new(SharedWriter(output.clone())),
            metrics.clone(),
            LoggerDeliveryConfig::for_test(2, Duration::from_millis(100)),
        )
        .expect("worker should spawn");

        assert!(
            delivery
                .producer()
                .deliver_host(LogBatch::one(record(LoggerAction::InstanceStart)))
        );
        assert_eq!(metrics.missed_log_count(), 0);
        assert_eq!(
            *output.lock().expect("output lock should succeed"),
            b"action=InstanceStart\n"
        );
    }

    #[test]
    fn zero_short_error_and_flush_failures_are_single_accounted() {
        for writer in [
            PreparedLoggerWriter::new(ZeroWriter),
            PreparedLoggerWriter::new(ShortWriter),
            PreparedLoggerWriter::new(FailingWriter),
            PreparedLoggerWriter::new(FlushFailingWriter),
        ] {
            let metrics = SharedLoggerMetrics::default();
            let delivery = LoggerDelivery::spawn(
                writer,
                metrics.clone(),
                LoggerDeliveryConfig::for_test(2, Duration::from_millis(100)),
            )
            .expect("worker should spawn");
            assert!(
                !delivery
                    .producer()
                    .deliver_host(LogBatch::one(record(LoggerAction::InstanceStart)))
            );
            assert_eq!(metrics.missed_log_count(), 1);
        }
    }

    #[test]
    fn held_writer_times_out_queued_host_but_not_guest() {
        let gate = Arc::new(WriterGate::default());
        let metrics = SharedLoggerMetrics::default();
        let delivery = LoggerDelivery::spawn(
            PreparedLoggerWriter::new(HeldWriter { gate: gate.clone() }),
            metrics.clone(),
            LoggerDeliveryConfig::for_test(2, Duration::from_millis(10)),
        )
        .expect("worker should spawn");
        let producer = delivery.producer();

        assert!(producer.deliver_guest(LogBatch::one(record(LoggerAction::InstanceStart))));
        gate.wait_until_entered();
        assert!(!producer.deliver_host(LogBatch::one(record(LoggerAction::FlushMetrics))));
        assert_eq!(metrics.missed_log_count(), 1);

        gate.release();
        assert!(producer.wait_for_idle_for_test());
        assert_eq!(metrics.missed_log_count(), 1);
    }

    #[test]
    fn host_timeout_during_write_wins_accounting_once() {
        let gate = Arc::new(WriterGate::default());
        let metrics = SharedLoggerMetrics::default();
        let delivery = LoggerDelivery::spawn(
            PreparedLoggerWriter::new(HeldWriter { gate: gate.clone() }),
            metrics.clone(),
            LoggerDeliveryConfig::for_test(2, Duration::from_millis(10)),
        )
        .expect("worker should spawn");
        let producer = delivery.producer();
        let host_producer = producer.clone();
        let delivery_result = thread::spawn(move || {
            host_producer.deliver_host(LogBatch::one(record(LoggerAction::InstanceStart)))
        });

        gate.wait_until_entered();
        assert!(!delivery_result.join().expect("host caller should return"));
        assert_eq!(metrics.missed_log_count(), 1);
        gate.release();
        assert!(producer.wait_for_idle_for_test());
        assert_eq!(metrics.missed_log_count(), 1);
    }

    #[test]
    fn full_queue_counts_every_rejected_batch_record() {
        let gate = Arc::new(WriterGate::default());
        let metrics = SharedLoggerMetrics::default();
        let delivery = LoggerDelivery::spawn(
            PreparedLoggerWriter::new(HeldWriter { gate: gate.clone() }),
            metrics.clone(),
            LoggerDeliveryConfig::for_test(1, Duration::from_millis(10)),
        )
        .expect("worker should spawn");
        let producer = delivery.producer();

        assert!(producer.deliver_guest(LogBatch::one(record(LoggerAction::InstanceStart))));
        gate.wait_until_entered();
        assert!(producer.deliver_guest(LogBatch::one(record(LoggerAction::FlushMetrics))));
        assert!(!producer.deliver_guest(LogBatch::two(
            record(LoggerAction::InstanceStart),
            record(LoggerAction::FlushMetrics),
        )));
        assert_eq!(metrics.missed_log_count(), 2);

        gate.release();
        assert!(producer.wait_for_idle_for_test());
        assert_eq!(metrics.missed_log_count(), 2);
    }

    #[test]
    fn disconnected_stale_sender_rejects_immediately_and_counts_batch() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let metrics = SharedLoggerMetrics::default();
        let delivery = LoggerDelivery::spawn(
            PreparedLoggerWriter::new(SharedWriter(output)),
            metrics.clone(),
            LoggerDeliveryConfig::for_test(2, Duration::from_millis(10)),
        )
        .expect("worker should spawn");
        let producer = delivery.producer();

        assert!(delivery.disconnect_for_test());
        assert!(!producer.deliver_guest(LogBatch::two(
            record(LoggerAction::InstanceStart),
            record(LoggerAction::FlushMetrics),
        )));
        assert_eq!(metrics.missed_log_count(), 2);
    }
}
