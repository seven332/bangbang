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
    ApiRequest {
        method: LoggerHttpMethod,
        route: LoggerApiRoute,
    },
    Action(LoggerAction),
    BootTime {
        wall_time_us: u64,
        cpu_time_us: u64,
    },
    RateLimitRecovery {
        suppressed: u64,
    },
    ProcessPanic,
    ProcessExit(ProcessTerminalCategory),
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
            LoggerEvent::ApiRequest { method, route } => {
                encoder.push_str("The API server received a ");
                encoder.push_str(method.as_str());
                encoder.push_str(" request on \"");
                encoder.push_str(route.as_str());
                encoder.push_str("\".");
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
            LoggerEvent::RateLimitRecovery { suppressed } => {
                encoder.push_u64(suppressed);
                encoder.push_str(" messages were suppressed due to rate limiting");
            }
            LoggerEvent::ProcessPanic => encoder.push_str("event=process-panic"),
            LoggerEvent::ProcessExit(category) => {
                encoder.push_str("event=process-exit category=");
                encoder.push_str(category.as_str());
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

#[derive(Debug)]
pub(super) struct LogBatch {
    first: LogRecord,
    second: Option<LogRecord>,
}

impl LogBatch {
    pub(super) const fn one(record: LogRecord) -> Self {
        Self {
            first: record,
            second: None,
        }
    }

    pub(super) const fn two(first: LogRecord, second: LogRecord) -> Self {
        Self {
            first,
            second: Some(second),
        }
    }

    pub(super) const fn len(&self) -> usize {
        if self.second.is_some() { 2 } else { 1 }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &LogRecord> {
        std::iter::once(&self.first).chain(self.second.as_ref())
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
        LogOrigin, LogRecord, LoggerAction, LoggerApiRoute, LoggerEvent, LoggerHttpMethod,
        MAX_LOG_RECORD_BYTES, PanicLogRecords, ProcessTerminalCategory, normalize_origin,
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
