//! Backend-neutral guest interrupt signaling primitives.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

const QUEUE_INTERRUPT_STATUS_BIT: u32 = 1 << 0;
const CONFIG_INTERRUPT_STATUS_BIT: u32 = 1 << 1;
const KNOWN_INTERRUPT_STATUS_BITS: u32 = QUEUE_INTERRUPT_STATUS_BIT | CONFIG_INTERRUPT_STATUS_BIT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GuestInterruptLine(u32);

impl GuestInterruptLine {
    pub fn new(value: u32) -> Result<Self, GuestInterruptLineError> {
        if value == 0 {
            Err(GuestInterruptLineError::Zero)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn raw_value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for GuestInterruptLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestInterruptLineError {
    Zero,
}

impl fmt::Display for GuestInterruptLineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => f.write_str("guest interrupt line 0 is invalid"),
        }
    }
}

impl std::error::Error for GuestInterruptLineError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceInterruptKind {
    Queue,
    Config,
}

impl DeviceInterruptKind {
    pub const fn status(self) -> DeviceInterruptStatus {
        match self {
            Self::Queue => DeviceInterruptStatus(QUEUE_INTERRUPT_STATUS_BIT),
            Self::Config => DeviceInterruptStatus(CONFIG_INTERRUPT_STATUS_BIT),
        }
    }
}

impl fmt::Display for DeviceInterruptKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Queue => f.write_str("queue"),
            Self::Config => f.write_str("configuration"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeviceInterruptStatus(u32);

impl DeviceInterruptStatus {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub fn from_bits(bits: u32) -> Result<Self, DeviceInterruptStatusError> {
        let unknown_bits = bits & !KNOWN_INTERRUPT_STATUS_BITS;
        if unknown_bits == 0 {
            Ok(Self(bits))
        } else {
            Err(DeviceInterruptStatusError::UnknownBits { bits: unknown_bits })
        }
    }

    const fn from_valid_bits(bits: u32) -> Self {
        Self(bits & KNOWN_INTERRUPT_STATUS_BITS)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, kind: DeviceInterruptKind) -> bool {
        self.0 & kind.status().bits() != 0
    }

    pub fn insert(&mut self, kind: DeviceInterruptKind) {
        self.0 |= kind.status().bits();
    }

    pub fn clear(&mut self, status: Self) {
        self.0 &= !status.bits();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceInterruptStatusError {
    UnknownBits { bits: u32 },
}

impl fmt::Display for DeviceInterruptStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBits { bits } => {
                write!(f, "unknown device interrupt status bits 0x{bits:x}")
            }
        }
    }
}

impl std::error::Error for DeviceInterruptStatusError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptSignalError {
    message: String,
}

impl InterruptSignalError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for InterruptSignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for InterruptSignalError {}

pub trait InterruptSink: fmt::Debug + Send + Sync {
    fn signal(&self, line: GuestInterruptLine) -> Result<(), InterruptSignalError>;
}

#[derive(Debug, Clone)]
pub struct DeviceInterruptTrigger {
    line: GuestInterruptLine,
    pending_status: Arc<AtomicU32>,
    sink: Arc<dyn InterruptSink>,
}

impl DeviceInterruptTrigger {
    pub fn new(line: GuestInterruptLine, sink: impl InterruptSink + 'static) -> Self {
        Self::with_shared_sink(line, Arc::new(sink))
    }

    pub fn with_shared_sink(line: GuestInterruptLine, sink: Arc<dyn InterruptSink>) -> Self {
        Self {
            line,
            pending_status: Arc::new(AtomicU32::new(0)),
            sink,
        }
    }

    pub const fn line(&self) -> GuestInterruptLine {
        self.line
    }

    pub fn pending_status(&self) -> DeviceInterruptStatus {
        DeviceInterruptStatus::from_valid_bits(self.pending_status.load(Ordering::Acquire))
    }

    pub fn trigger(&self, kind: DeviceInterruptKind) -> Result<(), DeviceInterruptTriggerError> {
        self.pending_status
            .fetch_or(kind.status().bits(), Ordering::AcqRel);
        self.sink
            .signal(self.line)
            .map_err(|source| DeviceInterruptTriggerError::Signal {
                line: self.line,
                kind,
                source,
            })
    }

