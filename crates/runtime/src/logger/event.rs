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
        MAX_LOG_RECORD_BYTES, normalize_origin,
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
    fn dynamic_routes_are_fixed_templates() {
        assert_eq!(LoggerApiRoute::Drive.as_str(), "/drives/{drive_id}");
        assert_eq!(
            LoggerApiRoute::NetworkInterface.as_str(),
            "/network-interfaces/{iface_id}"
        );
        assert_eq!(LoggerApiRoute::Pmem.as_str(), "/pmem/{pmem_id}");
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
