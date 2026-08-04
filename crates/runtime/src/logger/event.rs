use std::fmt::{self, Write as _};
use std::panic::Location;

use super::LoggerLevel;

pub(super) const MAX_LOG_RECORD_BYTES: usize = 512;
const MAX_LOG_ORIGIN_BYTES: usize = 240;
const TRUNCATION_MARKER: &str = "...";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerHttpMethod {
    Get,
    Put,
    Patch,
    Delete,
}

impl LoggerHttpMethod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "Get",
            Self::Put => "Put",
            Self::Patch => "Patch",
            Self::Delete => "Delete",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerApiRoute {
    Root,
    Actions,
    Balloon,
    BalloonHintingStart,
    BalloonHintingStatus,
    BalloonHintingStop,
    BalloonStatistics,
    BootSource,
    CpuConfig,
    Drive,
    Entropy,
    Logger,
    MachineConfig,
    MemoryHotplug,
    Metrics,
    Mmds,
    MmdsConfig,
    NetworkInterface,
    Pmem,
    Serial,
    SnapshotCreate,
    SnapshotLoad,
    Version,
    Vm,
    VmConfig,
    Vsock,
}

impl LoggerApiRoute {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "/",
            Self::Actions => "/actions",
            Self::Balloon => "/balloon",
            Self::BalloonHintingStart => "/balloon/hinting/start",
            Self::BalloonHintingStatus => "/balloon/hinting/status",
            Self::BalloonHintingStop => "/balloon/hinting/stop",
            Self::BalloonStatistics => "/balloon/statistics",
            Self::BootSource => "/boot-source",
            Self::CpuConfig => "/cpu-config",
            Self::Drive => "/drives/{drive_id}",
            Self::Entropy => "/entropy",
            Self::Logger => "/logger",
            Self::MachineConfig => "/machine-config",
            Self::MemoryHotplug => "/hotplug/memory",
            Self::Metrics => "/metrics",
            Self::Mmds => "/mmds",
            Self::MmdsConfig => "/mmds/config",
            Self::NetworkInterface => "/network-interfaces/{iface_id}",
            Self::Pmem => "/pmem/{pmem_id}",
            Self::Serial => "/serial",
            Self::SnapshotCreate => "/snapshot/create",
            Self::SnapshotLoad => "/snapshot/load",
            Self::Version => "/version",
            Self::Vm => "/vm",
            Self::VmConfig => "/vm/config",
            Self::Vsock => "/vsock",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerAction {
    InstanceStart,
    FlushMetrics,
}

/// Fixed API-server control outcomes synchronized with the checked logger audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerApiControlOutcome {
    ServerRunning,
    ServerStopped,
    ConnectionFailed,
    RequestDeprecated,
    RequestCompleted,
    RequestParseBadRequest,
    RequestParsePayloadTooLarge,
}

impl LoggerApiControlOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::ServerRunning | Self::ServerStopped => "server",
            Self::ConnectionFailed => "connection",
            Self::RequestDeprecated | Self::RequestCompleted => "request",
            Self::RequestParseBadRequest | Self::RequestParsePayloadTooLarge => "request-parse",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::ServerRunning => "running",
            Self::ServerStopped => "stopped",
            Self::ConnectionFailed => "failed",
            Self::RequestDeprecated => "deprecated",
            Self::RequestCompleted => "completed",
            Self::RequestParseBadRequest => "bad-request",
            Self::RequestParsePayloadTooLarge => "payload-too-large",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::ConnectionFailed
            | Self::RequestParseBadRequest
            | Self::RequestParsePayloadTooLarge => LoggerLevel::Error,
            Self::RequestDeprecated => LoggerLevel::Warn,
            Self::ServerStopped => LoggerLevel::Debug,
            Self::ServerRunning | Self::RequestCompleted => LoggerLevel::Info,
        }
    }
}

/// Fixed result class for one successfully parsed and dispatched API request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerApiResultOutcome {
    Ok,
    NoContent,
    BadRequest,
    PayloadTooLarge,
}

impl LoggerApiResultOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NoContent => "no-content",
            Self::BadRequest => "bad-request",
            Self::PayloadTooLarge => "payload-too-large",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::Ok | Self::NoContent => LoggerLevel::Info,
            Self::BadRequest | Self::PayloadTooLarge => LoggerLevel::Error,
        }
    }
}

/// Fixed normal process-startup outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStartupOutcome {
    Running,
}

impl ProcessStartupOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
        }
    }
}

/// Fixed public device kinds admitted by logger records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerDeviceKind {
    Balloon,
    Block,
    Entropy,
    MemoryHotplug,
    Network,
    Pmem,
    Serial,
    TimeIdentity,
    Vsock,
}

impl LoggerDeviceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Balloon => "balloon",
            Self::Block => "block",
            Self::Entropy => "entropy",
            Self::MemoryHotplug => "memory-hotplug",
            Self::Network => "network",
            Self::Pmem => "pmem",
            Self::Serial => "serial",
            Self::TimeIdentity => "time-identity",
            Self::Vsock => "vsock",
        }
    }

    pub(crate) const fn from_virtio_device_id(device_id: u32) -> Option<Self> {
        match device_id {
            1 => Some(Self::Network),
            2 => Some(Self::Block),
            3 => Some(Self::Serial),
            4 => Some(Self::Entropy),
            5 => Some(Self::Balloon),
            19 => Some(Self::Vsock),
            24 => Some(Self::MemoryHotplug),
            27 => Some(Self::Pmem),
            _ => None,
        }
    }
}

/// Fixed, value-free balloon data-plane outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerBalloonOutcome {
    InflateSucceeded,
    DeflateSucceeded,
    StatisticsUpdated,
    StatisticsOversized,
    StatisticsFailed,
    HintingSucceeded,
    HintingFailed,
    ReportingSucceeded,
    ReportingFailed,
    MemoryDiscardFailed,
    AccountingFailed,
    QueueDispatchFailed,
    QueueNotificationInactive,
    QueueNotificationUnsupported,
    InterruptDeliveryFailed,
}

impl LoggerBalloonOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::InflateSucceeded => "inflate",
            Self::DeflateSucceeded => "deflate",
            Self::StatisticsUpdated | Self::StatisticsOversized | Self::StatisticsFailed => {
                "statistics"
            }
            Self::HintingSucceeded | Self::HintingFailed => "hinting",
            Self::ReportingSucceeded | Self::ReportingFailed => "reporting",
            Self::MemoryDiscardFailed => "memory-discard",
            Self::AccountingFailed => "accounting",
            Self::QueueDispatchFailed => "queue-dispatch",
            Self::QueueNotificationInactive | Self::QueueNotificationUnsupported => {
                "queue-notification"
            }
            Self::InterruptDeliveryFailed => "interrupt-delivery",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::InflateSucceeded
            | Self::DeflateSucceeded
            | Self::HintingSucceeded
            | Self::ReportingSucceeded => "succeeded",
            Self::StatisticsUpdated => "updated",
            Self::StatisticsOversized => "oversized",
            Self::StatisticsFailed
            | Self::HintingFailed
            | Self::ReportingFailed
            | Self::MemoryDiscardFailed
            | Self::AccountingFailed
            | Self::QueueDispatchFailed
            | Self::InterruptDeliveryFailed => "failed",
            Self::QueueNotificationInactive => "inactive",
            Self::QueueNotificationUnsupported => "unsupported",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::InflateSucceeded
            | Self::DeflateSucceeded
            | Self::StatisticsUpdated
            | Self::HintingSucceeded
            | Self::ReportingSucceeded => LoggerLevel::Info,
            Self::StatisticsOversized | Self::QueueNotificationUnsupported => LoggerLevel::Warn,
            Self::StatisticsFailed
            | Self::HintingFailed
            | Self::ReportingFailed
            | Self::MemoryDiscardFailed
            | Self::AccountingFailed
            | Self::QueueDispatchFailed
            | Self::QueueNotificationInactive
            | Self::InterruptDeliveryFailed => LoggerLevel::Error,
        }
    }
}

/// Fixed, value-free virtio-mem data-plane outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerMemoryHotplugOutcome {
    RequestSucceeded,
    RequestUnsupported,
    StateQuerySucceeded,
    PolicyRejected,
    MutationFailed,
    MutationRollbackSucceeded,
    MutationRollbackFailed,
    RequestParseFailed,
    ResponseWriteFailed,
    MemoryDiscardFailed,
    ConfigurationUpdateSucceeded,
    ConfigurationUpdateFailed,
    QueueDispatchFailed,
    QueueNotificationInactive,
    QueueNotificationUnsupported,
    InterruptDeliveryFailed,
}

impl LoggerMemoryHotplugOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::RequestSucceeded | Self::RequestUnsupported => "request",
            Self::StateQuerySucceeded => "state-query",
            Self::PolicyRejected => "policy",
            Self::MutationFailed => "mutation",
            Self::MutationRollbackSucceeded | Self::MutationRollbackFailed => "mutation-rollback",
            Self::RequestParseFailed => "request-parse",
            Self::ResponseWriteFailed => "response-write",
            Self::MemoryDiscardFailed => "memory-discard",
            Self::ConfigurationUpdateSucceeded | Self::ConfigurationUpdateFailed => {
                "configuration-update"
            }
            Self::QueueDispatchFailed => "queue-dispatch",
            Self::QueueNotificationInactive | Self::QueueNotificationUnsupported => {
                "queue-notification"
            }
            Self::InterruptDeliveryFailed => "interrupt-delivery",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::RequestSucceeded
            | Self::StateQuerySucceeded
            | Self::MutationRollbackSucceeded
            | Self::ConfigurationUpdateSucceeded => "succeeded",
            Self::RequestUnsupported | Self::QueueNotificationUnsupported => "unsupported",
            Self::PolicyRejected => "rejected",
            Self::MutationFailed
            | Self::MutationRollbackFailed
            | Self::RequestParseFailed
            | Self::ResponseWriteFailed
            | Self::MemoryDiscardFailed
            | Self::ConfigurationUpdateFailed
            | Self::QueueDispatchFailed
            | Self::InterruptDeliveryFailed => "failed",
            Self::QueueNotificationInactive => "inactive",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::RequestSucceeded
            | Self::StateQuerySucceeded
            | Self::ConfigurationUpdateSucceeded => LoggerLevel::Info,
            Self::RequestUnsupported
            | Self::PolicyRejected
            | Self::MutationRollbackSucceeded
            | Self::QueueNotificationUnsupported => LoggerLevel::Warn,
            Self::MutationFailed
            | Self::MutationRollbackFailed
            | Self::RequestParseFailed
            | Self::ResponseWriteFailed
            | Self::MemoryDiscardFailed
            | Self::ConfigurationUpdateFailed
            | Self::QueueDispatchFailed
            | Self::QueueNotificationInactive
            | Self::InterruptDeliveryFailed => LoggerLevel::Error,
        }
    }
}

/// Fixed, value-free entropy data-plane outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerEntropyOutcome {
    FillSucceeded,
    FillFailed,
    RequestParseFailed,
    BufferWriteFailed,
    QueueDispatchFailed,
    QueueNotificationInactive,
    QueueNotificationUnsupported,
    RateLimiterThrottled,
    RateLimiterResumed,
    EntropyProviderFailed,
    InterruptDeliveryFailed,
}

impl LoggerEntropyOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::FillSucceeded | Self::FillFailed => "fill",
            Self::RequestParseFailed => "request-parse",
            Self::BufferWriteFailed => "buffer-write",
            Self::QueueDispatchFailed => "queue-dispatch",
            Self::QueueNotificationInactive | Self::QueueNotificationUnsupported => {
                "queue-notification"
            }
            Self::RateLimiterThrottled | Self::RateLimiterResumed => "rate-limiter",
            Self::EntropyProviderFailed => "entropy-provider",
            Self::InterruptDeliveryFailed => "interrupt-delivery",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::FillSucceeded => "succeeded",
            Self::FillFailed
            | Self::RequestParseFailed
            | Self::BufferWriteFailed
            | Self::QueueDispatchFailed
            | Self::EntropyProviderFailed
            | Self::InterruptDeliveryFailed => "failed",
            Self::QueueNotificationInactive => "inactive",
            Self::QueueNotificationUnsupported => "unsupported",
            Self::RateLimiterThrottled => "throttled",
            Self::RateLimiterResumed => "resumed",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::RateLimiterThrottled | Self::RateLimiterResumed => LoggerLevel::Debug,
            Self::FillSucceeded => LoggerLevel::Info,
            Self::QueueNotificationUnsupported => LoggerLevel::Warn,
            Self::FillFailed
            | Self::RequestParseFailed
            | Self::BufferWriteFailed
            | Self::QueueDispatchFailed
            | Self::QueueNotificationInactive
            | Self::EntropyProviderFailed
            | Self::InterruptDeliveryFailed => LoggerLevel::Error,
        }
    }
}

/// Fixed, value-free serial input and output outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerSerialOutcome {
    InputReadSucceeded,
    InputReadFailed,
    InputRearmSucceeded,
    InputBackpressurePaused,
    InputDetachEof,
    InputDetachFailed,
    OutputFailed,
    RateLimiterThrottled,
    InterruptDeliverySucceeded,
    InterruptDeliveryFailed,
}

impl LoggerSerialOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::InputReadSucceeded | Self::InputReadFailed => "input-read",
            Self::InputRearmSucceeded => "input-rearm",
            Self::InputBackpressurePaused => "input-backpressure",
            Self::InputDetachEof | Self::InputDetachFailed => "input-detach",
            Self::OutputFailed => "output",
            Self::RateLimiterThrottled => "rate-limiter",
            Self::InterruptDeliverySucceeded | Self::InterruptDeliveryFailed => {
                "interrupt-delivery"
            }
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::InputReadSucceeded
            | Self::InputRearmSucceeded
            | Self::InterruptDeliverySucceeded => "succeeded",
            Self::InputReadFailed
            | Self::InputDetachFailed
            | Self::OutputFailed
            | Self::InterruptDeliveryFailed => "failed",
            Self::InputBackpressurePaused => "paused",
            Self::InputDetachEof => "eof",
            Self::RateLimiterThrottled => "throttled",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::InputRearmSucceeded
            | Self::InputBackpressurePaused
            | Self::RateLimiterThrottled => LoggerLevel::Debug,
            Self::InputReadSucceeded | Self::InputDetachEof | Self::InterruptDeliverySucceeded => {
                LoggerLevel::Info
            }
            Self::InputReadFailed
            | Self::InputDetachFailed
            | Self::OutputFailed
            | Self::InterruptDeliveryFailed => LoggerLevel::Error,
        }
    }
}

/// Fixed, value-free arm64 time and clone-identity outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerTimeIdentityOutcome {
    RtcReadRejected,
    RtcWriteRejected,
    RtcRestoreSucceeded,
    RtcRestoreFailed,
    PlatformPublicationSucceeded,
    PlatformPublicationFailed,
    VmGenIdReplacementSucceeded,
    VmGenIdReplacementFailed,
    VmGenIdNotificationSucceeded,
    VmGenIdNotificationFailed,
    VmClockUpdateSucceeded,
    VmClockUpdateFailed,
    VmClockUpdatePartiallyCommitted,
    VmClockNotificationSucceeded,
    VmClockNotificationFailed,
    OrderedRestoreSucceeded,
    OrderedRestoreFailed,
    OrderedRestorePartiallyCommitted,
    PvTimeInitializationSucceeded,
    PvTimeInitializationFailed,
    PvTimeAccountingPublished,
    PvTimeAccountingDiscarded,
    PvTimeAccountingFailed,
}