    pub fn acknowledge(&self, status: DeviceInterruptStatus) {
        self.pending_status
            .fetch_and(!status.bits(), Ordering::AcqRel);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceInterruptTriggerError {
    Signal {
        line: GuestInterruptLine,
        kind: DeviceInterruptKind,
        source: InterruptSignalError,
    },
}

impl fmt::Display for DeviceInterruptTriggerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Signal { line, kind, source } => {
                write!(
                    f,
                    "failed to signal guest interrupt line {line} for {kind} interrupt: {source}"
                )
            }
        }
    }
}

impl std::error::Error for DeviceInterruptTriggerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Signal { source, .. } => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::sync::{Arc, Mutex};

    use super::{
        DeviceInterruptKind, DeviceInterruptStatus, DeviceInterruptStatusError,
        DeviceInterruptTrigger, DeviceInterruptTriggerError, GuestInterruptLine,
        GuestInterruptLineError, InterruptSignalError, InterruptSink,
    };

    #[derive(Debug)]
    struct RecordingSink {
        lines: Arc<Mutex<Vec<GuestInterruptLine>>>,
        result: Result<(), InterruptSignalError>,
    }

    impl RecordingSink {
        fn successful() -> (Arc<Mutex<Vec<GuestInterruptLine>>>, Self) {
            let lines = Arc::new(Mutex::new(Vec::new()));
            (
                Arc::clone(&lines),
                Self {
                    lines,
                    result: Ok(()),
                },
            )
        }

        fn failing(message: &'static str) -> (Arc<Mutex<Vec<GuestInterruptLine>>>, Self) {
            let lines = Arc::new(Mutex::new(Vec::new()));
            (
                Arc::clone(&lines),
                Self {
                    lines,
                    result: Err(InterruptSignalError::new(message)),
                },
            )
        }
    }

    impl InterruptSink for RecordingSink {
        fn signal(&self, line: GuestInterruptLine) -> Result<(), InterruptSignalError> {
            self.lines
                .lock()
                .expect("recorded interrupt lines lock should not be poisoned")
                .push(line);

            self.result.clone()
        }
    }

    fn line(value: u32) -> GuestInterruptLine {
        GuestInterruptLine::new(value).expect("test interrupt line should be valid")
    }

    fn trigger() -> (DeviceInterruptTrigger, Arc<Mutex<Vec<GuestInterruptLine>>>) {
        let (lines, sink) = RecordingSink::successful();
        (DeviceInterruptTrigger::new(line(32), sink), lines)
    }

    #[test]
    fn guest_interrupt_line_rejects_zero() {
        assert_eq!(
            GuestInterruptLine::new(0),
            Err(GuestInterruptLineError::Zero)
        );
    }

    #[test]
    fn guest_interrupt_line_exposes_raw_value_and_display() {
        let line = line(32);

        assert_eq!(line.raw_value(), 32);
        assert_eq!(line.to_string(), "32");
    }

    #[test]
    fn interrupt_status_bits_distinguish_queue_and_config() {
        let mut status = DeviceInterruptStatus::empty();

        assert!(status.is_empty());
        assert!(!status.contains(DeviceInterruptKind::Queue));
        assert!(!status.contains(DeviceInterruptKind::Config));

        status.insert(DeviceInterruptKind::Queue);
        assert_eq!(status.bits(), 1);
        assert!(status.contains(DeviceInterruptKind::Queue));
        assert!(!status.contains(DeviceInterruptKind::Config));

        status.insert(DeviceInterruptKind::Config);
        assert_eq!(status.bits(), 3);
        assert!(status.contains(DeviceInterruptKind::Queue));
        assert!(status.contains(DeviceInterruptKind::Config));

        status.clear(DeviceInterruptKind::Queue.status());
        assert_eq!(status.bits(), 2);
        assert!(!status.contains(DeviceInterruptKind::Queue));
        assert!(status.contains(DeviceInterruptKind::Config));
    }

    #[test]
    fn interrupt_status_rejects_unknown_bits() {
        assert_eq!(
            DeviceInterruptStatus::from_bits(4),
            Err(DeviceInterruptStatusError::UnknownBits { bits: 4 })
        );
    }

    #[test]
    fn interrupt_status_accepts_known_bits() {
        let mut expected = DeviceInterruptKind::Queue.status();
        expected.insert(DeviceInterruptKind::Config);

        assert_eq!(DeviceInterruptStatus::from_bits(3), Ok(expected));
    }

    #[test]
    fn cloned_trigger_handles_share_pending_status() {
        let (trigger, _) = trigger();
        let cloned_trigger = trigger.clone();

        cloned_trigger
            .trigger(DeviceInterruptKind::Config)
            .expect("config interrupt should signal");

        assert_eq!(
            trigger.pending_status(),
            DeviceInterruptKind::Config.status()
        );
        trigger.acknowledge(DeviceInterruptKind::Config.status());
        assert_eq!(
            cloned_trigger.pending_status(),
            DeviceInterruptStatus::empty()
        );
    }

    #[test]
    fn trigger_records_pending_status_and_signals_line() {
        let (trigger, lines) = trigger();

        assert_eq!(trigger.line(), line(32));
        assert_eq!(trigger.pending_status(), DeviceInterruptStatus::empty());

        trigger
            .trigger(DeviceInterruptKind::Queue)
            .expect("queue interrupt should signal");

        assert_eq!(
            trigger.pending_status(),
            DeviceInterruptKind::Queue.status()
        );
        assert_eq!(
            *lines
                .lock()
                .expect("recorded interrupt lines lock should not be poisoned"),
            vec![line(32)]
        );
    }

    #[test]
    fn repeated_triggers_preserve_pending_status_and_signal_each_time() {
        let (trigger, lines) = trigger();

        trigger
            .trigger(DeviceInterruptKind::Queue)
            .expect("first queue interrupt should signal");
        trigger
            .trigger(DeviceInterruptKind::Queue)
            .expect("second queue interrupt should signal");

        assert_eq!(
            trigger.pending_status(),
            DeviceInterruptKind::Queue.status()
        );
        assert_eq!(
            *lines
                .lock()
                .expect("recorded interrupt lines lock should not be poisoned"),
            vec![line(32), line(32)]
        );
    }

    #[test]
    fn acknowledge_clears_selected_pending_bits() {
        let (trigger, _) = trigger();

        trigger
            .trigger(DeviceInterruptKind::Queue)
            .expect("queue interrupt should signal");
        trigger
            .trigger(DeviceInterruptKind::Config)
            .expect("config interrupt should signal");

        trigger.acknowledge(DeviceInterruptKind::Queue.status());
        assert_eq!(
            trigger.pending_status(),
            DeviceInterruptKind::Config.status()
        );

        trigger.acknowledge(DeviceInterruptKind::Config.status());
        assert_eq!(trigger.pending_status(), DeviceInterruptStatus::empty());
    }

    #[test]
    fn acknowledging_empty_status_is_noop() {
        let (trigger, _) = trigger();

        trigger
            .trigger(DeviceInterruptKind::Config)
            .expect("config interrupt should signal");
        trigger.acknowledge(DeviceInterruptStatus::empty());

        assert_eq!(
            trigger.pending_status(),
            DeviceInterruptKind::Config.status()
        );
    }

    #[test]
    fn trigger_failure_preserves_pending_status_and_source() {
        let (lines, sink) = RecordingSink::failing("injected signal failure");
        let trigger = DeviceInterruptTrigger::new(line(40), sink);

        assert_eq!(
            trigger.trigger(DeviceInterruptKind::Config),
            Err(DeviceInterruptTriggerError::Signal {
                line: line(40),
                kind: DeviceInterruptKind::Config,
                source: InterruptSignalError::new("injected signal failure"),
            })
        );
        assert_eq!(
            trigger.pending_status(),
            DeviceInterruptKind::Config.status()
        );
        assert_eq!(
            *lines
                .lock()
                .expect("recorded interrupt lines lock should not be poisoned"),
            vec![line(40)]
        );

        let err = trigger
            .trigger(DeviceInterruptKind::Queue)
            .expect_err("failing sink should return an error");
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("injected signal failure".to_string())
        );
    }

    #[test]
    fn displays_interrupt_errors() {
        assert_eq!(
            GuestInterruptLineError::Zero.to_string(),
            "guest interrupt line 0 is invalid"
        );
        assert_eq!(
            DeviceInterruptStatusError::UnknownBits { bits: 8 }.to_string(),
            "unknown device interrupt status bits 0x8"
        );
        assert_eq!(
            InterruptSignalError::new("backend failed").to_string(),
            "backend failed"
        );
        assert_eq!(
            DeviceInterruptTriggerError::Signal {
                line: line(32),
                kind: DeviceInterruptKind::Queue,
                source: InterruptSignalError::new("backend failed"),
            }
            .to_string(),
            "failed to signal guest interrupt line 32 for queue interrupt: backend failed"
        );
    }

    #[test]
    fn interrupt_signal_error_exposes_message() {
        let err = InterruptSignalError::new("backend failed");

        assert_eq!(err.message(), "backend failed");
    }

    #[test]
    fn trigger_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<DeviceInterruptTrigger>();
    }
}