impl LoggerTimeIdentityOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::RtcReadRejected => "rtc-read",
            Self::RtcWriteRejected => "rtc-write",
            Self::RtcRestoreSucceeded | Self::RtcRestoreFailed => "rtc-restore",
            Self::PlatformPublicationSucceeded | Self::PlatformPublicationFailed => {
                "platform-publication"
            }
            Self::VmGenIdReplacementSucceeded | Self::VmGenIdReplacementFailed => {
                "vmgenid-replacement"
            }
            Self::VmGenIdNotificationSucceeded | Self::VmGenIdNotificationFailed => {
                "vmgenid-notification"
            }
            Self::VmClockUpdateSucceeded
            | Self::VmClockUpdateFailed
            | Self::VmClockUpdatePartiallyCommitted => "vmclock-update",
            Self::VmClockNotificationSucceeded | Self::VmClockNotificationFailed => {
                "vmclock-notification"
            }
            Self::OrderedRestoreSucceeded
            | Self::OrderedRestoreFailed
            | Self::OrderedRestorePartiallyCommitted => "ordered-restore",
            Self::PvTimeInitializationSucceeded | Self::PvTimeInitializationFailed => {
                "pvtime-initialization"
            }
            Self::PvTimeAccountingPublished
            | Self::PvTimeAccountingDiscarded
            | Self::PvTimeAccountingFailed => "pvtime-accounting",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::RtcReadRejected | Self::RtcWriteRejected => "rejected",
            Self::RtcRestoreSucceeded
            | Self::PlatformPublicationSucceeded
            | Self::VmGenIdReplacementSucceeded
            | Self::VmGenIdNotificationSucceeded
            | Self::VmClockUpdateSucceeded
            | Self::VmClockNotificationSucceeded
            | Self::OrderedRestoreSucceeded
            | Self::PvTimeInitializationSucceeded => "succeeded",
            Self::RtcRestoreFailed
            | Self::PlatformPublicationFailed
            | Self::VmGenIdReplacementFailed
            | Self::VmGenIdNotificationFailed
            | Self::VmClockUpdateFailed
            | Self::VmClockNotificationFailed
            | Self::OrderedRestoreFailed
            | Self::PvTimeInitializationFailed
            | Self::PvTimeAccountingFailed => "failed",
            Self::VmClockUpdatePartiallyCommitted | Self::OrderedRestorePartiallyCommitted => {
                "partially-committed"
            }
            Self::PvTimeAccountingPublished => "published",
            Self::PvTimeAccountingDiscarded => "discarded",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::PvTimeAccountingDiscarded => LoggerLevel::Debug,
            Self::RtcRestoreSucceeded
            | Self::PlatformPublicationSucceeded
            | Self::VmGenIdReplacementSucceeded
            | Self::VmGenIdNotificationSucceeded
            | Self::VmClockUpdateSucceeded
            | Self::VmClockNotificationSucceeded
            | Self::OrderedRestoreSucceeded
            | Self::PvTimeInitializationSucceeded
            | Self::PvTimeAccountingPublished => LoggerLevel::Info,
            Self::RtcReadRejected | Self::RtcWriteRejected => LoggerLevel::Warn,
            Self::RtcRestoreFailed
            | Self::PlatformPublicationFailed
            | Self::VmGenIdReplacementFailed
            | Self::VmGenIdNotificationFailed
            | Self::VmClockUpdateFailed
            | Self::VmClockUpdatePartiallyCommitted
            | Self::VmClockNotificationFailed
            | Self::OrderedRestoreFailed
            | Self::OrderedRestorePartiallyCommitted
            | Self::PvTimeInitializationFailed
            | Self::PvTimeAccountingFailed => LoggerLevel::Error,
        }
    }
}

/// Fixed, value-free block data-plane outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerBlockOutcome {
    RequestSucceeded,
    RequestUnsupported,
    RequestParseFailed,
    RequestIoFailed,
    StatusWriteFailed,
    QueueDispatchFailed,
    QueueNotificationInactive,
    QueueNotificationUnsupported,
    RateLimiterThrottled,
    RateLimiterResumed,
    AsyncEngineThrottled,
    AsyncEngineFailed,
    VhostUserNotificationSucceeded,
    VhostUserNotificationFailed,
    VhostUserDisconnected,
    VhostUserTerminal,
    VhostUserConfigSucceeded,
    VhostUserConfigFailed,
    InterruptDeliveryFailed,
}

impl LoggerBlockOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::RequestSucceeded | Self::RequestUnsupported => "request",
            Self::RequestParseFailed => "request-parse",
            Self::RequestIoFailed => "request-io",
            Self::StatusWriteFailed => "status-write",
            Self::QueueDispatchFailed => "queue-dispatch",
            Self::QueueNotificationInactive | Self::QueueNotificationUnsupported => {
                "queue-notification"
            }
            Self::RateLimiterThrottled | Self::RateLimiterResumed => "rate-limiter",
            Self::AsyncEngineThrottled | Self::AsyncEngineFailed => "async-engine",
            Self::VhostUserNotificationSucceeded
            | Self::VhostUserNotificationFailed
            | Self::VhostUserDisconnected
            | Self::VhostUserTerminal => "vhost-user-notification",
            Self::VhostUserConfigSucceeded | Self::VhostUserConfigFailed => "vhost-user-config",
            Self::InterruptDeliveryFailed => "interrupt-delivery",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::RequestSucceeded
            | Self::VhostUserNotificationSucceeded
            | Self::VhostUserConfigSucceeded => "succeeded",
            Self::RequestUnsupported | Self::QueueNotificationUnsupported => "unsupported",
            Self::RequestParseFailed
            | Self::RequestIoFailed
            | Self::StatusWriteFailed
            | Self::QueueDispatchFailed
            | Self::AsyncEngineFailed
            | Self::VhostUserNotificationFailed
            | Self::VhostUserConfigFailed
            | Self::InterruptDeliveryFailed => "failed",
            Self::QueueNotificationInactive => "inactive",
            Self::RateLimiterThrottled | Self::AsyncEngineThrottled => "throttled",
            Self::RateLimiterResumed => "resumed",
            Self::VhostUserDisconnected => "disconnected",
            Self::VhostUserTerminal => "terminal",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::RateLimiterThrottled | Self::RateLimiterResumed | Self::AsyncEngineThrottled => {
                LoggerLevel::Debug
            }
            Self::RequestSucceeded
            | Self::VhostUserNotificationSucceeded
            | Self::VhostUserConfigSucceeded => LoggerLevel::Info,
            Self::RequestUnsupported
            | Self::QueueNotificationUnsupported
            | Self::VhostUserDisconnected => LoggerLevel::Warn,
            Self::RequestParseFailed
            | Self::RequestIoFailed
            | Self::StatusWriteFailed
            | Self::QueueDispatchFailed
            | Self::QueueNotificationInactive
            | Self::AsyncEngineFailed
            | Self::VhostUserNotificationFailed
            | Self::VhostUserTerminal
            | Self::VhostUserConfigFailed
            | Self::InterruptDeliveryFailed => LoggerLevel::Error,
        }
    }
}

/// Fixed, value-free persistent-memory data-plane outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerPmemOutcome {
    FlushSucceeded,
    FlushFailed,
    RequestParseFailed,
    StatusWriteFailed,
    QueueDispatchFailed,
    QueueNotificationInactive,
    QueueNotificationUnsupported,
    RateLimiterThrottled,
    RateLimiterResumed,
    InterruptDeliveryFailed,
}

impl LoggerPmemOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::FlushSucceeded | Self::FlushFailed => "flush",
            Self::RequestParseFailed => "request-parse",
            Self::StatusWriteFailed => "status-write",
            Self::QueueDispatchFailed => "queue-dispatch",
            Self::QueueNotificationInactive | Self::QueueNotificationUnsupported => {
                "queue-notification"
            }
            Self::RateLimiterThrottled | Self::RateLimiterResumed => "rate-limiter",
            Self::InterruptDeliveryFailed => "interrupt-delivery",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::FlushSucceeded => "succeeded",
            Self::FlushFailed
            | Self::RequestParseFailed
            | Self::StatusWriteFailed
            | Self::QueueDispatchFailed
            | Self::InterruptDeliveryFailed => "failed",
            Self::QueueNotificationInactive => "inactive",
            Self::QueueNotificationUnsupported => "unsupported",
            Self::RateLimiterThrottled => "throttled",
            Self::RateLimiterResumed => "resumed",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::RateLimiterThrottled | Self::RateLimiterResumed => LoggerLevel::Debug,
            Self::FlushSucceeded => LoggerLevel::Info,
            Self::QueueNotificationUnsupported => LoggerLevel::Warn,
            Self::FlushFailed
            | Self::RequestParseFailed
            | Self::StatusWriteFailed
            | Self::QueueDispatchFailed
            | Self::QueueNotificationInactive
            | Self::InterruptDeliveryFailed => LoggerLevel::Error,
        }
    }
}

/// Fixed, value-free network and MMDS data-plane outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerNetworkOutcome {
    RxSucceeded,
    RxBufferMalformed,
    RxBufferTooSmall,
    RxBufferUnavailable,
    RxProviderFailed,
    TxSucceeded,
    TxFrameMalformed,
    TxSpoofRejected,
    TxProviderFailed,
    QueueDispatchFailed,
    QueueNotificationInactive,
    QueueNotificationUnsupported,
    RateLimiterThrottled,
    RateLimiterResumed,
    PacketProviderFailed,
    PacketProviderPartial,
    MmdsRequestDetoured,
    MmdsTokenKeyRotated,
    MmdsTokenKeyRotationFailed,
    InterruptDeliveryFailed,
}

impl LoggerNetworkOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::RxSucceeded => "rx",
            Self::RxBufferMalformed | Self::RxBufferTooSmall | Self::RxBufferUnavailable => {
                "rx-buffer"
            }
            Self::RxProviderFailed => "rx-provider",
            Self::TxSucceeded => "tx",
            Self::TxFrameMalformed | Self::TxSpoofRejected => "tx-frame",
            Self::TxProviderFailed => "tx-provider",
            Self::QueueDispatchFailed => "queue-dispatch",
            Self::QueueNotificationInactive | Self::QueueNotificationUnsupported => {
                "queue-notification"
            }
            Self::RateLimiterThrottled | Self::RateLimiterResumed => "rate-limiter",
            Self::PacketProviderFailed | Self::PacketProviderPartial => "packet-provider",
            Self::MmdsRequestDetoured => "mmds-request",
            Self::MmdsTokenKeyRotated | Self::MmdsTokenKeyRotationFailed => "mmds-token-key",
            Self::InterruptDeliveryFailed => "interrupt-delivery",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::RxSucceeded | Self::TxSucceeded => "succeeded",
            Self::RxBufferMalformed | Self::TxFrameMalformed => "malformed",
            Self::RxBufferTooSmall => "too-small",
            Self::RxBufferUnavailable => "unavailable",
            Self::RxProviderFailed
            | Self::TxProviderFailed
            | Self::QueueDispatchFailed
            | Self::PacketProviderFailed
            | Self::MmdsTokenKeyRotationFailed
            | Self::InterruptDeliveryFailed => "failed",
            Self::TxSpoofRejected => "spoof-rejected",
            Self::QueueNotificationInactive => "inactive",
            Self::QueueNotificationUnsupported => "unsupported",
            Self::RateLimiterThrottled => "throttled",
            Self::RateLimiterResumed => "resumed",
            Self::PacketProviderPartial => "partial",
            Self::MmdsRequestDetoured => "detoured",
            Self::MmdsTokenKeyRotated => "rotated",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::RxBufferUnavailable | Self::RateLimiterThrottled | Self::RateLimiterResumed => {
                LoggerLevel::Debug
            }
            Self::RxSucceeded
            | Self::TxSucceeded
            | Self::MmdsRequestDetoured
            | Self::MmdsTokenKeyRotated => LoggerLevel::Info,
            Self::RxBufferTooSmall
            | Self::TxSpoofRejected
            | Self::QueueNotificationUnsupported
            | Self::PacketProviderPartial => LoggerLevel::Warn,
            Self::RxBufferMalformed
            | Self::RxProviderFailed
            | Self::TxFrameMalformed
            | Self::TxProviderFailed
            | Self::QueueDispatchFailed
            | Self::QueueNotificationInactive
            | Self::PacketProviderFailed
            | Self::MmdsTokenKeyRotationFailed
            | Self::InterruptDeliveryFailed => LoggerLevel::Error,
        }
    }
}

/// Fixed, value-free vsock queue and connection outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerVsockOutcome {
    RxSucceeded,
    RxBufferMalformed,
    RxBufferTooSmall,
    TxSucceeded,
    TxPacketMalformed,
    QueueDispatchFailed,
    QueueNotificationInactive,
    QueueNotificationUnsupported,
    HostConnectionAccepted,
    HostConnectionCompleted,
    HostConnectionPending,
    HostConnectionDropped,
    GuestConnectionRetained,
    GuestConnectionForwarded,
    GuestConnectionUpdated,
    GuestConnectionClosed,
    GuestConnectionIgnored,
    GuestConnectionDropped,
    ConnectionResetQueued,
    ConnectionResetDropped,
    TransportResetSucceeded,
    TransportResetFailed,
    InterruptDeliveryFailed,
}

impl LoggerVsockOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::RxSucceeded => "rx",
            Self::RxBufferMalformed | Self::RxBufferTooSmall => "rx-buffer",
            Self::TxSucceeded => "tx",
            Self::TxPacketMalformed => "tx-packet",
            Self::QueueDispatchFailed => "queue-dispatch",
            Self::QueueNotificationInactive | Self::QueueNotificationUnsupported => {
                "queue-notification"
            }
            Self::HostConnectionAccepted
            | Self::HostConnectionCompleted
            | Self::HostConnectionPending
            | Self::HostConnectionDropped => "host-connection",
            Self::GuestConnectionRetained
            | Self::GuestConnectionForwarded
            | Self::GuestConnectionUpdated
            | Self::GuestConnectionClosed
            | Self::GuestConnectionIgnored
            | Self::GuestConnectionDropped => "guest-connection",
            Self::ConnectionResetQueued | Self::ConnectionResetDropped => "connection-reset",
            Self::TransportResetSucceeded | Self::TransportResetFailed => "transport-reset",
            Self::InterruptDeliveryFailed => "interrupt-delivery",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::RxSucceeded | Self::TxSucceeded | Self::TransportResetSucceeded => "succeeded",
            Self::RxBufferMalformed | Self::TxPacketMalformed => "malformed",
            Self::RxBufferTooSmall => "too-small",
            Self::QueueDispatchFailed
            | Self::TransportResetFailed
            | Self::InterruptDeliveryFailed => "failed",
            Self::QueueNotificationInactive => "inactive",
            Self::QueueNotificationUnsupported => "unsupported",
            Self::HostConnectionAccepted => "accepted",
            Self::HostConnectionCompleted => "completed",
            Self::HostConnectionPending => "pending",
            Self::HostConnectionDropped | Self::GuestConnectionDropped => "dropped",
            Self::GuestConnectionRetained => "retained",
            Self::GuestConnectionForwarded => "forwarded",
            Self::GuestConnectionUpdated => "updated",
            Self::GuestConnectionClosed => "closed",
            Self::GuestConnectionIgnored => "ignored",
            Self::ConnectionResetQueued => "queued",
            Self::ConnectionResetDropped => "dropped",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::HostConnectionPending | Self::GuestConnectionIgnored => LoggerLevel::Debug,
            Self::RxSucceeded
            | Self::TxSucceeded
            | Self::HostConnectionAccepted
            | Self::HostConnectionCompleted
            | Self::GuestConnectionRetained
            | Self::GuestConnectionForwarded
            | Self::GuestConnectionUpdated
            | Self::GuestConnectionClosed
            | Self::ConnectionResetQueued
            | Self::TransportResetSucceeded => LoggerLevel::Info,
            Self::RxBufferTooSmall
            | Self::QueueNotificationUnsupported
            | Self::HostConnectionDropped
            | Self::GuestConnectionDropped
            | Self::ConnectionResetDropped => LoggerLevel::Warn,
            Self::RxBufferMalformed
            | Self::TxPacketMalformed
            | Self::QueueDispatchFailed
            | Self::QueueNotificationInactive
            | Self::TransportResetFailed
            | Self::InterruptDeliveryFailed => LoggerLevel::Error,
        }
    }
}

/// Fixed, value-free backend outcomes observed before HVF errors are formatted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerBackendOutcome {
    CacheConfigurationFailed,
    MemoryMappingFailed,
    MemoryDiscardFailed,
    VmCreationFailed,
    VmCleanupFailed,
    VcpuStartFailed,
    VcpuRunFailed,
    VcpuExitGuestShutdown,
    VcpuExitGuestReset,
    VcpuExitUnsupported,
    MmioDispatchFailed,
    InterruptDeliveryFailed,
    VirtualTimerActivated,
    VirtualTimerFailed,
}

impl LoggerBackendOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::CacheConfigurationFailed => "cache-configuration",
            Self::MemoryMappingFailed => "memory-map",
            Self::MemoryDiscardFailed => "memory-discard",
            Self::VmCreationFailed => "vm-create",
            Self::VmCleanupFailed => "vm-cleanup",
            Self::VcpuStartFailed => "vcpu-start",
            Self::VcpuRunFailed => "vcpu-run",
            Self::VcpuExitGuestShutdown | Self::VcpuExitGuestReset | Self::VcpuExitUnsupported => {
                "vcpu-exit"
            }
            Self::MmioDispatchFailed => "mmio-dispatch",
            Self::InterruptDeliveryFailed => "interrupt-delivery",
            Self::VirtualTimerActivated | Self::VirtualTimerFailed => "virtual-timer",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::CacheConfigurationFailed
            | Self::MemoryMappingFailed
            | Self::MemoryDiscardFailed
            | Self::VmCreationFailed
            | Self::VmCleanupFailed
            | Self::VcpuStartFailed
            | Self::VcpuRunFailed
            | Self::MmioDispatchFailed
            | Self::InterruptDeliveryFailed
            | Self::VirtualTimerFailed => "failed",
            Self::VcpuExitGuestShutdown => "guest-shutdown",
            Self::VcpuExitGuestReset => "guest-reset",
            Self::VcpuExitUnsupported => "unsupported",
            Self::VirtualTimerActivated => "activated",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::VcpuExitGuestShutdown | Self::VcpuExitGuestReset => LoggerLevel::Info,
            Self::VirtualTimerActivated => LoggerLevel::Debug,
            Self::VcpuExitUnsupported => LoggerLevel::Warn,
            Self::CacheConfigurationFailed
            | Self::MemoryMappingFailed
            | Self::MemoryDiscardFailed
            | Self::VmCreationFailed
            | Self::VmCleanupFailed
            | Self::VcpuStartFailed
            | Self::VcpuRunFailed
            | Self::MmioDispatchFailed
            | Self::InterruptDeliveryFailed
            | Self::VirtualTimerFailed => LoggerLevel::Error,
        }
    }
}

/// Fixed, value-free generic device transport outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerTransportOutcome {
    MmioRegistrationSucceeded,
    MmioRegistrationFailed,
    MmioReleaseSucceeded,
    MmioReleaseFailed,
    MmioAccessFailed(Option<LoggerDeviceKind>),
    FeatureNegotiationRejected(LoggerDeviceKind),
    DeviceConfigRejected(LoggerDeviceKind),
    DeviceConfigFailed(LoggerDeviceKind),
    QueueConfigurationRejected(LoggerDeviceKind),
    QueueNotificationSucceeded(LoggerDeviceKind),
    QueueNotificationFailed(LoggerDeviceKind),
    UsedRingRejected(Option<LoggerDeviceKind>),
    DeviceActivationSucceeded(LoggerDeviceKind),
    DeviceActivationFailed(LoggerDeviceKind),
    DeviceResetSucceeded(LoggerDeviceKind),
    DeviceResetUnsupported(LoggerDeviceKind),
    DeviceResetFailed(LoggerDeviceKind),
    PciFunctionPublished(LoggerDeviceKind),
    PciFunctionPublicationFailed(LoggerDeviceKind),
    PciFunctionRemoved(LoggerDeviceKind),
    PciFunctionRemovalFailed(LoggerDeviceKind),
    PciConfigRejected(LoggerDeviceKind),
    MsiConfigurationSucceeded(LoggerDeviceKind),
    MsiConfigurationFailed(LoggerDeviceKind),
    InterruptDelivered(LoggerDeviceKind),
    InterruptDeliveryFailed(LoggerDeviceKind),
    PublicationRollbackSucceeded(Option<LoggerDeviceKind>),
    PublicationRollbackFailed(Option<LoggerDeviceKind>),
    RateLimiterRejected(LoggerDeviceKind),
}

impl LoggerTransportOutcome {
    pub const fn device_kind(self) -> Option<LoggerDeviceKind> {
        match self {
            Self::MmioRegistrationSucceeded
            | Self::MmioRegistrationFailed
            | Self::MmioReleaseSucceeded
            | Self::MmioReleaseFailed => None,
            Self::MmioAccessFailed(kind)
            | Self::UsedRingRejected(kind)
            | Self::PublicationRollbackSucceeded(kind)
            | Self::PublicationRollbackFailed(kind) => kind,
            Self::FeatureNegotiationRejected(kind)
            | Self::DeviceConfigRejected(kind)
            | Self::DeviceConfigFailed(kind)
            | Self::QueueConfigurationRejected(kind)
            | Self::QueueNotificationSucceeded(kind)
            | Self::QueueNotificationFailed(kind)
            | Self::DeviceActivationSucceeded(kind)
            | Self::DeviceActivationFailed(kind)
            | Self::DeviceResetSucceeded(kind)
            | Self::DeviceResetUnsupported(kind)
            | Self::DeviceResetFailed(kind)
            | Self::PciFunctionPublished(kind)
            | Self::PciFunctionPublicationFailed(kind)
            | Self::PciFunctionRemoved(kind)
            | Self::PciFunctionRemovalFailed(kind)
            | Self::PciConfigRejected(kind)
            | Self::MsiConfigurationSucceeded(kind)
            | Self::MsiConfigurationFailed(kind)
            | Self::InterruptDelivered(kind)
            | Self::InterruptDeliveryFailed(kind)
            | Self::RateLimiterRejected(kind) => Some(kind),
        }
    }

    pub const fn operation(self) -> &'static str {
        match self {
            Self::MmioRegistrationSucceeded | Self::MmioRegistrationFailed => "mmio-registration",
            Self::MmioReleaseSucceeded | Self::MmioReleaseFailed => "mmio-release",
            Self::MmioAccessFailed(_) => "mmio-access",
            Self::FeatureNegotiationRejected(_) => "feature-negotiation",
            Self::DeviceConfigRejected(_) | Self::DeviceConfigFailed(_) => "device-config",
            Self::QueueConfigurationRejected(_) => "queue-configuration",
            Self::QueueNotificationSucceeded(_) | Self::QueueNotificationFailed(_) => {
                "queue-notification"
            }
            Self::UsedRingRejected(_) => "used-ring",
            Self::DeviceActivationSucceeded(_) | Self::DeviceActivationFailed(_) => {
                "device-activation"
            }
            Self::DeviceResetSucceeded(_)
            | Self::DeviceResetUnsupported(_)
            | Self::DeviceResetFailed(_) => "device-reset",
            Self::PciFunctionPublished(_) | Self::PciFunctionPublicationFailed(_) => {
                "pci-publication"
            }
            Self::PciFunctionRemoved(_) | Self::PciFunctionRemovalFailed(_) => "pci-removal",
            Self::PciConfigRejected(_) => "pci-config",
            Self::MsiConfigurationSucceeded(_) | Self::MsiConfigurationFailed(_) => {
                "msi-configuration"
            }
            Self::InterruptDelivered(_) | Self::InterruptDeliveryFailed(_) => "interrupt-delivery",
            Self::PublicationRollbackSucceeded(_) | Self::PublicationRollbackFailed(_) => {
                "publication-rollback"
            }
            Self::RateLimiterRejected(_) => "rate-limiter",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::MmioRegistrationSucceeded
            | Self::MmioReleaseSucceeded
            | Self::QueueNotificationSucceeded(_)
            | Self::DeviceActivationSucceeded(_)
            | Self::DeviceResetSucceeded(_)
            | Self::MsiConfigurationSucceeded(_)
            | Self::PublicationRollbackSucceeded(_) => "succeeded",
            Self::PciFunctionPublished(_) => "published",
            Self::PciFunctionRemoved(_) => "removed",
            Self::InterruptDelivered(_) => "delivered",
            Self::FeatureNegotiationRejected(_)
            | Self::DeviceConfigRejected(_)
            | Self::QueueConfigurationRejected(_)
            | Self::UsedRingRejected(_)
            | Self::PciConfigRejected(_)
            | Self::RateLimiterRejected(_) => "rejected",
            Self::DeviceResetUnsupported(_) => "unsupported",
            Self::MmioRegistrationFailed
            | Self::MmioReleaseFailed
            | Self::MmioAccessFailed(_)
            | Self::DeviceConfigFailed(_)
            | Self::QueueNotificationFailed(_)
            | Self::DeviceActivationFailed(_)
            | Self::DeviceResetFailed(_)
            | Self::PciFunctionPublicationFailed(_)
            | Self::PciFunctionRemovalFailed(_)
            | Self::MsiConfigurationFailed(_)
            | Self::InterruptDeliveryFailed(_)
            | Self::PublicationRollbackFailed(_) => "failed",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::MmioRegistrationSucceeded
            | Self::MmioReleaseSucceeded
            | Self::QueueNotificationSucceeded(_)
            | Self::MsiConfigurationSucceeded(_)
            | Self::InterruptDelivered(_)
            | Self::RateLimiterRejected(_) => LoggerLevel::Debug,
            Self::DeviceActivationSucceeded(_)
            | Self::DeviceResetSucceeded(_)
            | Self::PciFunctionPublished(_)
            | Self::PciFunctionRemoved(_) => LoggerLevel::Info,
            Self::FeatureNegotiationRejected(_)
            | Self::DeviceConfigRejected(_)
            | Self::QueueConfigurationRejected(_)
            | Self::UsedRingRejected(_)
            | Self::DeviceResetUnsupported(_)
            | Self::PciConfigRejected(_)
            | Self::PublicationRollbackSucceeded(_) => LoggerLevel::Warn,
            Self::MmioRegistrationFailed
            | Self::MmioReleaseFailed
            | Self::MmioAccessFailed(_)
            | Self::DeviceConfigFailed(_)
            | Self::QueueNotificationFailed(_)
            | Self::DeviceActivationFailed(_)
            | Self::DeviceResetFailed(_)
            | Self::PciFunctionPublicationFailed(_)
            | Self::PciFunctionRemovalFailed(_)
            | Self::MsiConfigurationFailed(_)
            | Self::InterruptDeliveryFailed(_)
            | Self::PublicationRollbackFailed(_) => LoggerLevel::Error,
        }
    }
}

/// Fixed VM lifecycle and host-control outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerLifecycleOutcome {
    BackendStartupSucceeded,
    BackendStartupFailed,
    VmStartSucceeded,
    VmStartRejected,
    VmStartFailed,
    VmPauseSucceeded,
    VmPauseUnchanged,
    VmPauseRejected,
    VmPauseFailed,
    VmResumeSucceeded,
    VmResumeUnchanged,
    VmResumeRejected,
    VmResumeFailed,
    VmStopSucceeded,
    VmStopFailed,
    DeviceAttachSucceeded(LoggerDeviceKind),
    DeviceAttachRejected(LoggerDeviceKind),
    DeviceAttachFailed(LoggerDeviceKind),
    DeviceUpdateSucceeded(LoggerDeviceKind),
    DeviceUpdateRejected(LoggerDeviceKind),
    DeviceUpdateFailed(LoggerDeviceKind),
    DeviceDetachSucceeded(LoggerDeviceKind),
    DeviceDetachRejected(LoggerDeviceKind),
    DeviceDetachFailed(LoggerDeviceKind),
}

impl LoggerLifecycleOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::BackendStartupSucceeded | Self::BackendStartupFailed => "backend-startup",
            Self::VmStartSucceeded | Self::VmStartRejected | Self::VmStartFailed => "vm-start",
            Self::VmPauseSucceeded
            | Self::VmPauseUnchanged
            | Self::VmPauseRejected
            | Self::VmPauseFailed => "vm-pause",
            Self::VmResumeSucceeded
            | Self::VmResumeUnchanged
            | Self::VmResumeRejected
            | Self::VmResumeFailed => "vm-resume",
            Self::VmStopSucceeded | Self::VmStopFailed => "vm-stop",
            Self::DeviceAttachSucceeded(_)
            | Self::DeviceAttachRejected(_)
            | Self::DeviceAttachFailed(_) => "device-attach",
            Self::DeviceUpdateSucceeded(_)
            | Self::DeviceUpdateRejected(_)
            | Self::DeviceUpdateFailed(_) => "device-update",
            Self::DeviceDetachSucceeded(_)
            | Self::DeviceDetachRejected(_)
            | Self::DeviceDetachFailed(_) => "device-detach",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::BackendStartupSucceeded
            | Self::VmStartSucceeded
            | Self::VmPauseSucceeded
            | Self::VmResumeSucceeded
            | Self::VmStopSucceeded
            | Self::DeviceAttachSucceeded(_)
            | Self::DeviceUpdateSucceeded(_)
            | Self::DeviceDetachSucceeded(_) => "succeeded",
            Self::VmPauseUnchanged | Self::VmResumeUnchanged => "unchanged",
            Self::VmStartRejected
            | Self::VmPauseRejected
            | Self::VmResumeRejected
            | Self::DeviceAttachRejected(_)
            | Self::DeviceUpdateRejected(_)
            | Self::DeviceDetachRejected(_) => "rejected",
            Self::BackendStartupFailed
            | Self::VmStartFailed
            | Self::VmPauseFailed
            | Self::VmResumeFailed
            | Self::VmStopFailed
            | Self::DeviceAttachFailed(_)
            | Self::DeviceUpdateFailed(_)
            | Self::DeviceDetachFailed(_) => "failed",
        }
    }

    pub const fn device_kind(self) -> Option<LoggerDeviceKind> {
        match self {
            Self::DeviceAttachSucceeded(kind)
            | Self::DeviceAttachRejected(kind)
            | Self::DeviceAttachFailed(kind)
            | Self::DeviceUpdateSucceeded(kind)
            | Self::DeviceUpdateRejected(kind)
            | Self::DeviceUpdateFailed(kind)
            | Self::DeviceDetachSucceeded(kind)
            | Self::DeviceDetachRejected(kind)
            | Self::DeviceDetachFailed(kind) => Some(kind),
            _ => None,
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::VmPauseUnchanged | Self::VmResumeUnchanged => LoggerLevel::Debug,
            Self::BackendStartupFailed
            | Self::VmStartRejected
            | Self::VmStartFailed
            | Self::VmPauseRejected
            | Self::VmPauseFailed
            | Self::VmResumeRejected
            | Self::VmResumeFailed
            | Self::VmStopFailed
            | Self::DeviceAttachRejected(_)
            | Self::DeviceAttachFailed(_)
            | Self::DeviceUpdateRejected(_)
            | Self::DeviceUpdateFailed(_)
            | Self::DeviceDetachRejected(_)
            | Self::DeviceDetachFailed(_) => LoggerLevel::Error,
            Self::BackendStartupSucceeded
            | Self::VmStartSucceeded
            | Self::VmPauseSucceeded
            | Self::VmResumeSucceeded
            | Self::VmStopSucceeded
            | Self::DeviceAttachSucceeded(_)
            | Self::DeviceUpdateSucceeded(_)
            | Self::DeviceDetachSucceeded(_) => LoggerLevel::Info,
        }
    }
}

/// Fixed snapshot request outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerSnapshotOutcome {
    CreateSucceeded,
    CreateRejected,
    CreateFailed,
    CreateCancelled,
    LoadSucceeded,
    LoadRejected,
    LoadFailed,
    LoadCancelled,
}

impl LoggerSnapshotOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::CreateSucceeded
            | Self::CreateRejected
            | Self::CreateFailed
            | Self::CreateCancelled => "snapshot-create",
            Self::LoadSucceeded | Self::LoadRejected | Self::LoadFailed | Self::LoadCancelled => {
                "snapshot-load"
            }
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::CreateSucceeded | Self::LoadSucceeded => "succeeded",
            Self::CreateRejected | Self::LoadRejected => "rejected",
            Self::CreateFailed | Self::LoadFailed => "failed",
            Self::CreateCancelled | Self::LoadCancelled => "cancelled",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::CreateSucceeded | Self::LoadSucceeded => LoggerLevel::Info,
            Self::CreateCancelled | Self::LoadCancelled => LoggerLevel::Warn,
            Self::CreateRejected | Self::CreateFailed | Self::LoadRejected | Self::LoadFailed => {
                LoggerLevel::Error
            }
        }
    }
}

/// Fixed process-observed boot-worker outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerApiWorkerOutcome {
    Running,
    Exited,
    Stopped,
    Failed,
}

impl LoggerApiWorkerOutcome {
    pub const fn operation(self) -> &'static str {
        "boot-worker"
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::Running | Self::Exited => LoggerLevel::Info,
            Self::Stopped => LoggerLevel::Debug,
            Self::Failed => LoggerLevel::Error,
        }
    }
}

/// Fixed process-observed metrics-worker outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerObservabilityOutcome {
    MetricsWorkerFailed,
}

impl LoggerObservabilityOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::MetricsWorkerFailed => "metrics-worker",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::MetricsWorkerFailed => "failed",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::MetricsWorkerFailed => LoggerLevel::Error,
        }
    }
}

/// Fixed deferred signal and process-shutdown convergence outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoggerProcessSignalOutcome {
    HostSignalReceived,
    CancellationRequested,
    GuestPoweroff,
    GuestReset,
    ShutdownOrderly,
    ShutdownAbnormal,
}

impl LoggerProcessSignalOutcome {
    pub const fn operation(self) -> &'static str {
        match self {
            Self::HostSignalReceived => "host-signal",
            Self::CancellationRequested => "cancellation",
            Self::GuestPoweroff | Self::GuestReset => "guest-power",
            Self::ShutdownOrderly | Self::ShutdownAbnormal => "shutdown",
        }
    }

    pub const fn outcome(self) -> &'static str {
        match self {
            Self::HostSignalReceived => "received",
            Self::CancellationRequested => "requested",
            Self::GuestPoweroff => "poweroff",
            Self::GuestReset => "reset",
            Self::ShutdownOrderly => "orderly",
            Self::ShutdownAbnormal => "abnormal",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::ShutdownAbnormal => LoggerLevel::Error,
            Self::HostSignalReceived
            | Self::CancellationRequested
            | Self::GuestPoweroff
            | Self::GuestReset
            | Self::ShutdownOrderly => LoggerLevel::Info,
        }
    }
}

/// Stable process termination categories emitted by the executable lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTerminalCategory {
    Success,
    Configuration,
    ProcessFailure,
    Cancelled,
    Panic,
}

impl ProcessTerminalCategory {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Configuration => "configuration",
            Self::ProcessFailure => "process-failure",
            Self::Cancelled => "cancelled",
            Self::Panic => "panic",
        }
    }

    pub(super) const fn level(self) -> LoggerLevel {
        match self {
            Self::Success | Self::Cancelled => LoggerLevel::Info,
            Self::Configuration | Self::ProcessFailure | Self::Panic => LoggerLevel::Error,
        }
    }
}

impl LoggerAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InstanceStart => "InstanceStart",
            Self::FlushMetrics => "FlushMetrics",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoggerEvent {
    ApiControl(LoggerApiControlOutcome),
    ApiWorker(LoggerApiWorkerOutcome),
    ApiRequest {
        method: LoggerHttpMethod,
        route: LoggerApiRoute,
    },
    ApiResult(LoggerApiResultOutcome),
    Action(LoggerAction),
    BootTime {
        wall_time_us: u64,
        cpu_time_us: u64,
    },
    Backend(LoggerBackendOutcome),
    Balloon(LoggerBalloonOutcome),
    Block(LoggerBlockOutcome),
    Entropy(LoggerEntropyOutcome),
    Lifecycle(LoggerLifecycleOutcome),
    MemoryHotplug(LoggerMemoryHotplugOutcome),
    Network(LoggerNetworkOutcome),
    Observability(LoggerObservabilityOutcome),
    Pmem(LoggerPmemOutcome),
    RateLimitRecovery {
        suppressed: u64,
    },
    ProcessPanic,
    ProcessSignal(LoggerProcessSignalOutcome),
    ProcessStartup(ProcessStartupOutcome),
    ProcessExit(ProcessTerminalCategory),
    Serial(LoggerSerialOutcome),
    Snapshot(LoggerSnapshotOutcome),
    TimeIdentity(LoggerTimeIdentityOutcome),
    Transport(LoggerTransportOutcome),
    Vsock(LoggerVsockOutcome),
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LogOrigin<'a> {
    file: &'a str,
    line: u32,
}

impl<'a> LogOrigin<'a> {
    #[cfg(test)]
    pub(super) const fn new(file: &'a str, line: u32) -> Self {
        Self { file, line }
    }
}

impl<'a> From<&'a Location<'a>> for LogOrigin<'a> {
    fn from(location: &'a Location<'a>) -> Self {
        Self {
            file: location.file(),
            line: location.line(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct LogRecord {
    bytes: [u8; MAX_LOG_RECORD_BYTES],
    len: usize,
}

impl fmt::Debug for LogRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LogRecord")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl LogRecord {
    pub(super) fn encode(
        show_level: bool,
        show_log_origin: bool,
        origin: LogOrigin<'_>,
        level: LoggerLevel,
        event: LoggerEvent,
    ) -> Self {
        let mut encoder = RecordEncoder::default();

        if show_level {
            encoder.push_str("level=");
            encoder.push_str(level.as_str());
            encoder.push_byte(b' ');
        }
        if show_log_origin {
            encoder.push_str("origin=");
            encoder.push_origin(origin.file);
            encoder.push_byte(b':');
            encoder.push_u64(u64::from(origin.line));
            encoder.push_byte(b' ');
        }

        match event {
            LoggerEvent::ApiControl(outcome) => {
                encoder.push_str("operation=");
                encoder.push_str(outcome.operation());
                encoder.push_str(" outcome=");
                encoder.push_str(outcome.outcome());
            }
            LoggerEvent::ApiWorker(outcome) => {
                encoder.push_str("operation=");
                encoder.push_str(outcome.operation());
                encoder.push_str(" outcome=");
                encoder.push_str(outcome.outcome());
            }
            LoggerEvent::ApiRequest { method, route } => {
                encoder.push_str("The API server received a ");
                encoder.push_str(method.as_str());
                encoder.push_str(" request on \"");
                encoder.push_str(route.as_str());
                encoder.push_str("\".");
            }
            LoggerEvent::ApiResult(outcome) => {
                encoder.push_str("action=request outcome=");
                encoder.push_str(outcome.as_str());
            }
            LoggerEvent::Action(action) => {
                encoder.push_str("action=");
                encoder.push_str(action.as_str());
            }
            LoggerEvent::BootTime {
                wall_time_us,
                cpu_time_us,
            } => {
                encoder.push_str("Guest-boot-time = ");
                encoder.push_padded_u64(wall_time_us, 6);
                encoder.push_str(" us ");
                encoder.push_u64(wall_time_us / 1_000);
                encoder.push_str(" ms, ");
                encoder.push_padded_u64(cpu_time_us, 6);
                encoder.push_str(" CPU us ");
                encoder.push_u64(cpu_time_us / 1_000);
                encoder.push_str(" CPU ms");
            }
            LoggerEvent::Backend(outcome) => {
                encoder.push_str("operation=");
                encoder.push_str(outcome.operation());
                encoder.push_str(" outcome=");
                encoder.push_str(outcome.outcome());
            }
            LoggerEvent::Balloon(outcome) => {
                encode_fixed_device_outcome(
                    &mut encoder,
                    LoggerDeviceKind::Balloon,
                    outcome.operation(),
                    outcome.outcome(),
                );
            }
            LoggerEvent::Block(outcome) => {
                encode_fixed_device_outcome(
                    &mut encoder,
                    LoggerDeviceKind::Block,
                    outcome.operation(),
                    outcome.outcome(),
                );
            }
            LoggerEvent::Entropy(outcome) => {
                encode_fixed_device_outcome(
                    &mut encoder,
                    LoggerDeviceKind::Entropy,
                    outcome.operation(),
                    outcome.outcome(),
                );
            }
            LoggerEvent::Lifecycle(outcome) => {
                if let Some(kind) = outcome.device_kind() {
                    encoder.push_str("device-kind=");
                    encoder.push_str(kind.as_str());
                    encoder.push_byte(b' ');
                }
                encoder.push_str("operation=");
                encoder.push_str(outcome.operation());
                encoder.push_str(" outcome=");
                encoder.push_str(outcome.outcome());
            }
            LoggerEvent::MemoryHotplug(outcome) => {
                encode_fixed_device_outcome(
                    &mut encoder,
                    LoggerDeviceKind::MemoryHotplug,
                    outcome.operation(),
                    outcome.outcome(),
                );
            }
            LoggerEvent::Network(outcome) => {
                encode_fixed_device_outcome(
                    &mut encoder,
                    LoggerDeviceKind::Network,
                    outcome.operation(),
                    outcome.outcome(),
                );
            }
            LoggerEvent::Observability(outcome) => {
                encoder.push_str("operation=");
                encoder.push_str(outcome.operation());
                encoder.push_str(" outcome=");
                encoder.push_str(outcome.outcome());
            }
            LoggerEvent::Pmem(outcome) => {
                encode_fixed_device_outcome(
                    &mut encoder,
                    LoggerDeviceKind::Pmem,
                    outcome.operation(),
                    outcome.outcome(),
                );
            }
            LoggerEvent::RateLimitRecovery { suppressed } => {
                encoder.push_u64(suppressed);
                encoder.push_str(" messages were suppressed due to rate limiting");
            }
            LoggerEvent::ProcessPanic => encoder.push_str("event=process-panic"),
            LoggerEvent::ProcessSignal(outcome) => {
                encoder.push_str("operation=");
                encoder.push_str(outcome.operation());
                encoder.push_str(" outcome=");
                encoder.push_str(outcome.outcome());
            }
            LoggerEvent::ProcessStartup(outcome) => {
                encoder.push_str("operation=process-startup outcome=");
                encoder.push_str(outcome.as_str());
            }
            LoggerEvent::ProcessExit(category) => {
                encoder.push_str("event=process-exit category=");
                encoder.push_str(category.as_str());
            }
            LoggerEvent::Serial(outcome) => {
                encode_fixed_device_outcome(
                    &mut encoder,
                    LoggerDeviceKind::Serial,
                    outcome.operation(),
                    outcome.outcome(),
                );
            }
            LoggerEvent::Snapshot(outcome) => {
                encoder.push_str("operation=");
                encoder.push_str(outcome.operation());
                encoder.push_str(" outcome=");
                encoder.push_str(outcome.outcome());
            }
            LoggerEvent::TimeIdentity(outcome) => {
                encode_fixed_device_outcome(
                    &mut encoder,
                    LoggerDeviceKind::TimeIdentity,
                    outcome.operation(),
                    outcome.outcome(),
                );
            }
            LoggerEvent::Transport(outcome) => {
                if let Some(kind) = outcome.device_kind() {
                    encoder.push_str("device-kind=");
                    encoder.push_str(kind.as_str());
                    encoder.push_byte(b' ');
                }
                encoder.push_str("operation=");
                encoder.push_str(outcome.operation());
                encoder.push_str(" outcome=");
                encoder.push_str(outcome.outcome());
            }
            LoggerEvent::Vsock(outcome) => {
                encode_fixed_device_outcome(
                    &mut encoder,
                    LoggerDeviceKind::Vsock,
                    outcome.operation(),
                    outcome.outcome(),
                );
            }
        }

        encoder.finish()
    }

    pub(super) fn as_bytes(&self) -> &[u8] {
        match self.bytes.get(..self.len) {
            Some(bytes) => bytes,
            None => &[],
        }
    }

    #[cfg(test)]
    pub(super) fn as_str(&self) -> &str {
        std::str::from_utf8(self.as_bytes()).unwrap_or("")
    }
}

fn encode_fixed_device_outcome(
    encoder: &mut RecordEncoder,
    kind: LoggerDeviceKind,
    operation: &str,
    outcome: &str,
) {
    encoder.push_str("device-kind=");
    encoder.push_str(kind.as_str());
    encoder.push_byte(b' ');
    encoder.push_str("operation=");
    encoder.push_str(operation);
    encoder.push_str(" outcome=");
    encoder.push_str(outcome);
}

/// Opaque, fixed panic records prepared before a panic hook can use them.
#[derive(Clone)]
pub struct PanicLogRecords {
    plain: LogRecord,
    level: LogRecord,
    origin: LogRecord,
    level_and_origin: LogRecord,
}

impl fmt::Debug for PanicLogRecords {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PanicLogRecords")
            .finish_non_exhaustive()
    }
}

impl Default for PanicLogRecords {
    #[track_caller]
    fn default() -> Self {
        Self::new()
    }
}

impl PanicLogRecords {
    /// Preencodes every permitted prefix form using only caller metadata.
    #[track_caller]
    pub fn new() -> Self {
        let origin = LogOrigin::from(Location::caller());
        Self {
            plain: LogRecord::encode(
                false,
                false,
                origin,
                LoggerLevel::Error,
                LoggerEvent::ProcessPanic,
            ),
            level: LogRecord::encode(
                true,
                false,
                origin,
                LoggerLevel::Error,
                LoggerEvent::ProcessPanic,
            ),
            origin: LogRecord::encode(
                false,
                true,
                origin,
                LoggerLevel::Error,
                LoggerEvent::ProcessPanic,
            ),
            level_and_origin: LogRecord::encode(
                true,
                true,
                origin,
                LoggerLevel::Error,
                LoggerEvent::ProcessPanic,
            ),
        }
    }

    /// Returns the fixed unprefixed fallback record.
    pub fn plain_bytes(&self) -> &[u8] {
        self.plain.as_bytes()
    }

    pub(super) const fn select(&self, show_level: bool, show_log_origin: bool) -> &LogRecord {
        match (show_level, show_log_origin) {
            (false, false) => &self.plain,
            (true, false) => &self.level,
            (false, true) => &self.origin,
            (true, true) => &self.level_and_origin,
        }
    }
}

const MAX_LOG_BATCH_RECORDS: usize = 16;

#[derive(Debug)]
pub(super) struct LogBatch {
    records: [Option<LogRecord>; MAX_LOG_BATCH_RECORDS],
    len: usize,
}

impl LogBatch {
    pub(super) fn empty() -> Self {
        Self {
            records: std::array::from_fn(|_| None),
            len: 0,
        }
    }

    pub(super) fn one(record: LogRecord) -> Self {
        let mut batch = Self::empty();
        let inserted = batch.push(record);
        debug_assert!(inserted);
        batch
    }

    pub(super) fn two(first: LogRecord, second: LogRecord) -> Self {
        let mut batch = Self::one(first);
        let inserted = batch.push(second);
        debug_assert!(inserted);
        batch
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(super) const fn len(&self) -> usize {
        self.len
    }

    pub(super) fn push(&mut self, record: LogRecord) -> bool {
        let Some(slot) = self.records.get_mut(self.len) else {
            return false;
        };
        *slot = Some(record);
        self.len += 1;
        true
    }

    pub(super) fn prepend(&mut self, record: LogRecord) -> bool {
        if self.len == self.records.len() {
            return false;
        }
        let Some(active) = self.records.get_mut(..=self.len) else {
            return false;
        };
        active.rotate_right(1);
        let Some(slot) = active.first_mut() else {
            return false;
        };
        *slot = Some(record);
        self.len += 1;
        true
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &LogRecord> {
        self.records
            .iter()
            .take(self.len)
            .filter_map(Option::as_ref)
    }
}

#[derive(Debug)]
struct RecordEncoder {
    bytes: [u8; MAX_LOG_RECORD_BYTES],
    len: usize,
    truncated: bool,
}

impl Default for RecordEncoder {
    fn default() -> Self {
        Self {
            bytes: [0; MAX_LOG_RECORD_BYTES],
            len: 0,
            truncated: false,
        }
    }
}

impl RecordEncoder {
    fn push_byte(&mut self, byte: u8) {
        let mut buffer = [0_u8; 4];
        self.push_str(char::from(byte).encode_utf8(&mut buffer));
    }

    fn push_str(&mut self, value: &str) {
        if self.truncated {
            return;
        }

        let payload_capacity = MAX_LOG_RECORD_BYTES - 1;
        let remaining = payload_capacity.saturating_sub(self.len);
        if value.len() <= remaining {
            if self.push_bytes(value.as_bytes()) {
                return;
            }
            self.truncated = true;
            return;
        }

        let content_capacity = remaining.saturating_sub(TRUNCATION_MARKER.len());
        let prefix_len = utf8_prefix_len(value, content_capacity);
        if let Some(prefix) = value.as_bytes().get(..prefix_len) {
            let _ = self.push_bytes(prefix);
        }

        let marker_len = TRUNCATION_MARKER
            .len()
            .min(payload_capacity.saturating_sub(self.len));
        if let Some(marker) = TRUNCATION_MARKER.as_bytes().get(..marker_len) {
            let _ = self.push_bytes(marker);
        }
        self.truncated = true;
    }

    fn push_bytes(&mut self, value: &[u8]) -> bool {
        let Some(end) = self.len.checked_add(value.len()) else {
            return false;
        };
        let Some(target) = self.bytes.get_mut(self.len..end) else {
            return false;
        };
        target.copy_from_slice(value);
        self.len = end;
        true
    }

    fn push_origin(&mut self, file: &str) {
        let normalized = normalize_origin(file);
        if normalized.len() <= MAX_LOG_ORIGIN_BYTES {
            self.push_str(normalized);
            return;
        }

        let prefix_capacity = MAX_LOG_ORIGIN_BYTES - TRUNCATION_MARKER.len();
        let prefix_len = utf8_prefix_len(normalized, prefix_capacity);
        if let Some(prefix) = normalized.get(..prefix_len) {
            self.push_str(prefix);
        }
        self.push_str(TRUNCATION_MARKER);
    }

    fn push_padded_u64(&mut self, value: u64, width: usize) {
        if write!(self, "{value:>width$}").is_err() {
            self.truncated = true;
        }
    }

    fn push_u64(&mut self, value: u64) {
        if write!(self, "{value}").is_err() {
            self.truncated = true;
        }
    }

    fn finish(mut self) -> LogRecord {
        if let Some(byte) = self.bytes.get_mut(self.len) {
            *byte = b'\n';
            self.len += 1;
        }
        LogRecord {
            bytes: self.bytes,
            len: self.len,
        }
    }
}

impl fmt::Write for RecordEncoder {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.push_str(value);
        Ok(())
    }
}

fn normalize_origin(file: &str) -> &str {
    let has_parent_component = file.split(['/', '\\']).any(|component| component == "..");
    let has_windows_prefix = file.as_bytes().get(1).is_some_and(|byte| *byte == b':');
    let needs_normalization = file.starts_with('/')
        || file.starts_with('\\')
        || has_parent_component
        || has_windows_prefix;

    if !needs_normalization {
        return if file.is_empty() { "<unknown>" } else { file };
    }

    if let Some(suffix) = repository_origin_suffix(file) {
        return suffix;
    }

    file.rsplit(['/', '\\'])
        .find(|component| !component.is_empty() && *component != "..")
        .unwrap_or("<unknown>")
}

fn repository_origin_suffix(file: &str) -> Option<&str> {
    file.rmatch_indices("crates").find_map(|(index, _)| {
        let bytes = file.as_bytes();
        let before_is_boundary = index == 0
            || bytes
                .get(index.saturating_sub(1))
                .is_some_and(|byte| matches!(*byte, b'/' | b'\\'));
        let after_index = index.checked_add("crates".len())?;
        let after_is_boundary = bytes
            .get(after_index)
            .is_some_and(|byte| matches!(*byte, b'/' | b'\\'));
        let suffix = file.get(index..)?;
        let stays_beneath_crates = !suffix.split(['/', '\\']).any(|component| component == "..");

        (before_is_boundary && after_is_boundary && stays_beneath_crates).then_some(suffix)
    })
}

fn utf8_prefix_len(value: &str, maximum: usize) -> usize {
    let mut length = maximum.min(value.len());
    while !value.is_char_boundary(length) {
        length -= 1;
    }
    length
}

#[cfg(test)]
mod tests {
    use super::{
        LogOrigin, LogRecord, LoggerAction, LoggerApiControlOutcome, LoggerApiResultOutcome,
        LoggerApiRoute, LoggerApiWorkerOutcome, LoggerBackendOutcome, LoggerBalloonOutcome,
        LoggerBlockOutcome, LoggerDeviceKind, LoggerEntropyOutcome, LoggerEvent, LoggerHttpMethod,
        LoggerLifecycleOutcome, LoggerMemoryHotplugOutcome, LoggerNetworkOutcome,
        LoggerObservabilityOutcome, LoggerPmemOutcome, LoggerProcessSignalOutcome,
        LoggerSerialOutcome, LoggerSnapshotOutcome, LoggerTimeIdentityOutcome,
        LoggerTransportOutcome, LoggerVsockOutcome, MAX_LOG_RECORD_BYTES, PanicLogRecords,
        ProcessStartupOutcome, ProcessTerminalCategory, normalize_origin,
    };
    use crate::logger::LoggerLevel;

    #[test]
    fn encodes_existing_short_record_shapes() {
        let origin = LogOrigin::new("crates/runtime/src/logger.rs", 42);
        let api = LogRecord::encode(
            false,
            false,
            origin,
            LoggerLevel::Info,
            LoggerEvent::ApiRequest {
                method: LoggerHttpMethod::Put,
                route: LoggerApiRoute::Mmds,
            },
        );
        let action = LogRecord::encode(
            true,
            false,
            origin,
            LoggerLevel::Info,
            LoggerEvent::Action(LoggerAction::InstanceStart),
        );
        let boot = LogRecord::encode(
            false,
            false,
            origin,
            LoggerLevel::Info,
            LoggerEvent::BootTime {
                wall_time_us: 7_123,
                cpu_time_us: 1_456,
            },
        );
        let recovery = LogRecord::encode(
            false,
            false,
            origin,
            LoggerLevel::Warn,
            LoggerEvent::RateLimitRecovery { suppressed: 3 },
        );

        assert_eq!(
            api.as_str(),
            "The API server received a Put request on \"/mmds\".\n"
        );
        assert_eq!(action.as_str(), "level=Info action=InstanceStart\n");
        assert_eq!(
            boot.as_str(),
            "Guest-boot-time =   7123 us 7 ms,   1456 CPU us 1 CPU ms\n"
        );
        assert_eq!(
            recovery.as_str(),
            "3 messages were suppressed due to rate limiting\n"
        );
    }

    #[test]
    fn encodes_closed_api_control_result_and_startup_shapes() {
        let origin = LogOrigin::new("crates/bangbang/src/api_server.rs", 19);
        let control_cases = [
            (
                LoggerApiControlOutcome::ServerRunning,
                LoggerLevel::Info,
                "operation=server outcome=running\n",
            ),
            (
                LoggerApiControlOutcome::ServerStopped,
                LoggerLevel::Debug,
                "operation=server outcome=stopped\n",
            ),
            (
                LoggerApiControlOutcome::ConnectionFailed,
                LoggerLevel::Error,
                "operation=connection outcome=failed\n",
            ),
            (
                LoggerApiControlOutcome::RequestDeprecated,
                LoggerLevel::Warn,
                "operation=request outcome=deprecated\n",
            ),
            (
                LoggerApiControlOutcome::RequestCompleted,
                LoggerLevel::Info,
                "operation=request outcome=completed\n",
            ),
            (
                LoggerApiControlOutcome::RequestParseBadRequest,
                LoggerLevel::Error,
                "operation=request-parse outcome=bad-request\n",
            ),
            (
                LoggerApiControlOutcome::RequestParsePayloadTooLarge,
                LoggerLevel::Error,
                "operation=request-parse outcome=payload-too-large\n",
            ),
        ];
        for (outcome, level, expected) in control_cases {
            let record = LogRecord::encode(
                false,
                false,
                origin,
                outcome.level(),
                LoggerEvent::ApiControl(outcome),
            );
            assert_eq!(outcome.level(), level);
            assert_eq!(record.as_str(), expected);
            assert!(record.as_bytes().len() <= MAX_LOG_RECORD_BYTES);
            assert!(std::str::from_utf8(record.as_bytes()).is_ok());
        }

        let result_cases = [
            (LoggerApiResultOutcome::Ok, LoggerLevel::Info, "ok"),
            (
                LoggerApiResultOutcome::NoContent,
                LoggerLevel::Info,
                "no-content",
            ),
            (
                LoggerApiResultOutcome::BadRequest,
                LoggerLevel::Error,
                "bad-request",
            ),
            (
                LoggerApiResultOutcome::PayloadTooLarge,
                LoggerLevel::Error,
                "payload-too-large",
            ),
        ];
        for (outcome, level, expected) in result_cases {
            let record = LogRecord::encode(
                true,
                false,
                origin,
                outcome.level(),
                LoggerEvent::ApiResult(outcome),
            );
            assert_eq!(outcome.level(), level);
            assert_eq!(
                record.as_str(),
                format!(
                    "level={} action=request outcome={expected}\n",
                    level.as_str()
                )
            );
        }

        let startup = LogRecord::encode(
            false,
            false,
            origin,
            LoggerLevel::Info,
            LoggerEvent::ProcessStartup(ProcessStartupOutcome::Running),
        );
        assert_eq!(
            startup.as_str(),
            "operation=process-startup outcome=running\n"
        );
    }

    fn assert_closed_event(event: LoggerEvent, level: LoggerLevel, body: &str) {
        let origin = LogOrigin::new("crates/runtime/src/logger/event.rs", 77);
        for (show_level, show_origin) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let record = LogRecord::encode(show_level, show_origin, origin, level, event);
            let mut expected = String::new();
            if show_level {
                expected.push_str("level=");
                expected.push_str(level.as_str());
                expected.push(' ');
            }
            if show_origin {
                expected.push_str("origin=crates/runtime/src/logger/event.rs:77 ");
            }
            expected.push_str(body);
            expected.push('\n');

            assert_eq!(record.as_str(), expected);
            assert!(record.as_bytes().len() <= MAX_LOG_RECORD_BYTES);
            assert!(std::str::from_utf8(record.as_bytes()).is_ok());
        }
    }

    fn assert_device_outcome_cases<T: Copy>(
        kind: &str,
        cases: &[(T, LoggerLevel, &str, &str)],
        event: impl Fn(T) -> LoggerEvent,
        actual_level: impl Fn(T) -> LoggerLevel,
    ) {
        for &(outcome, level, operation, result) in cases {
            assert_eq!(actual_level(outcome), level);
            let body = format!("device-kind={kind} operation={operation} outcome={result}");
            assert_closed_event(event(outcome), level, &body);
            for forbidden in [
                "0xfeedface",
                "pfn=",
                "size=",
                "id=",
                "path=",
                "descriptor=",
                "serial-byte=",
                "clock=",
                "generation-id=",
                "vcpu=",
                "provider-error=",
            ] {
                assert!(!body.contains(forbidden));
            }
        }
    }

    #[test]
    fn encodes_every_closed_backend_outcome() {
        let cases = [
            (
                LoggerBackendOutcome::CacheConfigurationFailed,
                LoggerLevel::Error,
                "operation=cache-configuration outcome=failed",
            ),
            (
                LoggerBackendOutcome::MemoryMappingFailed,
                LoggerLevel::Error,
                "operation=memory-map outcome=failed",
            ),
            (
                LoggerBackendOutcome::MemoryDiscardFailed,
                LoggerLevel::Error,
                "operation=memory-discard outcome=failed",
            ),
            (
                LoggerBackendOutcome::VmCreationFailed,
                LoggerLevel::Error,
                "operation=vm-create outcome=failed",
            ),
            (
                LoggerBackendOutcome::VmCleanupFailed,
                LoggerLevel::Error,
                "operation=vm-cleanup outcome=failed",
            ),
            (
                LoggerBackendOutcome::VcpuStartFailed,
                LoggerLevel::Error,
                "operation=vcpu-start outcome=failed",
            ),
            (
                LoggerBackendOutcome::VcpuRunFailed,
                LoggerLevel::Error,
                "operation=vcpu-run outcome=failed",
            ),
            (
                LoggerBackendOutcome::VcpuExitGuestShutdown,
                LoggerLevel::Info,
                "operation=vcpu-exit outcome=guest-shutdown",
            ),
            (
                LoggerBackendOutcome::VcpuExitGuestReset,
                LoggerLevel::Info,
                "operation=vcpu-exit outcome=guest-reset",
            ),
            (
                LoggerBackendOutcome::VcpuExitUnsupported,
                LoggerLevel::Warn,
                "operation=vcpu-exit outcome=unsupported",
            ),
            (
                LoggerBackendOutcome::MmioDispatchFailed,
                LoggerLevel::Error,
                "operation=mmio-dispatch outcome=failed",
            ),
            (
                LoggerBackendOutcome::InterruptDeliveryFailed,
                LoggerLevel::Error,
                "operation=interrupt-delivery outcome=failed",
            ),
            (
                LoggerBackendOutcome::VirtualTimerActivated,
                LoggerLevel::Debug,
                "operation=virtual-timer outcome=activated",
            ),
            (
                LoggerBackendOutcome::VirtualTimerFailed,
                LoggerLevel::Error,
                "operation=virtual-timer outcome=failed",
            ),
        ];

        for (outcome, level, expected) in cases {
            assert_eq!(outcome.level(), level);
            assert_closed_event(LoggerEvent::Backend(outcome), level, expected);
        }
    }

    #[test]
    fn encodes_every_closed_transport_outcome_without_dynamic_values() {
        use LoggerDeviceKind::{
            Balloon, Block, Entropy, MemoryHotplug, Network, Pmem, Serial, Vsock,
        };

        let cases = [
            (
                LoggerTransportOutcome::MmioRegistrationSucceeded,
                LoggerLevel::Debug,
                "operation=mmio-registration outcome=succeeded",
            ),
            (
                LoggerTransportOutcome::MmioRegistrationFailed,
                LoggerLevel::Error,
                "operation=mmio-registration outcome=failed",
            ),
            (
                LoggerTransportOutcome::MmioReleaseSucceeded,
                LoggerLevel::Debug,
                "operation=mmio-release outcome=succeeded",
            ),
            (
                LoggerTransportOutcome::MmioReleaseFailed,
                LoggerLevel::Error,
                "operation=mmio-release outcome=failed",
            ),
            (
                LoggerTransportOutcome::MmioAccessFailed(Some(Block)),
                LoggerLevel::Error,
                "device-kind=block operation=mmio-access outcome=failed",
            ),
            (
                LoggerTransportOutcome::FeatureNegotiationRejected(Network),
                LoggerLevel::Warn,
                "device-kind=network operation=feature-negotiation outcome=rejected",
            ),
            (
                LoggerTransportOutcome::DeviceConfigRejected(Pmem),
                LoggerLevel::Warn,
                "device-kind=pmem operation=device-config outcome=rejected",
            ),
            (
                LoggerTransportOutcome::DeviceConfigFailed(Vsock),
                LoggerLevel::Error,
                "device-kind=vsock operation=device-config outcome=failed",
            ),
            (
                LoggerTransportOutcome::QueueConfigurationRejected(Balloon),
                LoggerLevel::Warn,
                "device-kind=balloon operation=queue-configuration outcome=rejected",
            ),
            (
                LoggerTransportOutcome::QueueNotificationSucceeded(Entropy),
                LoggerLevel::Debug,
                "device-kind=entropy operation=queue-notification outcome=succeeded",
            ),
            (
                LoggerTransportOutcome::QueueNotificationFailed(MemoryHotplug),
                LoggerLevel::Error,
                "device-kind=memory-hotplug operation=queue-notification outcome=failed",
            ),
            (
                LoggerTransportOutcome::UsedRingRejected(None),
                LoggerLevel::Warn,
                "operation=used-ring outcome=rejected",
            ),
            (
                LoggerTransportOutcome::DeviceActivationSucceeded(Block),
                LoggerLevel::Info,
                "device-kind=block operation=device-activation outcome=succeeded",
            ),
            (
                LoggerTransportOutcome::DeviceActivationFailed(Network),
                LoggerLevel::Error,
                "device-kind=network operation=device-activation outcome=failed",
            ),
            (
                LoggerTransportOutcome::DeviceResetSucceeded(Pmem),
                LoggerLevel::Info,
                "device-kind=pmem operation=device-reset outcome=succeeded",
            ),
            (
                LoggerTransportOutcome::DeviceResetUnsupported(Vsock),
                LoggerLevel::Warn,
                "device-kind=vsock operation=device-reset outcome=unsupported",
            ),
            (
                LoggerTransportOutcome::DeviceResetFailed(Balloon),
                LoggerLevel::Error,
                "device-kind=balloon operation=device-reset outcome=failed",
            ),
            (
                LoggerTransportOutcome::PciFunctionPublished(Entropy),
                LoggerLevel::Info,
                "device-kind=entropy operation=pci-publication outcome=published",
            ),
            (
                LoggerTransportOutcome::PciFunctionPublicationFailed(MemoryHotplug),
                LoggerLevel::Error,
                "device-kind=memory-hotplug operation=pci-publication outcome=failed",
            ),
            (
                LoggerTransportOutcome::PciFunctionRemoved(Serial),
                LoggerLevel::Info,
                "device-kind=serial operation=pci-removal outcome=removed",
            ),
            (
                LoggerTransportOutcome::PciFunctionRemovalFailed(Block),
                LoggerLevel::Error,
                "device-kind=block operation=pci-removal outcome=failed",
            ),
            (
                LoggerTransportOutcome::PciConfigRejected(Network),
                LoggerLevel::Warn,
                "device-kind=network operation=pci-config outcome=rejected",
            ),
            (
                LoggerTransportOutcome::MsiConfigurationSucceeded(Vsock),
                LoggerLevel::Debug,
                "device-kind=vsock operation=msi-configuration outcome=succeeded",
            ),
            (
                LoggerTransportOutcome::MsiConfigurationFailed(Balloon),
                LoggerLevel::Error,
                "device-kind=balloon operation=msi-configuration outcome=failed",
            ),
            (
                LoggerTransportOutcome::InterruptDelivered(Entropy),
                LoggerLevel::Debug,
                "device-kind=entropy operation=interrupt-delivery outcome=delivered",
            ),
            (
                LoggerTransportOutcome::InterruptDeliveryFailed(MemoryHotplug),
                LoggerLevel::Error,
                "device-kind=memory-hotplug operation=interrupt-delivery outcome=failed",
            ),
            (
                LoggerTransportOutcome::PublicationRollbackSucceeded(Some(Serial)),
                LoggerLevel::Warn,
                "device-kind=serial operation=publication-rollback outcome=succeeded",
            ),
            (
                LoggerTransportOutcome::PublicationRollbackFailed(Some(Block)),
                LoggerLevel::Error,
                "device-kind=block operation=publication-rollback outcome=failed",
            ),
            (
                LoggerTransportOutcome::RateLimiterRejected(Network),
                LoggerLevel::Debug,
                "device-kind=network operation=rate-limiter outcome=rejected",
            ),
        ];

        for (outcome, level, expected) in cases {
            assert_eq!(outcome.level(), level);
            assert_closed_event(LoggerEvent::Transport(outcome), level, expected);
            for forbidden in [
                "/private/forbidden",
                "device-secret",
                "0xfeedface",
                "queue-index=7",
                "descriptor=11",
                "guest-bytes",
            ] {
                assert!(!expected.contains(forbidden));
            }
        }
    }

    #[test]
    fn encodes_every_closed_balloon_outcome() {
        use LoggerBalloonOutcome as Outcome;
        let cases = [
            (
                Outcome::InflateSucceeded,
                LoggerLevel::Info,
                "inflate",
                "succeeded",
            ),
            (
                Outcome::DeflateSucceeded,
                LoggerLevel::Info,
                "deflate",
                "succeeded",
            ),
            (
                Outcome::StatisticsUpdated,
                LoggerLevel::Info,
                "statistics",
                "updated",
            ),
            (
                Outcome::StatisticsOversized,
                LoggerLevel::Warn,
                "statistics",
                "oversized",
            ),
            (
                Outcome::StatisticsFailed,
                LoggerLevel::Error,
                "statistics",
                "failed",
            ),
            (
                Outcome::HintingSucceeded,
                LoggerLevel::Info,
                "hinting",
                "succeeded",
            ),
            (
                Outcome::HintingFailed,
                LoggerLevel::Error,
                "hinting",
                "failed",
            ),
            (
                Outcome::ReportingSucceeded,
                LoggerLevel::Info,
                "reporting",
                "succeeded",
            ),
            (
                Outcome::ReportingFailed,
                LoggerLevel::Error,
                "reporting",
                "failed",
            ),
            (
                Outcome::MemoryDiscardFailed,
                LoggerLevel::Error,
                "memory-discard",
                "failed",
            ),
            (
                Outcome::AccountingFailed,
                LoggerLevel::Error,
                "accounting",
                "failed",
            ),
            (
                Outcome::QueueDispatchFailed,
                LoggerLevel::Error,
                "queue-dispatch",
                "failed",
            ),
            (
                Outcome::QueueNotificationInactive,
                LoggerLevel::Error,
                "queue-notification",
                "inactive",
            ),
            (
                Outcome::QueueNotificationUnsupported,
                LoggerLevel::Warn,
                "queue-notification",
                "unsupported",
            ),
            (
                Outcome::InterruptDeliveryFailed,
                LoggerLevel::Error,
                "interrupt-delivery",
                "failed",
            ),
        ];
        assert_device_outcome_cases(
            "balloon",
            &cases,
            LoggerEvent::Balloon,
            LoggerBalloonOutcome::level,
        );
    }

    #[test]
    fn encodes_every_closed_memory_hotplug_outcome() {
        use LoggerMemoryHotplugOutcome as Outcome;
        let cases = [
            (
                Outcome::RequestSucceeded,
                LoggerLevel::Info,
                "request",
                "succeeded",
            ),
            (
                Outcome::RequestUnsupported,
                LoggerLevel::Warn,
                "request",
                "unsupported",
            ),
            (
                Outcome::StateQuerySucceeded,
                LoggerLevel::Info,
                "state-query",
                "succeeded",
            ),
            (
                Outcome::PolicyRejected,
                LoggerLevel::Warn,
                "policy",
                "rejected",
            ),
            (
                Outcome::MutationFailed,
                LoggerLevel::Error,
                "mutation",
                "failed",
            ),
            (
                Outcome::MutationRollbackSucceeded,
                LoggerLevel::Warn,
                "mutation-rollback",
                "succeeded",
            ),
            (
                Outcome::MutationRollbackFailed,
                LoggerLevel::Error,
                "mutation-rollback",
                "failed",
            ),
            (
                Outcome::RequestParseFailed,
                LoggerLevel::Error,
                "request-parse",
                "failed",
            ),
            (
                Outcome::ResponseWriteFailed,
                LoggerLevel::Error,
                "response-write",
                "failed",
            ),
            (
                Outcome::MemoryDiscardFailed,
                LoggerLevel::Error,
                "memory-discard",
                "failed",
            ),
            (
                Outcome::ConfigurationUpdateSucceeded,
                LoggerLevel::Info,
                "configuration-update",
                "succeeded",
            ),
            (
                Outcome::ConfigurationUpdateFailed,
                LoggerLevel::Error,
                "configuration-update",
                "failed",
            ),
            (
                Outcome::QueueDispatchFailed,
                LoggerLevel::Error,
                "queue-dispatch",
                "failed",
            ),
            (
                Outcome::QueueNotificationInactive,
                LoggerLevel::Error,
                "queue-notification",
                "inactive",
            ),
            (
                Outcome::QueueNotificationUnsupported,
                LoggerLevel::Warn,
                "queue-notification",
                "unsupported",
            ),
            (
                Outcome::InterruptDeliveryFailed,
                LoggerLevel::Error,
                "interrupt-delivery",
                "failed",
            ),
        ];
        assert_device_outcome_cases(
            "memory-hotplug",
            &cases,
            LoggerEvent::MemoryHotplug,
            LoggerMemoryHotplugOutcome::level,
        );
    }

    #[test]
    fn encodes_every_closed_entropy_outcome() {
        use LoggerEntropyOutcome as Outcome;
        let cases = [
            (
                Outcome::FillSucceeded,
                LoggerLevel::Info,
                "fill",
                "succeeded",
            ),
            (Outcome::FillFailed, LoggerLevel::Error, "fill", "failed"),
            (
                Outcome::RequestParseFailed,
                LoggerLevel::Error,
                "request-parse",
                "failed",
            ),
            (
                Outcome::BufferWriteFailed,
                LoggerLevel::Error,
                "buffer-write",
                "failed",
            ),
            (
                Outcome::QueueDispatchFailed,
                LoggerLevel::Error,
                "queue-dispatch",
                "failed",
            ),
            (
                Outcome::QueueNotificationInactive,
                LoggerLevel::Error,
                "queue-notification",
                "inactive",
            ),
            (
                Outcome::QueueNotificationUnsupported,
                LoggerLevel::Warn,
                "queue-notification",
                "unsupported",
            ),
            (
                Outcome::RateLimiterThrottled,
                LoggerLevel::Debug,
                "rate-limiter",
                "throttled",
            ),
            (
                Outcome::RateLimiterResumed,
                LoggerLevel::Debug,
                "rate-limiter",
                "resumed",
            ),
            (
                Outcome::EntropyProviderFailed,
                LoggerLevel::Error,
                "entropy-provider",
                "failed",
            ),
            (
                Outcome::InterruptDeliveryFailed,
                LoggerLevel::Error,
                "interrupt-delivery",
                "failed",
            ),
        ];
        assert_device_outcome_cases(
            "entropy",
            &cases,
            LoggerEvent::Entropy,
            LoggerEntropyOutcome::level,
        );
    }

    #[test]
    fn encodes_every_closed_serial_outcome() {
        use LoggerSerialOutcome as Outcome;
        let cases = [
            (
                Outcome::InputReadSucceeded,
                LoggerLevel::Info,
                "input-read",
                "succeeded",
            ),
            (
                Outcome::InputReadFailed,
                LoggerLevel::Error,
                "input-read",
                "failed",
            ),
            (
                Outcome::InputRearmSucceeded,
                LoggerLevel::Debug,
                "input-rearm",
                "succeeded",
            ),
            (
                Outcome::InputBackpressurePaused,
                LoggerLevel::Debug,
                "input-backpressure",
                "paused",
            ),
            (
                Outcome::InputDetachEof,
                LoggerLevel::Info,
                "input-detach",
                "eof",
            ),
            (
                Outcome::InputDetachFailed,
                LoggerLevel::Error,
                "input-detach",
                "failed",
            ),
            (
                Outcome::OutputFailed,
                LoggerLevel::Error,
                "output",
                "failed",
            ),
            (
                Outcome::RateLimiterThrottled,
                LoggerLevel::Debug,
                "rate-limiter",
                "throttled",
            ),
            (
                Outcome::InterruptDeliverySucceeded,
                LoggerLevel::Info,
                "interrupt-delivery",
                "succeeded",
            ),
            (
                Outcome::InterruptDeliveryFailed,
                LoggerLevel::Error,
                "interrupt-delivery",
                "failed",
            ),
        ];
        assert_device_outcome_cases(
            "serial",
            &cases,
            LoggerEvent::Serial,
            LoggerSerialOutcome::level,
        );
    }

    #[test]
    fn encodes_every_closed_time_identity_outcome() {
        use LoggerTimeIdentityOutcome as Outcome;
        let cases = [
            (
                Outcome::RtcReadRejected,
                LoggerLevel::Warn,
                "rtc-read",
                "rejected",
            ),
            (
                Outcome::RtcWriteRejected,
                LoggerLevel::Warn,
                "rtc-write",
                "rejected",
            ),
            (
                Outcome::RtcRestoreSucceeded,
                LoggerLevel::Info,
                "rtc-restore",
                "succeeded",
            ),
            (
                Outcome::RtcRestoreFailed,
                LoggerLevel::Error,
                "rtc-restore",
                "failed",
            ),
            (
                Outcome::PlatformPublicationSucceeded,
                LoggerLevel::Info,
                "platform-publication",
                "succeeded",
            ),
            (
                Outcome::PlatformPublicationFailed,
                LoggerLevel::Error,
                "platform-publication",
                "failed",
            ),
            (
                Outcome::VmGenIdReplacementSucceeded,
                LoggerLevel::Info,
                "vmgenid-replacement",
                "succeeded",
            ),
            (
                Outcome::VmGenIdReplacementFailed,
                LoggerLevel::Error,
                "vmgenid-replacement",
                "failed",
            ),
            (
                Outcome::VmGenIdNotificationSucceeded,
                LoggerLevel::Info,
                "vmgenid-notification",
                "succeeded",
            ),
            (
                Outcome::VmGenIdNotificationFailed,
                LoggerLevel::Error,
                "vmgenid-notification",
                "failed",
            ),
            (
                Outcome::VmClockUpdateSucceeded,
                LoggerLevel::Info,
                "vmclock-update",
                "succeeded",
            ),
            (
                Outcome::VmClockUpdateFailed,
                LoggerLevel::Error,
                "vmclock-update",
                "failed",
            ),
            (
                Outcome::VmClockUpdatePartiallyCommitted,
                LoggerLevel::Error,
                "vmclock-update",
                "partially-committed",
            ),
            (
                Outcome::VmClockNotificationSucceeded,
                LoggerLevel::Info,
                "vmclock-notification",
                "succeeded",
            ),
            (
                Outcome::VmClockNotificationFailed,
                LoggerLevel::Error,
                "vmclock-notification",
                "failed",
            ),
            (
                Outcome::OrderedRestoreSucceeded,
                LoggerLevel::Info,
                "ordered-restore",
                "succeeded",
            ),
            (
                Outcome::OrderedRestoreFailed,
                LoggerLevel::Error,
                "ordered-restore",
                "failed",
            ),
            (
                Outcome::OrderedRestorePartiallyCommitted,
                LoggerLevel::Error,
                "ordered-restore",
                "partially-committed",
            ),
            (
                Outcome::PvTimeInitializationSucceeded,
                LoggerLevel::Info,
                "pvtime-initialization",
                "succeeded",
            ),
            (
                Outcome::PvTimeInitializationFailed,
                LoggerLevel::Error,
                "pvtime-initialization",
                "failed",
            ),
            (
                Outcome::PvTimeAccountingPublished,
                LoggerLevel::Info,
                "pvtime-accounting",
                "published",
            ),
            (
                Outcome::PvTimeAccountingDiscarded,
                LoggerLevel::Debug,
                "pvtime-accounting",
                "discarded",
            ),
            (
                Outcome::PvTimeAccountingFailed,
                LoggerLevel::Error,
                "pvtime-accounting",
                "failed",
            ),
        ];
        assert_device_outcome_cases(
            "time-identity",
            &cases,
            LoggerEvent::TimeIdentity,
            LoggerTimeIdentityOutcome::level,
        );
    }

    #[test]
    fn encodes_every_closed_block_outcome() {
        let cases = [
            (
                LoggerBlockOutcome::RequestSucceeded,
                LoggerLevel::Info,
                "request",
                "succeeded",
            ),
            (
                LoggerBlockOutcome::RequestUnsupported,
                LoggerLevel::Warn,
                "request",
                "unsupported",
            ),
            (
                LoggerBlockOutcome::RequestParseFailed,
                LoggerLevel::Error,
                "request-parse",
                "failed",
            ),
            (
                LoggerBlockOutcome::RequestIoFailed,
                LoggerLevel::Error,
                "request-io",
                "failed",
            ),
            (
                LoggerBlockOutcome::StatusWriteFailed,
                LoggerLevel::Error,
                "status-write",
                "failed",
            ),
            (
                LoggerBlockOutcome::QueueDispatchFailed,
                LoggerLevel::Error,
                "queue-dispatch",
                "failed",
            ),
            (
                LoggerBlockOutcome::QueueNotificationInactive,
                LoggerLevel::Error,
                "queue-notification",
                "inactive",
            ),
            (
                LoggerBlockOutcome::QueueNotificationUnsupported,
                LoggerLevel::Warn,
                "queue-notification",
                "unsupported",
            ),
            (
                LoggerBlockOutcome::RateLimiterThrottled,
                LoggerLevel::Debug,
                "rate-limiter",
                "throttled",
            ),
            (
                LoggerBlockOutcome::RateLimiterResumed,
                LoggerLevel::Debug,
                "rate-limiter",
                "resumed",
            ),
            (
                LoggerBlockOutcome::AsyncEngineThrottled,
                LoggerLevel::Debug,
                "async-engine",
                "throttled",
            ),
            (
                LoggerBlockOutcome::AsyncEngineFailed,
                LoggerLevel::Error,
                "async-engine",
                "failed",
            ),
            (
                LoggerBlockOutcome::VhostUserNotificationSucceeded,
                LoggerLevel::Info,
                "vhost-user-notification",
                "succeeded",
            ),
            (
                LoggerBlockOutcome::VhostUserNotificationFailed,
                LoggerLevel::Error,
                "vhost-user-notification",
                "failed",
            ),
            (
                LoggerBlockOutcome::VhostUserDisconnected,
                LoggerLevel::Warn,
                "vhost-user-notification",
                "disconnected",
            ),
            (
                LoggerBlockOutcome::VhostUserTerminal,
                LoggerLevel::Error,
                "vhost-user-notification",
                "terminal",
            ),
            (
                LoggerBlockOutcome::VhostUserConfigSucceeded,
                LoggerLevel::Info,
                "vhost-user-config",
                "succeeded",
            ),
            (
                LoggerBlockOutcome::VhostUserConfigFailed,
                LoggerLevel::Error,
                "vhost-user-config",
                "failed",
            ),
            (
                LoggerBlockOutcome::InterruptDeliveryFailed,
                LoggerLevel::Error,
                "interrupt-delivery",
                "failed",
            ),
        ];
        for (outcome, level, operation, result) in cases {
            assert_eq!(outcome.level(), level);
            assert_closed_event(
                LoggerEvent::Block(outcome),
                level,
                &format!("device-kind=block operation={operation} outcome={result}"),
            );
        }
    }

    #[test]
    fn encodes_every_closed_pmem_outcome() {
        let cases = [
            (
                LoggerPmemOutcome::FlushSucceeded,
                LoggerLevel::Info,
                "flush",
                "succeeded",
            ),
            (
                LoggerPmemOutcome::FlushFailed,
                LoggerLevel::Error,
                "flush",
                "failed",
            ),
            (
                LoggerPmemOutcome::RequestParseFailed,
                LoggerLevel::Error,
                "request-parse",
                "failed",
            ),
            (
                LoggerPmemOutcome::StatusWriteFailed,
                LoggerLevel::Error,
                "status-write",
                "failed",
            ),
            (
                LoggerPmemOutcome::QueueDispatchFailed,
                LoggerLevel::Error,
                "queue-dispatch",
                "failed",
            ),
            (
                LoggerPmemOutcome::QueueNotificationInactive,
                LoggerLevel::Error,
                "queue-notification",
                "inactive",
            ),
            (
                LoggerPmemOutcome::QueueNotificationUnsupported,
                LoggerLevel::Warn,
                "queue-notification",
                "unsupported",
            ),
            (
                LoggerPmemOutcome::RateLimiterThrottled,
                LoggerLevel::Debug,
                "rate-limiter",
                "throttled",
            ),
            (
                LoggerPmemOutcome::RateLimiterResumed,
                LoggerLevel::Debug,
                "rate-limiter",
                "resumed",
            ),
            (
                LoggerPmemOutcome::InterruptDeliveryFailed,
                LoggerLevel::Error,
                "interrupt-delivery",
                "failed",
            ),
        ];
        for (outcome, level, operation, result) in cases {
            assert_eq!(outcome.level(), level);
            assert_closed_event(
                LoggerEvent::Pmem(outcome),
                level,
                &format!("device-kind=pmem operation={operation} outcome={result}"),
            );
        }
    }

    #[test]
    fn encodes_every_closed_network_outcome() {
        let cases = [
            (
                LoggerNetworkOutcome::RxSucceeded,
                LoggerLevel::Info,
                "rx",
                "succeeded",
            ),
            (
                LoggerNetworkOutcome::RxBufferMalformed,
                LoggerLevel::Error,
                "rx-buffer",
                "malformed",
            ),
            (
                LoggerNetworkOutcome::RxBufferTooSmall,
                LoggerLevel::Warn,
                "rx-buffer",
                "too-small",
            ),
            (
                LoggerNetworkOutcome::RxBufferUnavailable,
                LoggerLevel::Debug,
                "rx-buffer",
                "unavailable",
            ),
            (
                LoggerNetworkOutcome::RxProviderFailed,
                LoggerLevel::Error,
                "rx-provider",
                "failed",
            ),
            (
                LoggerNetworkOutcome::TxSucceeded,
                LoggerLevel::Info,
                "tx",
                "succeeded",
            ),
            (
                LoggerNetworkOutcome::TxFrameMalformed,
                LoggerLevel::Error,
                "tx-frame",
                "malformed",
            ),
            (
                LoggerNetworkOutcome::TxSpoofRejected,
                LoggerLevel::Warn,
                "tx-frame",
                "spoof-rejected",
            ),
            (
                LoggerNetworkOutcome::TxProviderFailed,
                LoggerLevel::Error,
                "tx-provider",
                "failed",
            ),
            (
                LoggerNetworkOutcome::QueueDispatchFailed,
                LoggerLevel::Error,
                "queue-dispatch",
                "failed",
            ),
            (
                LoggerNetworkOutcome::QueueNotificationInactive,
                LoggerLevel::Error,
                "queue-notification",
                "inactive",
            ),
            (
                LoggerNetworkOutcome::QueueNotificationUnsupported,
                LoggerLevel::Warn,
                "queue-notification",
                "unsupported",
            ),
            (
                LoggerNetworkOutcome::RateLimiterThrottled,
                LoggerLevel::Debug,
                "rate-limiter",
                "throttled",
            ),
            (
                LoggerNetworkOutcome::RateLimiterResumed,
                LoggerLevel::Debug,
                "rate-limiter",
                "resumed",
            ),
            (
                LoggerNetworkOutcome::PacketProviderFailed,
                LoggerLevel::Error,
                "packet-provider",
                "failed",
            ),
            (
                LoggerNetworkOutcome::PacketProviderPartial,
                LoggerLevel::Warn,
                "packet-provider",
                "partial",
            ),
            (
                LoggerNetworkOutcome::MmdsRequestDetoured,
                LoggerLevel::Info,
                "mmds-request",
                "detoured",
            ),
            (
                LoggerNetworkOutcome::MmdsTokenKeyRotated,
                LoggerLevel::Info,
                "mmds-token-key",
                "rotated",
            ),
            (
                LoggerNetworkOutcome::MmdsTokenKeyRotationFailed,
                LoggerLevel::Error,
                "mmds-token-key",
                "failed",
            ),
            (
                LoggerNetworkOutcome::InterruptDeliveryFailed,
                LoggerLevel::Error,
                "interrupt-delivery",
                "failed",
            ),
        ];
        for (outcome, level, operation, result) in cases {
            assert_eq!(outcome.level(), level);
            assert_closed_event(
                LoggerEvent::Network(outcome),
                level,
                &format!("device-kind=network operation={operation} outcome={result}"),
            );
        }
    }

    #[test]
    fn encodes_every_closed_vsock_outcome() {
        let cases = [
            (
                LoggerVsockOutcome::RxSucceeded,
                LoggerLevel::Info,
                "rx",
                "succeeded",
            ),
            (
                LoggerVsockOutcome::RxBufferMalformed,
                LoggerLevel::Error,
                "rx-buffer",
                "malformed",
            ),
            (
                LoggerVsockOutcome::RxBufferTooSmall,
                LoggerLevel::Warn,
                "rx-buffer",
                "too-small",
            ),
            (
                LoggerVsockOutcome::TxSucceeded,
                LoggerLevel::Info,
                "tx",
                "succeeded",
            ),
            (
                LoggerVsockOutcome::TxPacketMalformed,
                LoggerLevel::Error,
                "tx-packet",
                "malformed",
            ),
            (
                LoggerVsockOutcome::QueueDispatchFailed,
                LoggerLevel::Error,
                "queue-dispatch",
                "failed",
            ),
            (
                LoggerVsockOutcome::QueueNotificationInactive,
                LoggerLevel::Error,
                "queue-notification",
                "inactive",
            ),
            (
                LoggerVsockOutcome::QueueNotificationUnsupported,
                LoggerLevel::Warn,
                "queue-notification",
                "unsupported",
            ),
            (
                LoggerVsockOutcome::HostConnectionAccepted,
                LoggerLevel::Info,
                "host-connection",
                "accepted",
            ),
            (
                LoggerVsockOutcome::HostConnectionCompleted,
                LoggerLevel::Info,
                "host-connection",
                "completed",
            ),
            (
                LoggerVsockOutcome::HostConnectionPending,
                LoggerLevel::Debug,
                "host-connection",
                "pending",
            ),
            (
                LoggerVsockOutcome::HostConnectionDropped,
                LoggerLevel::Warn,
                "host-connection",
                "dropped",
            ),
            (
                LoggerVsockOutcome::GuestConnectionRetained,
                LoggerLevel::Info,
                "guest-connection",
                "retained",
            ),
            (
                LoggerVsockOutcome::GuestConnectionForwarded,
                LoggerLevel::Info,
                "guest-connection",
                "forwarded",
            ),
            (
                LoggerVsockOutcome::GuestConnectionUpdated,
                LoggerLevel::Info,
                "guest-connection",
                "updated",
            ),
            (
                LoggerVsockOutcome::GuestConnectionClosed,
                LoggerLevel::Info,
                "guest-connection",
                "closed",
            ),
            (
                LoggerVsockOutcome::GuestConnectionIgnored,
                LoggerLevel::Debug,
                "guest-connection",
                "ignored",
            ),
            (
                LoggerVsockOutcome::GuestConnectionDropped,
                LoggerLevel::Warn,
                "guest-connection",
                "dropped",
            ),
            (
                LoggerVsockOutcome::ConnectionResetQueued,
                LoggerLevel::Info,
                "connection-reset",
                "queued",
            ),
            (
                LoggerVsockOutcome::ConnectionResetDropped,
                LoggerLevel::Warn,
                "connection-reset",
                "dropped",
            ),
            (
                LoggerVsockOutcome::TransportResetSucceeded,
                LoggerLevel::Info,
                "transport-reset",
                "succeeded",
            ),
            (
                LoggerVsockOutcome::TransportResetFailed,
                LoggerLevel::Error,
                "transport-reset",
                "failed",
            ),
            (
                LoggerVsockOutcome::InterruptDeliveryFailed,
                LoggerLevel::Error,
                "interrupt-delivery",
                "failed",
            ),
        ];
        for (outcome, level, operation, result) in cases {
            assert_eq!(outcome.level(), level);
            assert_closed_event(
                LoggerEvent::Vsock(outcome),
                level,
                &format!("device-kind=vsock operation={operation} outcome={result}"),
            );
        }
    }

    #[test]
    fn encodes_every_closed_host_worker_snapshot_and_signal_outcome() {
        let lifecycle_cases = [
            (
                LoggerLifecycleOutcome::BackendStartupSucceeded,
                LoggerLevel::Info,
                "operation=backend-startup outcome=succeeded",
            ),
            (
                LoggerLifecycleOutcome::BackendStartupFailed,
                LoggerLevel::Error,
                "operation=backend-startup outcome=failed",
            ),
            (
                LoggerLifecycleOutcome::VmStartSucceeded,
                LoggerLevel::Info,
                "operation=vm-start outcome=succeeded",
            ),
            (
                LoggerLifecycleOutcome::VmStartRejected,
                LoggerLevel::Error,
                "operation=vm-start outcome=rejected",
            ),
            (
                LoggerLifecycleOutcome::VmStartFailed,
                LoggerLevel::Error,
                "operation=vm-start outcome=failed",
            ),
            (
                LoggerLifecycleOutcome::VmPauseSucceeded,
                LoggerLevel::Info,
                "operation=vm-pause outcome=succeeded",
            ),
            (
                LoggerLifecycleOutcome::VmPauseUnchanged,
                LoggerLevel::Debug,
                "operation=vm-pause outcome=unchanged",
            ),
            (
                LoggerLifecycleOutcome::VmPauseRejected,
                LoggerLevel::Error,
                "operation=vm-pause outcome=rejected",
            ),
            (
                LoggerLifecycleOutcome::VmPauseFailed,
                LoggerLevel::Error,
                "operation=vm-pause outcome=failed",
            ),
            (
                LoggerLifecycleOutcome::VmResumeSucceeded,
                LoggerLevel::Info,
                "operation=vm-resume outcome=succeeded",
            ),
            (
                LoggerLifecycleOutcome::VmResumeUnchanged,
                LoggerLevel::Debug,
                "operation=vm-resume outcome=unchanged",
            ),
            (
                LoggerLifecycleOutcome::VmResumeRejected,
                LoggerLevel::Error,
                "operation=vm-resume outcome=rejected",
            ),
            (
                LoggerLifecycleOutcome::VmResumeFailed,
                LoggerLevel::Error,
                "operation=vm-resume outcome=failed",
            ),
            (
                LoggerLifecycleOutcome::VmStopSucceeded,
                LoggerLevel::Info,
                "operation=vm-stop outcome=succeeded",
            ),
            (
                LoggerLifecycleOutcome::VmStopFailed,
                LoggerLevel::Error,
                "operation=vm-stop outcome=failed",
            ),
        ];
        for (outcome, level, expected) in lifecycle_cases {
            assert_eq!(outcome.level(), level);
            assert_closed_event(LoggerEvent::Lifecycle(outcome), level, expected);
        }

        for (kind, kind_text) in [
            (LoggerDeviceKind::Block, "block"),
            (LoggerDeviceKind::Network, "network"),
            (LoggerDeviceKind::Pmem, "pmem"),
        ] {
            for (outcome, operation, outcome_text, level) in [
                (
                    LoggerLifecycleOutcome::DeviceAttachSucceeded(kind),
                    "device-attach",
                    "succeeded",
                    LoggerLevel::Info,
                ),
                (
                    LoggerLifecycleOutcome::DeviceAttachRejected(kind),
                    "device-attach",
                    "rejected",
                    LoggerLevel::Error,
                ),
                (
                    LoggerLifecycleOutcome::DeviceAttachFailed(kind),
                    "device-attach",
                    "failed",
                    LoggerLevel::Error,
                ),
                (
                    LoggerLifecycleOutcome::DeviceUpdateSucceeded(kind),
                    "device-update",
                    "succeeded",
                    LoggerLevel::Info,
                ),
                (
                    LoggerLifecycleOutcome::DeviceUpdateRejected(kind),
                    "device-update",
                    "rejected",
                    LoggerLevel::Error,
                ),
                (
                    LoggerLifecycleOutcome::DeviceUpdateFailed(kind),
                    "device-update",
                    "failed",
                    LoggerLevel::Error,
                ),
                (
                    LoggerLifecycleOutcome::DeviceDetachSucceeded(kind),
                    "device-detach",
                    "succeeded",
                    LoggerLevel::Info,
                ),
                (
                    LoggerLifecycleOutcome::DeviceDetachRejected(kind),
                    "device-detach",
                    "rejected",
                    LoggerLevel::Error,
                ),
                (
                    LoggerLifecycleOutcome::DeviceDetachFailed(kind),
                    "device-detach",
                    "failed",
                    LoggerLevel::Error,
                ),
            ] {
                assert_eq!(outcome.level(), level);
                assert_closed_event(
                    LoggerEvent::Lifecycle(outcome),
                    level,
                    &format!(
                        "device-kind={kind_text} operation={operation} outcome={outcome_text}"
                    ),
                );
            }
        }

        for (outcome, level, expected) in [
            (
                LoggerSnapshotOutcome::CreateSucceeded,
                LoggerLevel::Info,
                "operation=snapshot-create outcome=succeeded",
            ),
            (
                LoggerSnapshotOutcome::CreateRejected,
                LoggerLevel::Error,
                "operation=snapshot-create outcome=rejected",
            ),
            (
                LoggerSnapshotOutcome::CreateFailed,
                LoggerLevel::Error,
                "operation=snapshot-create outcome=failed",
            ),
            (
                LoggerSnapshotOutcome::CreateCancelled,
                LoggerLevel::Warn,
                "operation=snapshot-create outcome=cancelled",
            ),
            (
                LoggerSnapshotOutcome::LoadSucceeded,
                LoggerLevel::Info,
                "operation=snapshot-load outcome=succeeded",
            ),
            (
                LoggerSnapshotOutcome::LoadRejected,
                LoggerLevel::Error,
                "operation=snapshot-load outcome=rejected",
            ),
            (
                LoggerSnapshotOutcome::LoadFailed,
                LoggerLevel::Error,
                "operation=snapshot-load outcome=failed",
            ),
            (
                LoggerSnapshotOutcome::LoadCancelled,
                LoggerLevel::Warn,
                "operation=snapshot-load outcome=cancelled",
            ),
        ] {
            assert_eq!(outcome.level(), level);
            assert_closed_event(LoggerEvent::Snapshot(outcome), level, expected);
        }

        for (outcome, level, expected) in [
            (
                LoggerApiWorkerOutcome::Running,
                LoggerLevel::Info,
                "operation=boot-worker outcome=running",
            ),
            (
                LoggerApiWorkerOutcome::Exited,
                LoggerLevel::Info,
                "operation=boot-worker outcome=exited",
            ),
            (
                LoggerApiWorkerOutcome::Stopped,
                LoggerLevel::Debug,
                "operation=boot-worker outcome=stopped",
            ),
            (
                LoggerApiWorkerOutcome::Failed,
                LoggerLevel::Error,
                "operation=boot-worker outcome=failed",
            ),
        ] {
            assert_eq!(outcome.level(), level);
            assert_closed_event(LoggerEvent::ApiWorker(outcome), level, expected);
        }

        let observability = LoggerObservabilityOutcome::MetricsWorkerFailed;
        assert_eq!(observability.level(), LoggerLevel::Error);
        assert_closed_event(
            LoggerEvent::Observability(observability),
            LoggerLevel::Error,
            "operation=metrics-worker outcome=failed",
        );

        for (outcome, level, expected) in [
            (
                LoggerProcessSignalOutcome::HostSignalReceived,
                LoggerLevel::Info,
                "operation=host-signal outcome=received",
            ),
            (
                LoggerProcessSignalOutcome::CancellationRequested,
                LoggerLevel::Info,
                "operation=cancellation outcome=requested",
            ),
            (
                LoggerProcessSignalOutcome::GuestPoweroff,
                LoggerLevel::Info,
                "operation=guest-power outcome=poweroff",
            ),
            (
                LoggerProcessSignalOutcome::GuestReset,
                LoggerLevel::Info,
                "operation=guest-power outcome=reset",
            ),
            (
                LoggerProcessSignalOutcome::ShutdownOrderly,
                LoggerLevel::Info,
                "operation=shutdown outcome=orderly",
            ),
            (
                LoggerProcessSignalOutcome::ShutdownAbnormal,
                LoggerLevel::Error,
                "operation=shutdown outcome=abnormal",
            ),
        ] {
            assert_eq!(outcome.level(), level);
            assert_closed_event(LoggerEvent::ProcessSignal(outcome), level, expected);
        }
    }

    #[test]
    fn encodes_fixed_process_records_and_levels() {
        let origin = LogOrigin::new("crates/bangbang/src/process.rs", 17);
        let cases = [
            (
                ProcessTerminalCategory::Success,
                LoggerLevel::Info,
                "success",
            ),
            (
                ProcessTerminalCategory::Configuration,
                LoggerLevel::Error,
                "configuration",
            ),
            (
                ProcessTerminalCategory::ProcessFailure,
                LoggerLevel::Error,
                "process-failure",
            ),
            (
                ProcessTerminalCategory::Cancelled,
                LoggerLevel::Info,
                "cancelled",
            ),
            (ProcessTerminalCategory::Panic, LoggerLevel::Error, "panic"),
        ];

        for (category, level, text) in cases {
            let record = LogRecord::encode(
                true,
                false,
                origin,
                category.level(),
                LoggerEvent::ProcessExit(category),
            );
            assert_eq!(category.level(), level);
            assert_eq!(
                record.as_str(),
                format!(
                    "level={} event=process-exit category={text}\n",
                    level.as_str()
                )
            );
        }
    }

    #[test]
    fn panic_record_set_preencodes_every_prefix_without_payload_input() {
        let records = PanicLogRecords::new();
        assert_eq!(records.plain.as_str(), "event=process-panic\n");
        assert_eq!(records.level.as_str(), "level=Error event=process-panic\n");
        assert!(records.origin.as_str().starts_with("origin="));
        assert!(records.origin.as_str().ends_with(" event=process-panic\n"));
        assert!(
            records
                .level_and_origin
                .as_str()
                .starts_with("level=Error origin=")
        );
        assert!(
            records
                .level_and_origin
                .as_str()
                .ends_with(" event=process-panic\n")
        );
        assert_eq!(records.plain_bytes(), b"event=process-panic\n");
        for record in [
            &records.plain,
            &records.level,
            &records.origin,
            &records.level_and_origin,
        ] {
            assert!(record.as_bytes().len() <= MAX_LOG_RECORD_BYTES);
            assert!(std::str::from_utf8(record.as_bytes()).is_ok());
        }
    }

    #[test]
    fn every_route_has_its_reviewed_fixed_template() {
        let cases = [
            (LoggerApiRoute::Root, "/"),
            (LoggerApiRoute::Actions, "/actions"),
            (LoggerApiRoute::Balloon, "/balloon"),
            (
                LoggerApiRoute::BalloonHintingStart,
                "/balloon/hinting/start",
            ),
            (
                LoggerApiRoute::BalloonHintingStatus,
                "/balloon/hinting/status",
            ),
            (LoggerApiRoute::BalloonHintingStop, "/balloon/hinting/stop"),
            (LoggerApiRoute::BalloonStatistics, "/balloon/statistics"),
            (LoggerApiRoute::BootSource, "/boot-source"),
            (LoggerApiRoute::CpuConfig, "/cpu-config"),
            (LoggerApiRoute::Drive, "/drives/{drive_id}"),
            (LoggerApiRoute::Entropy, "/entropy"),
            (LoggerApiRoute::Logger, "/logger"),
            (LoggerApiRoute::MachineConfig, "/machine-config"),
            (LoggerApiRoute::MemoryHotplug, "/hotplug/memory"),
            (LoggerApiRoute::Metrics, "/metrics"),
            (LoggerApiRoute::Mmds, "/mmds"),
            (LoggerApiRoute::MmdsConfig, "/mmds/config"),
            (
                LoggerApiRoute::NetworkInterface,
                "/network-interfaces/{iface_id}",
            ),
            (LoggerApiRoute::Pmem, "/pmem/{pmem_id}"),
            (LoggerApiRoute::Serial, "/serial"),
            (LoggerApiRoute::SnapshotCreate, "/snapshot/create"),
            (LoggerApiRoute::SnapshotLoad, "/snapshot/load"),
            (LoggerApiRoute::Version, "/version"),
            (LoggerApiRoute::Vm, "/vm"),
            (LoggerApiRoute::VmConfig, "/vm/config"),
            (LoggerApiRoute::Vsock, "/vsock"),
        ];

        for (route, expected) in cases {
            assert_eq!(route.as_str(), expected);
        }
    }

    #[test]
    fn normalizes_absolute_parent_and_windows_origins() {
        assert_eq!(
            normalize_origin("/Users/secret/work/crates/runtime/src/logger.rs"),
            "crates/runtime/src/logger.rs"
        );
        assert_eq!(normalize_origin("../../private/秘密.rs"), "秘密.rs");
        assert_eq!(normalize_origin(r"C:\\secret\\worker.rs"), "worker.rs");
        assert_eq!(
            normalize_origin(r"C:\\secret\\crates\\runtime\\src\\logger.rs"),
            r"crates\\runtime\\src\\logger.rs"
        );
        assert_eq!(
            normalize_origin("/private/crates/../secret.rs"),
            "secret.rs"
        );
        assert_eq!(normalize_origin("/private/mycrates/secret.rs"), "secret.rs");
        assert_eq!(
            normalize_origin("crates/runtime/src/logger.rs"),
            "crates/runtime/src/logger.rs"
        );
    }

    #[test]
    fn maximum_values_and_multibyte_origin_stay_bounded_utf8() {
        let long_origin = format!("/private/{}", "秘密".repeat(300));
        let record = LogRecord::encode(
            true,
            true,
            LogOrigin::new(&long_origin, u32::MAX),
            LoggerLevel::Info,
            LoggerEvent::BootTime {
                wall_time_us: u64::MAX,
                cpu_time_us: u64::MAX,
            },
        );

        assert!(record.as_bytes().len() <= MAX_LOG_RECORD_BYTES);
        assert!(record.as_str().ends_with('\n'));
        assert!(!record.as_str().contains("/private/"));
        assert!(record.as_str().contains("...:"));
    }
}
