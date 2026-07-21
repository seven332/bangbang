//! Backend-neutral serial MMIO device model with bounded receive state.

use std::collections::{TryReserveError, VecDeque};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::mmio::{
    MmioAccess, MmioAccessBytes, MmioAccessBytesError, MmioHandler, MmioHandlerError,
};
use crate::token_bucket::{TokenBucket, TokenBucketConfig};

pub const SERIAL_MMIO_DEVICE_WINDOW_SIZE: u64 = 0x1000;
pub const SERIAL_REGISTER_SPACE_SIZE: u64 = 8;
pub const SERIAL_TRANSMIT_REGISTER_OFFSET: u64 = 0;
pub const SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET: u64 = 1;
pub const SERIAL_INTERRUPT_IDENTIFICATION_REGISTER_OFFSET: u64 = 2;
pub const SERIAL_FIFO_CONTROL_REGISTER_OFFSET: u64 = 2;
pub const SERIAL_LINE_CONTROL_REGISTER_OFFSET: u64 = 3;
pub const SERIAL_MODEM_CONTROL_REGISTER_OFFSET: u64 = 4;
pub const SERIAL_LINE_STATUS_REGISTER_OFFSET: u64 = 5;
pub const SERIAL_MODEM_STATUS_REGISTER_OFFSET: u64 = 6;
pub const SERIAL_SCRATCH_REGISTER_OFFSET: u64 = 7;
pub const SERIAL_LINE_CONTROL_DLAB: u8 = 0x80;
pub const SERIAL_INTERRUPT_ENABLE_RECEIVED_DATA_AVAILABLE: u8 = 0x01;
pub const SERIAL_INTERRUPT_IDENTIFICATION_FIFO_ENABLED: u8 = 0xc0;
pub const SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING: u8 = 0x01;
pub const SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE: u8 = 0x04;
pub const SERIAL_FIFO_CONTROL_CLEAR_RECEIVE: u8 = 0x02;
pub const SERIAL_LINE_STATUS_DATA_READY: u8 = 0x01;
pub const SERIAL_LINE_STATUS_OVERRUN_ERROR: u8 = 0x02;
pub const SERIAL_LINE_STATUS_TRANSMIT_HOLDING_REGISTER_EMPTY: u8 = 0x20;
pub const SERIAL_LINE_STATUS_TRANSMITTER_EMPTY: u8 = 0x40;
pub const SERIAL_LINE_STATUS_DEFAULT: u8 =
    SERIAL_LINE_STATUS_TRANSMIT_HOLDING_REGISTER_EMPTY | SERIAL_LINE_STATUS_TRANSMITTER_EMPTY;
pub const SERIAL_OUTPUT_BUFFER_DEFAULT_LIMIT: usize = 64 * 1024;
pub const SERIAL_RECEIVE_FIFO_CAPACITY: usize = 0x40;

pub trait SerialOutput: fmt::Debug + Send {
    fn write_byte(&mut self, byte: u8) -> Result<(), SerialOutputError>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SerialOutputMetrics {
    error_count: u64,
    flush_count: u64,
    input_count: u64,
    interrupt_count: u64,
    missed_read_count: u64,
    missed_write_count: u64,
    overrun_count: u64,
    read_count: u64,
    write_count: u64,
    rate_limiter_dropped_bytes: u64,
}

impl SerialOutputMetrics {
    const fn incremental_delta(current: u64, previous: u64) -> u64 {
        if current >= previous {
            current - previous
        } else {
            current
        }
    }

    pub(crate) const fn delta_since(self, previous: Self) -> Self {
        Self {
            error_count: Self::incremental_delta(self.error_count, previous.error_count),
            flush_count: Self::incremental_delta(self.flush_count, previous.flush_count),
            input_count: Self::incremental_delta(self.input_count, previous.input_count),
            interrupt_count: Self::incremental_delta(
                self.interrupt_count,
                previous.interrupt_count,
            ),
            missed_read_count: Self::incremental_delta(
                self.missed_read_count,
                previous.missed_read_count,
            ),
            missed_write_count: Self::incremental_delta(
                self.missed_write_count,
                previous.missed_write_count,
            ),
            overrun_count: Self::incremental_delta(self.overrun_count, previous.overrun_count),
            read_count: Self::incremental_delta(self.read_count, previous.read_count),
            write_count: Self::incremental_delta(self.write_count, previous.write_count),
            rate_limiter_dropped_bytes: Self::incremental_delta(
                self.rate_limiter_dropped_bytes,
                previous.rate_limiter_dropped_bytes,
            ),
        }
    }

    pub const fn error_count(self) -> u64 {
        self.error_count
    }

    pub const fn flush_count(self) -> u64 {
        self.flush_count
    }

    pub const fn input_count(self) -> u64 {
        self.input_count
    }

    pub const fn interrupt_count(self) -> u64 {
        self.interrupt_count
    }

    pub const fn missed_read_count(self) -> u64 {
        self.missed_read_count
    }

    pub const fn missed_write_count(self) -> u64 {
        self.missed_write_count
    }

    pub const fn overrun_count(self) -> u64 {
        self.overrun_count
    }

    pub const fn read_count(self) -> u64 {
        self.read_count
    }

    pub const fn write_count(self) -> u64 {
        self.write_count
    }

    pub const fn rate_limiter_dropped_bytes(self) -> u64 {
        self.rate_limiter_dropped_bytes
    }

    pub const fn with_error_count(mut self, error_count: u64) -> Self {
        self.error_count = error_count;
        self
    }

    pub const fn with_flush_count(mut self, flush_count: u64) -> Self {
        self.flush_count = flush_count;
        self
    }

    pub const fn with_input_count(mut self, input_count: u64) -> Self {
        self.input_count = input_count;
        self
    }

    pub const fn with_interrupt_count(mut self, interrupt_count: u64) -> Self {
        self.interrupt_count = interrupt_count;
        self
    }

    pub const fn with_missed_read_count(mut self, missed_read_count: u64) -> Self {
        self.missed_read_count = missed_read_count;
        self
    }

    pub const fn with_missed_write_count(mut self, missed_write_count: u64) -> Self {
        self.missed_write_count = missed_write_count;
        self
    }

    pub const fn with_overrun_count(mut self, overrun_count: u64) -> Self {
        self.overrun_count = overrun_count;
        self
    }

    pub const fn with_read_count(mut self, read_count: u64) -> Self {
        self.read_count = read_count;
        self
    }

    pub const fn with_write_count(mut self, write_count: u64) -> Self {
        self.write_count = write_count;
        self
    }

    pub const fn with_rate_limiter_dropped_bytes(
        mut self,
        rate_limiter_dropped_bytes: u64,
    ) -> Self {
        self.rate_limiter_dropped_bytes = rate_limiter_dropped_bytes;
        self
    }

    pub const fn is_empty(self) -> bool {
        self.error_count == 0
            && self.flush_count == 0
            && self.input_count == 0
            && self.interrupt_count == 0
            && self.missed_read_count == 0
            && self.missed_write_count == 0
            && self.overrun_count == 0
            && self.read_count == 0
            && self.write_count == 0
            && self.rate_limiter_dropped_bytes == 0
    }
}

#[derive(Debug, Clone, Default)]
struct SharedSerialOutputMetrics {
    inner: Arc<SharedSerialOutputMetricsInner>,
}

impl SharedSerialOutputMetrics {
    fn record_error(&self) {
        record_serial_metric(&self.inner.error_count, 1);
    }

    fn record_flush(&self) {
        record_serial_metric(&self.inner.flush_count, 1);
    }

    fn record_input(&self, bytes: u64) {
        record_serial_metric(&self.inner.input_count, bytes);
    }

    fn record_interrupt(&self) {
        record_serial_metric(&self.inner.interrupt_count, 1);
    }

    fn record_missed_read(&self) {
        record_serial_metric(&self.inner.missed_read_count, 1);
    }

    fn record_missed_write(&self) {
        record_serial_metric(&self.inner.missed_write_count, 1);
    }

    fn record_overrun(&self, bytes: u64) {
        record_serial_metric(&self.inner.overrun_count, bytes);
    }

    fn record_read(&self) {
        record_serial_metric(&self.inner.read_count, 1);
    }

    fn record_write(&self) {
        record_serial_metric(&self.inner.write_count, 1);
    }

    fn record_rate_limiter_dropped_bytes(&self, bytes: u64) {
        record_serial_metric(&self.inner.rate_limiter_dropped_bytes, bytes);
    }

    fn snapshot(&self) -> SerialOutputMetrics {
        SerialOutputMetrics {
            error_count: self.inner.error_count.load(Ordering::Relaxed),
            flush_count: self.inner.flush_count.load(Ordering::Relaxed),
            input_count: self.inner.input_count.load(Ordering::Relaxed),
            interrupt_count: self.inner.interrupt_count.load(Ordering::Relaxed),
            missed_read_count: self.inner.missed_read_count.load(Ordering::Relaxed),
            missed_write_count: self.inner.missed_write_count.load(Ordering::Relaxed),
            overrun_count: self.inner.overrun_count.load(Ordering::Relaxed),
            read_count: self.inner.read_count.load(Ordering::Relaxed),
            write_count: self.inner.write_count.load(Ordering::Relaxed),
            rate_limiter_dropped_bytes: self
                .inner
                .rate_limiter_dropped_bytes
                .load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Default)]
struct SharedSerialOutputMetricsInner {
    error_count: AtomicU64,
    flush_count: AtomicU64,
    input_count: AtomicU64,
    interrupt_count: AtomicU64,
    missed_read_count: AtomicU64,
    missed_write_count: AtomicU64,
    overrun_count: AtomicU64,
    read_count: AtomicU64,
    write_count: AtomicU64,
    rate_limiter_dropped_bytes: AtomicU64,
}

fn record_serial_metric(counter: &AtomicU64, value: u64) {
    if value == 0 {
        return;
    }

    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_add(value);
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn saturating_usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerialRateLimiterConfig {
    size: u64,
    one_time_burst: Option<u64>,
    refill_time: u64,
}

impl SerialRateLimiterConfig {
    pub const fn new(size: u64, one_time_burst: Option<u64>, refill_time: u64) -> Self {
        Self {
            size,
            one_time_burst,
            refill_time,
        }
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn one_time_burst(self) -> Option<u64> {
        self.one_time_burst
    }

    pub const fn refill_time(self) -> u64 {
        self.refill_time
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct SerialConfigInput {
    serial_out_path: Option<String>,
    rate_limiter: Option<SerialRateLimiterConfig>,
}

impl fmt::Debug for SerialConfigInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerialConfigInput")
            .field(
                "serial_out_path",
                &self.serial_out_path.as_ref().map(|_| "<redacted>"),
            )
            .field("rate_limiter", &self.rate_limiter)
            .finish()
    }
}

impl SerialConfigInput {
    pub const fn new() -> Self {
        Self {
            serial_out_path: None,
            rate_limiter: None,
        }
    }

    pub fn with_serial_out_path(mut self, serial_out_path: impl Into<String>) -> Self {
        self.serial_out_path = Some(serial_out_path.into());
        self
    }

    pub const fn with_rate_limiter(mut self, rate_limiter: SerialRateLimiterConfig) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }

    pub fn serial_out_path(&self) -> Option<&str> {
        self.serial_out_path.as_deref()
    }

    pub const fn rate_limiter(&self) -> Option<SerialRateLimiterConfig> {
        self.rate_limiter
    }

    pub fn validate(self) -> Result<SerialConfig, SerialConfigError> {
        SerialConfig::try_from(self)
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct SerialConfig {
    serial_out_path: Option<PathBuf>,
    rate_limiter: Option<SerialRateLimiterConfig>,
}

impl fmt::Debug for SerialConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerialConfig")
            .field(
                "serial_out_path",
                &self.serial_out_path.as_ref().map(|_| "<redacted>"),
            )
            .field("rate_limiter", &self.rate_limiter)
            .finish()
    }
}

impl SerialConfig {
    pub fn serial_out_path(&self) -> Option<&Path> {
        self.serial_out_path.as_deref()
    }

    pub const fn rate_limiter(&self) -> Option<SerialRateLimiterConfig> {
        self.rate_limiter
    }
}

impl TryFrom<SerialConfigInput> for SerialConfig {
    type Error = SerialConfigError;

    fn try_from(input: SerialConfigInput) -> Result<Self, Self::Error> {
        if let Some(serial_out_path) = input.serial_out_path.as_deref() {
            if serial_out_path.is_empty() {
                return Err(SerialConfigError::EmptyOutputPath);
            }
            if has_control_character(serial_out_path) {
                return Err(SerialConfigError::InvalidOutputPath);
            }
        }

        Ok(Self {
            serial_out_path: input.serial_out_path.map(PathBuf::from),
            rate_limiter: input.rate_limiter,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialConfigError {
    EmptyOutputPath,
    InvalidOutputPath,
    OpenOutput(std::io::ErrorKind),
    ProvidedOutputWithoutPath,
}

impl fmt::Display for SerialConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyOutputPath => f.write_str("serial output path must not be empty"),
            Self::InvalidOutputPath => {
                f.write_str("serial output path must not contain control characters")
            }
            Self::OpenOutput(kind) => {
                write!(f, "serial output could not be initialized: {kind:?}")
            }
            Self::ProvidedOutputWithoutPath => {
                f.write_str("provided serial output does not match configuration")
            }
        }
    }
}

impl std::error::Error for SerialConfigError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialOutputBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl SerialOutputBuffer {
    pub const fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub const fn limit(&self) -> usize {
        self.limit
    }
}

impl Default for SerialOutputBuffer {
    fn default() -> Self {
        Self::new(SERIAL_OUTPUT_BUFFER_DEFAULT_LIMIT)
    }
}

impl SerialOutput for SerialOutputBuffer {
    fn write_byte(&mut self, byte: u8) -> Result<(), SerialOutputError> {
        if self.bytes.len() >= self.limit {
            return Err(SerialOutputError::buffer_full(self.limit));
        }

        self.bytes
            .try_reserve(1)
            .map_err(SerialOutputError::from_try_reserve)?;
        self.bytes.push(byte);
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SharedSerialOutputBuffer {
    buffer: Arc<Mutex<SerialOutputBuffer>>,
}

impl SharedSerialOutputBuffer {
    pub fn new(limit: usize) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(SerialOutputBuffer::new(limit))),
        }
    }

    pub fn bytes(&self) -> Result<Vec<u8>, SerialOutputError> {
        let buffer = self
            .buffer
            .lock()
            .map_err(|_| SerialOutputError::lock_poisoned())?;

        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(buffer.bytes().len())
            .map_err(SerialOutputError::from_try_reserve)?;
        bytes.extend_from_slice(buffer.bytes());
        Ok(bytes)
    }

    pub fn limit(&self) -> Result<usize, SerialOutputError> {
        let buffer = self
            .buffer
            .lock()
            .map_err(|_| SerialOutputError::lock_poisoned())?;

        Ok(buffer.limit())
    }
}

impl Default for SharedSerialOutputBuffer {
    fn default() -> Self {
        Self::new(SERIAL_OUTPUT_BUFFER_DEFAULT_LIMIT)
    }
}

impl SerialOutput for SharedSerialOutputBuffer {
    fn write_byte(&mut self, byte: u8) -> Result<(), SerialOutputError> {
        let mut buffer = self
            .buffer
            .lock()
            .map_err(|_| SerialOutputError::lock_poisoned())?;

        buffer.write_byte(byte)
    }
}

#[derive(Debug)]
struct MeteredSerialOutput<O> {
    output: O,
    metrics: SharedSerialOutputMetrics,
}

impl<O> MeteredSerialOutput<O> {
    fn new(output: O, metrics: SharedSerialOutputMetrics) -> Self {
        Self { output, metrics }
    }
}

impl<O: SerialOutput> SerialOutput for MeteredSerialOutput<O> {
    fn write_byte(&mut self, byte: u8) -> Result<(), SerialOutputError> {
        match self.output.write_byte(byte) {
            Ok(()) => {
                self.metrics.record_write();
                Ok(())
            }
            Err(err) => {
                self.metrics.record_missed_write();
                self.metrics.record_error();
                Err(err)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct SharedSerialOutput {
    output: Arc<Mutex<Box<dyn SerialOutput>>>,
    metrics: SharedSerialOutputMetrics,
}

impl SharedSerialOutput {
    pub fn new(output: impl SerialOutput + 'static) -> Self {
        let metrics = SharedSerialOutputMetrics::default();
        Self {
            output: Arc::new(Mutex::new(Box::new(MeteredSerialOutput::new(
                output,
                metrics.clone(),
            )))),
            metrics,
        }
    }

    pub fn with_rate_limiter(
        output: impl SerialOutput + 'static,
        rate_limiter: Option<SerialRateLimiterConfig>,
    ) -> Self {
        let metrics = SharedSerialOutputMetrics::default();
        let metered_output = MeteredSerialOutput::new(output, metrics.clone());
        let output: Box<dyn SerialOutput> = match rate_limiter.and_then(serial_token_bucket) {
            Some(bucket) => Box::new(RateLimitedSerialOutput::from_bucket(
                metered_output,
                bucket,
                metrics.clone(),
            )),
            None => Box::new(metered_output),
        };

        Self {
            output: Arc::new(Mutex::new(output)),
            metrics,
        }
    }

    pub fn metrics(&self) -> SerialOutputMetrics {
        self.metrics.snapshot()
    }

    /// Record one host-input lifecycle failure without changing UART state.
    pub fn record_host_input_error(&self) {
        self.metrics.record_error();
    }
}

impl From<SharedSerialOutputBuffer> for SharedSerialOutput {
    fn from(output: SharedSerialOutputBuffer) -> Self {
        Self::new(output)
    }
}

impl SerialOutput for SharedSerialOutput {
    fn write_byte(&mut self, byte: u8) -> Result<(), SerialOutputError> {
        let mut output = self.output.lock().map_err(|_| {
            self.metrics.record_missed_write();
            self.metrics.record_error();
            SerialOutputError::lock_poisoned()
        })?;

        output.write_byte(byte)
    }
}

#[derive(Debug)]
struct RateLimitedSerialOutput<O> {
    output: O,
    bucket: TokenBucket,
    metrics: SharedSerialOutputMetrics,
}

impl<O> RateLimitedSerialOutput<O> {
    #[cfg(test)]
    fn new(output: O, config: SerialRateLimiterConfig) -> Option<Self> {
        let metrics = SharedSerialOutputMetrics::default();
        serial_token_bucket(config).map(|bucket| Self {
            output,
            bucket,
            metrics,
        })
    }

    fn from_bucket(output: O, bucket: TokenBucket, metrics: SharedSerialOutputMetrics) -> Self {
        Self {
            output,
            bucket,
            metrics,
        }
    }

    #[cfg(test)]
    fn metrics(&self) -> SerialOutputMetrics {
        self.metrics.snapshot()
    }
}

impl<O: SerialOutput> SerialOutput for RateLimitedSerialOutput<O> {
    fn write_byte(&mut self, byte: u8) -> Result<(), SerialOutputError> {
        if self.bucket.reduce(1) {
            self.output.write_byte(byte)
        } else {
            self.metrics.record_rate_limiter_dropped_bytes(1);
            Ok(())
        }
    }
}

fn serial_token_bucket(config: SerialRateLimiterConfig) -> Option<TokenBucket> {
    TokenBucket::new(TokenBucketConfig::new(
        config.size(),
        config.one_time_burst(),
        config.refill_time(),
    ))
}

pub struct SerialOutputFile {
    file: File,
}

impl fmt::Debug for SerialOutputFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerialOutputFile")
            .finish_non_exhaustive()
    }
}

impl SerialOutputFile {
    pub fn open(path: &Path) -> Result<Self, SerialConfigError> {
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|err| SerialConfigError::OpenOutput(err.kind()))?;

        Ok(Self { file })
    }

    /// Adopts an authority-provided write-only regular file as serial output.
    pub fn from_file(file: File) -> Result<Self, SerialConfigError> {
        let file = crate::output_file::adopt_write_only_file(file)
            .map_err(SerialConfigError::OpenOutput)?;
        Ok(Self { file })
    }
}

impl SerialOutput for SerialOutputFile {
    fn write_byte(&mut self, byte: u8) -> Result<(), SerialOutputError> {
        self.file
            .write_all(&[byte])
            .map_err(|err| SerialOutputError::file_write(err.kind()))
    }
}

/// Process-standard serial endpoints with one shared restoration lifetime.
pub struct SerialStdio {
    output: SerialStdioOutput,
    input: Option<SerialStdioInput>,
}

impl fmt::Debug for SerialStdio {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerialStdio")
            .field("output", &"<owned>")
            .field("input", &self.input.as_ref().map(|_| "<owned>"))
            .finish()
    }
}

impl SerialStdio {
    /// Prepare the process standard streams for Firecracker-compatible serial
    /// output and optional terminal/FIFO input.
    pub fn from_process_standard_streams() -> Result<Self, SerialStdioError> {
        Self::from_descriptors(libc::STDIN_FILENO, libc::STDOUT_FILENO)
    }

    /// Prepare caller-owned descriptors while retaining the caller's ownership.
    ///
    /// This exists for focused descriptor and pseudo-terminal verification. The
    /// returned endpoints use close-on-exec duplicates and restore the shared
    /// open-file-description state when their final owner is dropped.
    #[doc(hidden)]
    pub fn from_descriptors(
        input_descriptor: RawFd,
        output_descriptor: RawFd,
    ) -> Result<Self, SerialStdioError> {
        let output = duplicate_stdio_descriptor(output_descriptor)
            .map_err(|source| SerialStdioError::DuplicateOutput { source })?;
        let output_flags = descriptor_status_flags(output.as_raw_fd())
            .map_err(|source| SerialStdioError::InspectOutput { source })?;
        if output_flags & libc::O_ACCMODE == libc::O_RDONLY {
            return Err(SerialStdioError::OutputNotWritable);
        }

        let input_kind = serial_input_kind(input_descriptor)?;
        let (input, input_flags, input_termios) = match input_kind {
            Some(SerialStdioInputKind::Terminal) => {
                let input = duplicate_stdio_descriptor(input_descriptor)
                    .map_err(|source| SerialStdioError::DuplicateInput { source })?;
                let input_flags = descriptor_status_flags(input.as_raw_fd())
                    .map_err(|source| SerialStdioError::InspectInput { source })?;
                if input_flags & libc::O_ACCMODE == libc::O_WRONLY {
                    return Err(SerialStdioError::InputNotReadable);
                }
                let termios = terminal_attributes(input.as_raw_fd())
                    .map_err(|source| SerialStdioError::InspectTerminal { source })?;
                (Some(input), Some(input_flags), Some(termios))
            }
            Some(SerialStdioInputKind::Fifo) => {
                let input = duplicate_stdio_descriptor(input_descriptor)
                    .map_err(|source| SerialStdioError::DuplicateInput { source })?;
                let input_flags = descriptor_status_flags(input.as_raw_fd())
                    .map_err(|source| SerialStdioError::InspectInput { source })?;
                if input_flags & libc::O_ACCMODE == libc::O_WRONLY {
                    return Err(SerialStdioError::InputNotReadable);
                }
                (Some(input), Some(input_flags), None)
            }
            None => (None, None, None),
        };

        let descriptors = Arc::new(SerialStdioDescriptors {
            output,
            output_flags,
            input,
            input_flags,
            input_termios,
        });

        set_descriptor_status_flags(
            descriptors.output.as_raw_fd(),
            output_flags | libc::O_NONBLOCK,
        )
        .map_err(|source| SerialStdioError::ConfigureOutput { source })?;
        if let (Some(input), Some(flags)) = (&descriptors.input, input_flags) {
            if let Some(termios) = input_termios {
                set_raw_terminal(input.as_raw_fd(), termios)
                    .map_err(|source| SerialStdioError::ConfigureTerminal { source })?;
            }
            set_descriptor_status_flags(input.as_raw_fd(), flags | libc::O_NONBLOCK)
                .map_err(|source| SerialStdioError::ConfigureInput { source })?;
        }

        let input = descriptors.input.as_ref().map(|input| SerialStdioInput {
            descriptor: input.as_raw_fd(),
            descriptors: Arc::clone(&descriptors),
        });
        Ok(Self {
            output: SerialStdioOutput { descriptors },
            input,
        })
    }

    pub fn into_parts(self) -> (SerialStdioOutput, Option<SerialStdioInput>) {
        (self.output, self.input)
    }
}

struct SerialStdioDescriptors {
    output: File,
    output_flags: libc::c_int,
    input: Option<File>,
    input_flags: Option<libc::c_int>,
    input_termios: Option<libc::termios>,
}

impl fmt::Debug for SerialStdioDescriptors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerialStdioDescriptors")
            .field("endpoints", &"<redacted>")
            .finish()
    }
}

impl Drop for SerialStdioDescriptors {
    fn drop(&mut self) {
        if let Some(input) = self.input.as_ref() {
            if let Some(termios) = self.input_termios.as_ref() {
                // SAFETY: The descriptor and retained terminal attributes remain
                // valid until this restoration owner finishes dropping.
                let _ = unsafe { libc::tcsetattr(input.as_raw_fd(), libc::TCSANOW, termios) };
            }
            if let Some(flags) = self.input_flags {
                let _ = set_descriptor_status_flags(input.as_raw_fd(), flags);
            }
        }
        let _ = set_descriptor_status_flags(self.output.as_raw_fd(), self.output_flags);
    }
}

/// Nonblocking stdout half of one prepared serial stdio lifetime.
pub struct SerialStdioOutput {
    descriptors: Arc<SerialStdioDescriptors>,
}

impl fmt::Debug for SerialStdioOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerialStdioOutput")
            .field("endpoint", &"<redacted>")
            .finish()
    }
}

impl SerialOutput for SerialStdioOutput {
    fn write_byte(&mut self, byte: u8) -> Result<(), SerialOutputError> {
        let mut output = &self.descriptors.output;
        output
            .write_all(&[byte])
            .map_err(|error| SerialOutputError::file_write(error.kind()))
    }
}

/// Optional nonblocking terminal/FIFO stdin half of a serial stdio lifetime.
pub struct SerialStdioInput {
    descriptor: RawFd,
    descriptors: Arc<SerialStdioDescriptors>,
}

impl SerialStdioInput {
    pub fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let Some(input) = self.descriptors.input.as_ref() else {
            return Err(std::io::Error::other(
                "serial stdio input descriptor is unavailable",
            ));
        };
        let mut input = input;
        input.read(buffer)
    }
}

impl AsRawFd for SerialStdioInput {
    fn as_raw_fd(&self) -> RawFd {
        self.descriptor
    }
}

impl fmt::Debug for SerialStdioInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerialStdioInput")
            .field("endpoint", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialStdioError {
    InspectInput { source: std::io::ErrorKind },
    InspectOutput { source: std::io::ErrorKind },
    DuplicateInput { source: std::io::ErrorKind },
    DuplicateOutput { source: std::io::ErrorKind },
    InputNotReadable,
    OutputNotWritable,
    InspectTerminal { source: std::io::ErrorKind },
    ConfigureTerminal { source: std::io::ErrorKind },
    ConfigureInput { source: std::io::ErrorKind },
    ConfigureOutput { source: std::io::ErrorKind },
}

impl fmt::Display for SerialStdioError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InspectInput { source } => {
                write!(formatter, "serial stdin could not be inspected: {source:?}")
            }
            Self::InspectOutput { source } => {
                write!(
                    formatter,
                    "serial stdout could not be inspected: {source:?}"
                )
            }
            Self::DuplicateInput { source } => {
                write!(
                    formatter,
                    "serial stdin could not be duplicated: {source:?}"
                )
            }
            Self::DuplicateOutput { source } => {
                write!(
                    formatter,
                    "serial stdout could not be duplicated: {source:?}"
                )
            }
            Self::InputNotReadable => formatter.write_str("serial stdin is not readable"),
            Self::OutputNotWritable => formatter.write_str("serial stdout is not writable"),
            Self::InspectTerminal { source } => {
                write!(
                    formatter,
                    "serial terminal could not be inspected: {source:?}"
                )
            }
            Self::ConfigureTerminal { source } => {
                write!(
                    formatter,
                    "serial terminal could not be configured: {source:?}"
                )
            }
            Self::ConfigureInput { source } => {
                write!(
                    formatter,
                    "serial stdin could not be configured: {source:?}"
                )
            }
            Self::ConfigureOutput { source } => {
                write!(
                    formatter,
                    "serial stdout could not be configured: {source:?}"
                )
            }
        }
    }
}

impl std::error::Error for SerialStdioError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SerialStdioInputKind {
    Terminal,
    Fifo,
}

fn serial_input_kind(descriptor: RawFd) -> Result<Option<SerialStdioInputKind>, SerialStdioError> {
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `metadata` is writable for one complete `stat` value and the
    // descriptor is borrowed without ownership transfer.
    if unsafe { libc::fstat(descriptor, metadata.as_mut_ptr()) } != 0 {
        return Ok(None);
    }
    // SAFETY: Successful `fstat` initialized the complete value.
    let metadata = unsafe { metadata.assume_init() };
    // SAFETY: `isatty` only inspects the borrowed descriptor.
    if unsafe { libc::isatty(descriptor) } == 1 {
        Ok(Some(SerialStdioInputKind::Terminal))
    } else if metadata.st_mode & libc::S_IFMT == libc::S_IFIFO {
        Ok(Some(SerialStdioInputKind::Fifo))
    } else {
        Ok(None)
    }
}

fn duplicate_stdio_descriptor(descriptor: RawFd) -> Result<File, std::io::ErrorKind> {
    // SAFETY: `F_DUPFD_CLOEXEC` duplicates the borrowed descriptor and returns
    // a fresh descriptor on success.
    let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        Err(std::io::Error::last_os_error().kind())
    } else {
        // SAFETY: `duplicate` is a fresh successful descriptor owned here.
        Ok(unsafe { File::from_raw_fd(duplicate) })
    }
}

fn descriptor_status_flags(descriptor: RawFd) -> Result<libc::c_int, std::io::ErrorKind> {
    // SAFETY: `F_GETFL` only inspects one borrowed live descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        Err(std::io::Error::last_os_error().kind())
    } else {
        Ok(flags)
    }
}

fn set_descriptor_status_flags(
    descriptor: RawFd,
    flags: libc::c_int,
) -> Result<(), std::io::ErrorKind> {
    // SAFETY: `F_SETFL` changes only status flags on the borrowed live open file
    // description.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags) } < 0 {
        Err(std::io::Error::last_os_error().kind())
    } else {
        Ok(())
    }
}

fn terminal_attributes(descriptor: RawFd) -> Result<libc::termios, std::io::ErrorKind> {
    let mut attributes = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `attributes` is writable for a complete terminal state and the
    // descriptor is borrowed.
    if unsafe { libc::tcgetattr(descriptor, attributes.as_mut_ptr()) } != 0 {
        Err(std::io::Error::last_os_error().kind())
    } else {
        // SAFETY: Successful `tcgetattr` initialized the complete value.
        Ok(unsafe { attributes.assume_init() })
    }
}

fn set_raw_terminal(descriptor: RawFd, original: libc::termios) -> Result<(), std::io::ErrorKind> {
    let mut raw = original;
    // SAFETY: `raw` is a complete local termios value for in-place conversion.
    unsafe { libc::cfmakeraw(&raw mut raw) };
    // SAFETY: The descriptor is borrowed and `raw` remains valid for the call.
    if unsafe { libc::tcsetattr(descriptor, libc::TCSANOW, &raw) } != 0 {
        Err(std::io::Error::last_os_error().kind())
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiscardSerialOutput;

impl SerialOutput for DiscardSerialOutput {
    fn write_byte(&mut self, _byte: u8) -> Result<(), SerialOutputError> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialOutputError {
    message: String,
}

impl SerialOutputError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn from_try_reserve(source: TryReserveError) -> Self {
        Self::new(format!("serial output buffer allocation failed: {source}"))
    }

    fn buffer_full(limit: usize) -> Self {
        Self::new(format!(
            "serial output buffer reached its {limit}-byte limit"
        ))
    }

    fn lock_poisoned() -> Self {
        Self::new("serial output lock was poisoned")
    }

    fn file_write(kind: std::io::ErrorKind) -> Self {
        Self::new(format!("serial output file write failed: {kind:?}"))
    }
}

impl fmt::Display for SerialOutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SerialOutputError {}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SerialMmioState {
    interrupt_enable: u8,
    line_control: u8,
    modem_control: u8,
    scratch: u8,
    divisor_latch_low: u8,
    divisor_latch_high: u8,
}

impl SerialMmioState {
    pub const fn new(
        interrupt_enable: u8,
        line_control: u8,
        modem_control: u8,
        scratch: u8,
        divisor_latch_low: u8,
        divisor_latch_high: u8,
    ) -> Self {
        Self {
            interrupt_enable,
            line_control,
            modem_control,
            scratch,
            divisor_latch_low,
            divisor_latch_high,
        }
    }

    pub const fn interrupt_enable(self) -> u8 {
        self.interrupt_enable
    }

    pub const fn line_control(self) -> u8 {
        self.line_control
    }

    pub const fn modem_control(self) -> u8 {
        self.modem_control
    }

    pub const fn scratch(self) -> u8 {
        self.scratch
    }

    pub const fn divisor_latch_low(self) -> u8 {
        self.divisor_latch_low
    }

    pub const fn divisor_latch_high(self) -> u8 {
        self.divisor_latch_high
    }
}

impl fmt::Debug for SerialMmioState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SerialMmioState")
            .field("registers", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SerialReceiveInjectionOutcome {
    accepted_bytes: usize,
    rejected_bytes: usize,
    remaining_capacity: usize,
}

impl SerialReceiveInjectionOutcome {
    const fn new(accepted_bytes: usize, rejected_bytes: usize, remaining_capacity: usize) -> Self {
        Self {
            accepted_bytes,
            rejected_bytes,
            remaining_capacity,
        }
    }

    pub const fn accepted_bytes(self) -> usize {
        self.accepted_bytes
    }

    pub const fn rejected_bytes(self) -> usize {
        self.rejected_bytes
    }

    pub const fn remaining_capacity(self) -> usize {
        self.remaining_capacity
    }

    pub const fn is_backpressured(self) -> bool {
        self.remaining_capacity == 0
    }
}

#[derive(Debug)]
pub enum SerialReceiveInjectionError {
    BufferAllocation { source: TryReserveError },
}

impl fmt::Display for SerialReceiveInjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferAllocation { .. } => {
                formatter.write_str("serial receive buffer allocation failed")
            }
        }
    }
}

impl std::error::Error for SerialReceiveInjectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BufferAllocation { source } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialInterruptIntent {
    ReceivedDataAvailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialInputReadyIntent {
    ReceiveBufferEmpty,
}

pub struct SerialMmioCaptureStateParts {
    pub legacy_state: SerialMmioState,
    pub interrupt_identification: u8,
    pub line_status: u8,
    pub modem_status: u8,
    pub receive_bytes: Vec<u8>,
    pub receive_interrupt_intent_pending: bool,
    pub input_ready_intent_pending: bool,
}

impl fmt::Debug for SerialMmioCaptureStateParts {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerialMmioCaptureStateParts")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SerialMmioCaptureState {
    legacy_state: SerialMmioState,
    interrupt_identification: u8,
    line_status: u8,
    modem_status: u8,
    receive_bytes: Vec<u8>,
    receive_interrupt_intent_pending: bool,
    input_ready_intent_pending: bool,
}

impl SerialMmioCaptureState {
    pub fn try_from_parts(
        parts: SerialMmioCaptureStateParts,
    ) -> Result<Self, SerialMmioCaptureStateError> {
        if parts.receive_bytes.len() > SERIAL_RECEIVE_FIFO_CAPACITY {
            return Err(SerialMmioCaptureStateError::ReceiveBufferTooLarge {
                len: parts.receive_bytes.len(),
                capacity: SERIAL_RECEIVE_FIFO_CAPACITY,
            });
        }
        if !matches!(
            parts.interrupt_identification,
            SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING
                | SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE
        ) {
            return Err(
                SerialMmioCaptureStateError::InvalidInterruptIdentification {
                    value: parts.interrupt_identification,
                },
            );
        }
        let supported_line_status = SERIAL_LINE_STATUS_DEFAULT
            | SERIAL_LINE_STATUS_DATA_READY
            | SERIAL_LINE_STATUS_OVERRUN_ERROR;
        if parts.line_status & SERIAL_LINE_STATUS_DEFAULT != SERIAL_LINE_STATUS_DEFAULT
            || parts.line_status & !supported_line_status != 0
        {
            return Err(SerialMmioCaptureStateError::InvalidLineStatus {
                value: parts.line_status,
            });
        }
        if parts.modem_status != 0 {
            return Err(SerialMmioCaptureStateError::InvalidModemStatus {
                value: parts.modem_status,
            });
        }

        let has_receive_data = !parts.receive_bytes.is_empty();
        let reports_receive_data = parts.line_status & SERIAL_LINE_STATUS_DATA_READY != 0;
        if has_receive_data != reports_receive_data {
            return Err(SerialMmioCaptureStateError::DataReadyMismatch);
        }
        let receive_interrupt_identified = parts.interrupt_identification
            == SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE;
        if receive_interrupt_identified && !has_receive_data {
            return Err(SerialMmioCaptureStateError::ReceiveInterruptWithoutData);
        }
        if parts.receive_interrupt_intent_pending && !receive_interrupt_identified {
            return Err(SerialMmioCaptureStateError::ReceiveInterruptIntentMismatch);
        }
        if parts.input_ready_intent_pending && has_receive_data {
            return Err(SerialMmioCaptureStateError::InputReadyIntentMismatch);
        }

        Ok(Self {
            legacy_state: parts.legacy_state,
            interrupt_identification: parts.interrupt_identification,
            line_status: parts.line_status,
            modem_status: parts.modem_status,
            receive_bytes: parts.receive_bytes,
            receive_interrupt_intent_pending: parts.receive_interrupt_intent_pending,
            input_ready_intent_pending: parts.input_ready_intent_pending,
        })
    }

    pub const fn legacy_state(&self) -> SerialMmioState {
        self.legacy_state
    }

    pub const fn interrupt_identification(&self) -> u8 {
        self.interrupt_identification
    }

    pub const fn line_status(&self) -> u8 {
        self.line_status
    }

    pub const fn modem_status(&self) -> u8 {
        self.modem_status
    }

    pub fn receive_bytes(&self) -> &[u8] {
        self.receive_bytes.as_slice()
    }

    pub const fn receive_interrupt_intent_pending(&self) -> bool {
        self.receive_interrupt_intent_pending
    }

    pub const fn input_ready_intent_pending(&self) -> bool {
        self.input_ready_intent_pending
    }

    pub fn into_parts(self) -> SerialMmioCaptureStateParts {
        SerialMmioCaptureStateParts {
            legacy_state: self.legacy_state,
            interrupt_identification: self.interrupt_identification,
            line_status: self.line_status,
            modem_status: self.modem_status,
            receive_bytes: self.receive_bytes,
            receive_interrupt_intent_pending: self.receive_interrupt_intent_pending,
            input_ready_intent_pending: self.input_ready_intent_pending,
        }
    }
}

impl fmt::Debug for SerialMmioCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerialMmioCaptureState")
            .field("state", &"<redacted>")
            .finish()
    }
}

/// Reconstructible serial configuration paired with complete guest-visible
/// UART state. Live host endpoints intentionally remain outside this value.
#[derive(Clone, PartialEq, Eq)]
pub struct CaptureReadySerialState {
    config: SerialConfig,
    device: SerialMmioCaptureState,
}

impl CaptureReadySerialState {
    pub const fn new(config: SerialConfig, device: SerialMmioCaptureState) -> Self {
        Self { config, device }
    }

    pub const fn config(&self) -> &SerialConfig {
        &self.config
    }

    pub const fn device(&self) -> &SerialMmioCaptureState {
        &self.device
    }

    pub fn into_parts(self) -> (SerialConfig, SerialMmioCaptureState) {
        (self.config, self.device)
    }
}

impl fmt::Debug for CaptureReadySerialState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureReadySerialState")
            .field("config", &self.config)
            .field("device", &self.device)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialMmioCaptureStateError {
    ReceiveBufferTooLarge { len: usize, capacity: usize },
    InvalidInterruptIdentification { value: u8 },
    InvalidLineStatus { value: u8 },
    InvalidModemStatus { value: u8 },
    DataReadyMismatch,
    ReceiveInterruptWithoutData,
    ReceiveInterruptIntentMismatch,
    InputReadyIntentMismatch,
}

impl fmt::Display for SerialMmioCaptureStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReceiveBufferTooLarge { len, capacity } => write!(
                formatter,
                "serial receive buffer has {len} bytes, exceeding its {capacity}-byte capacity"
            ),
            Self::InvalidInterruptIdentification { value } => write!(
                formatter,
                "serial interrupt-identification value 0x{value:02x} is invalid"
            ),
            Self::InvalidLineStatus { value } => {
                write!(
                    formatter,
                    "serial line-status value 0x{value:02x} is invalid"
                )
            }
            Self::InvalidModemStatus { value } => write!(
                formatter,
                "serial modem-status value 0x{value:02x} is invalid"
            ),
            Self::DataReadyMismatch => {
                formatter.write_str("serial data-ready status does not match receive bytes")
            }
            Self::ReceiveInterruptWithoutData => {
                formatter.write_str("serial receive interrupt has no receive data")
            }
            Self::ReceiveInterruptIntentMismatch => {
                formatter.write_str("serial receive interrupt intent has no identified interrupt")
            }
            Self::InputReadyIntentMismatch => {
                formatter.write_str("serial input-ready intent has a nonempty receive buffer")
            }
        }
    }
}

impl std::error::Error for SerialMmioCaptureStateError {}

#[derive(Debug)]
pub enum SerialMmioCaptureError {
    BufferAllocation { source: TryReserveError },
    InvalidState { source: SerialMmioCaptureStateError },
}

impl fmt::Display for SerialMmioCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferAllocation { .. } => {
                formatter.write_str("serial capture buffer allocation failed")
            }
            Self::InvalidState { .. } => formatter.write_str("serial live state is invalid"),
        }
    }
}

impl std::error::Error for SerialMmioCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BufferAllocation { source } => Some(source),
            Self::InvalidState { source } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialMmioLegacyStateError {
    ReceiveStateNotRepresentable,
}

impl fmt::Display for SerialMmioLegacyStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("serial receive state is not representable by the legacy projection")
    }
}

impl std::error::Error for SerialMmioLegacyStateError {}

#[derive(Clone)]
pub struct SerialMmioDevice<O> {
    output: O,
    metrics: SharedSerialOutputMetrics,
    interrupt_enable: u8,
    interrupt_identification: u8,
    line_control: u8,
    line_status: u8,
    modem_control: u8,
    modem_status: u8,
    scratch: u8,
    divisor_latch_low: u8,
    divisor_latch_high: u8,
    receive_fifo: VecDeque<u8>,
    receive_interrupt_intent_pending: bool,
    input_ready_intent_pending: bool,
}

impl<O: fmt::Debug> fmt::Debug for SerialMmioDevice<O> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SerialMmioDevice")
            .field("output", &self.output)
            .field("state", &"<redacted>")
            .finish()
    }
}

impl<O: PartialEq> PartialEq for SerialMmioDevice<O> {
    fn eq(&self, other: &Self) -> bool {
        self.output == other.output
            && self.interrupt_enable == other.interrupt_enable
            && self.interrupt_identification == other.interrupt_identification
            && self.line_control == other.line_control
            && self.line_status == other.line_status
            && self.modem_control == other.modem_control
            && self.modem_status == other.modem_status
            && self.scratch == other.scratch
            && self.divisor_latch_low == other.divisor_latch_low
            && self.divisor_latch_high == other.divisor_latch_high
            && self.receive_fifo == other.receive_fifo
            && self.receive_interrupt_intent_pending == other.receive_interrupt_intent_pending
            && self.input_ready_intent_pending == other.input_ready_intent_pending
    }
}

impl<O: Eq> Eq for SerialMmioDevice<O> {}

impl<O> SerialMmioDevice<O> {
    pub fn new(output: O) -> Self {
        Self::new_with_metrics(output, SharedSerialOutputMetrics::default())
    }

    fn new_with_metrics(output: O, metrics: SharedSerialOutputMetrics) -> Self {
        Self {
            output,
            metrics,
            interrupt_enable: 0,
            interrupt_identification: SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING,
            line_control: 0,
            line_status: SERIAL_LINE_STATUS_DEFAULT,
            modem_control: 0,
            modem_status: 0,
            scratch: 0,
            divisor_latch_low: 0,
            divisor_latch_high: 0,
            receive_fifo: VecDeque::new(),
            receive_interrupt_intent_pending: false,
            input_ready_intent_pending: false,
        }
    }

    pub fn from_state(output: O, state: SerialMmioState) -> Self {
        Self::from_state_with_metrics(output, state, SharedSerialOutputMetrics::default())
    }

    fn from_state_with_metrics(
        output: O,
        state: SerialMmioState,
        metrics: SharedSerialOutputMetrics,
    ) -> Self {
        Self {
            output,
            metrics,
            interrupt_enable: state.interrupt_enable,
            interrupt_identification: SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING,
            line_control: state.line_control,
            line_status: SERIAL_LINE_STATUS_DEFAULT,
            modem_control: state.modem_control,
            modem_status: 0,
            scratch: state.scratch,
            divisor_latch_low: state.divisor_latch_low,
            divisor_latch_high: state.divisor_latch_high,
            receive_fifo: VecDeque::new(),
            receive_interrupt_intent_pending: false,
            input_ready_intent_pending: false,
        }
    }

    pub fn from_capture_state(output: O, state: SerialMmioCaptureState) -> Self {
        Self::from_capture_state_with_metrics(output, state, SharedSerialOutputMetrics::default())
    }

    fn from_capture_state_with_metrics(
        output: O,
        state: SerialMmioCaptureState,
        metrics: SharedSerialOutputMetrics,
    ) -> Self {
        Self {
            output,
            metrics,
            interrupt_enable: state.legacy_state.interrupt_enable,
            interrupt_identification: state.interrupt_identification,
            line_control: state.legacy_state.line_control,
            line_status: state.line_status,
            modem_control: state.legacy_state.modem_control,
            modem_status: state.modem_status,
            scratch: state.legacy_state.scratch,
            divisor_latch_low: state.legacy_state.divisor_latch_low,
            divisor_latch_high: state.legacy_state.divisor_latch_high,
            receive_fifo: VecDeque::from(state.receive_bytes),
            receive_interrupt_intent_pending: state.receive_interrupt_intent_pending,
            input_ready_intent_pending: state.input_ready_intent_pending,
        }
    }

    pub const fn state(&self) -> SerialMmioState {
        SerialMmioState::new(
            self.interrupt_enable,
            self.line_control,
            self.modem_control,
            self.scratch,
            self.divisor_latch_low,
            self.divisor_latch_high,
        )
    }

    pub fn legacy_state(&self) -> Result<SerialMmioState, SerialMmioLegacyStateError> {
        if self.receive_fifo.is_empty()
            && self.interrupt_identification == SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING
            && self.line_status == SERIAL_LINE_STATUS_DEFAULT
            && self.modem_status == 0
            && !self.receive_interrupt_intent_pending
            && !self.input_ready_intent_pending
        {
            Ok(self.state())
        } else {
            Err(SerialMmioLegacyStateError::ReceiveStateNotRepresentable)
        }
    }

    pub fn capture_state(&self) -> Result<SerialMmioCaptureState, SerialMmioCaptureError> {
        self.capture_state_with(|bytes, len| bytes.try_reserve_exact(len))
    }

    fn capture_state_with(
        &self,
        reserve: impl FnOnce(&mut Vec<u8>, usize) -> Result<(), TryReserveError>,
    ) -> Result<SerialMmioCaptureState, SerialMmioCaptureError> {
        let mut receive_bytes = Vec::new();
        if let Err(source) = reserve(&mut receive_bytes, self.receive_fifo.len()) {
            self.metrics.record_error();
            return Err(SerialMmioCaptureError::BufferAllocation { source });
        }
        receive_bytes.extend(self.receive_fifo.iter().copied());
        SerialMmioCaptureState::try_from_parts(SerialMmioCaptureStateParts {
            legacy_state: self.state(),
            interrupt_identification: self.interrupt_identification,
            line_status: self.line_status,
            modem_status: self.modem_status,
            receive_bytes,
            receive_interrupt_intent_pending: self.receive_interrupt_intent_pending,
            input_ready_intent_pending: self.input_ready_intent_pending,
        })
        .map_err(|source| SerialMmioCaptureError::InvalidState { source })
    }

    pub const fn output(&self) -> &O {
        &self.output
    }

    pub fn output_mut(&mut self) -> &mut O {
        &mut self.output
    }

    pub fn into_output(self) -> O {
        self.output
    }

    pub fn metrics(&self) -> SerialOutputMetrics {
        self.metrics.snapshot()
    }

    pub const fn interrupt_enable(&self) -> u8 {
        self.interrupt_enable
    }

    pub const fn interrupt_identification(&self) -> u8 {
        self.interrupt_identification
    }

    pub const fn line_control(&self) -> u8 {
        self.line_control
    }

    pub const fn line_status(&self) -> u8 {
        self.line_status
    }

    pub const fn modem_control(&self) -> u8 {
        self.modem_control
    }

    pub const fn modem_status(&self) -> u8 {
        self.modem_status
    }

    pub const fn scratch(&self) -> u8 {
        self.scratch
    }

    pub const fn divisor_latch_low(&self) -> u8 {
        self.divisor_latch_low
    }

    pub const fn divisor_latch_high(&self) -> u8 {
        self.divisor_latch_high
    }

    pub const fn divisor_latch_access_enabled(&self) -> bool {
        (self.line_control & SERIAL_LINE_CONTROL_DLAB) != 0
    }

    pub fn receive_fifo_len(&self) -> usize {
        self.receive_fifo.len()
    }

    pub fn receive_fifo_capacity(&self) -> usize {
        SERIAL_RECEIVE_FIFO_CAPACITY - self.receive_fifo.len()
    }

    pub const fn pending_interrupt_intent(&self) -> Option<SerialInterruptIntent> {
        if self.receive_interrupt_intent_pending {
            Some(SerialInterruptIntent::ReceivedDataAvailable)
        } else {
            None
        }
    }

    pub fn take_interrupt_intent(&mut self) -> Option<SerialInterruptIntent> {
        let intent = self.pending_interrupt_intent();
        self.receive_interrupt_intent_pending = false;
        intent
    }

    pub const fn pending_input_ready_intent(&self) -> Option<SerialInputReadyIntent> {
        if self.input_ready_intent_pending {
            Some(SerialInputReadyIntent::ReceiveBufferEmpty)
        } else {
            None
        }
    }

    pub fn take_input_ready_intent(&mut self) -> Option<SerialInputReadyIntent> {
        let intent = self.pending_input_ready_intent();
        self.input_ready_intent_pending = false;
        intent
    }

    pub fn inject_receive_bytes(
        &mut self,
        input: &[u8],
    ) -> Result<SerialReceiveInjectionOutcome, SerialReceiveInjectionError> {
        self.inject_receive_bytes_with(input, |fifo, len| fifo.try_reserve_exact(len))
    }

    fn inject_receive_bytes_with(
        &mut self,
        input: &[u8],
        reserve: impl FnOnce(&mut VecDeque<u8>, usize) -> Result<(), TryReserveError>,
    ) -> Result<SerialReceiveInjectionOutcome, SerialReceiveInjectionError> {
        if input.is_empty() {
            return Ok(SerialReceiveInjectionOutcome::new(
                0,
                0,
                self.receive_fifo_capacity(),
            ));
        }

        let accepted_bytes = self.receive_fifo_capacity().min(input.len());
        let rejected_bytes = input.len() - accepted_bytes;
        if accepted_bytes != 0
            && let Err(source) = reserve(&mut self.receive_fifo, accepted_bytes)
        {
            self.metrics.record_error();
            return Err(SerialReceiveInjectionError::BufferAllocation { source });
        }

        if accepted_bytes != 0 {
            self.input_ready_intent_pending = false;
            self.receive_fifo
                .extend(input.iter().copied().take(accepted_bytes));
            self.line_status |= SERIAL_LINE_STATUS_DATA_READY;
        }
        if rejected_bytes != 0 {
            self.line_status |= SERIAL_LINE_STATUS_OVERRUN_ERROR;
        }

        let should_publish_interrupt = accepted_bytes != 0
            && self.interrupt_enable & SERIAL_INTERRUPT_ENABLE_RECEIVED_DATA_AVAILABLE != 0
            && self.interrupt_identification
                != SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE;
        if should_publish_interrupt {
            self.interrupt_identification = SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE;
        }

        self.metrics
            .record_input(saturating_usize_to_u64(accepted_bytes));
        self.metrics
            .record_overrun(saturating_usize_to_u64(rejected_bytes));
        if should_publish_interrupt {
            self.publish_receive_interrupt_intent();
        }

        Ok(SerialReceiveInjectionOutcome::new(
            accepted_bytes,
            rejected_bytes,
            self.receive_fifo_capacity(),
        ))
    }

    fn publish_receive_interrupt_intent(&mut self) {
        if !self.receive_interrupt_intent_pending {
            self.receive_interrupt_intent_pending = true;
            self.metrics.record_interrupt();
        }
    }

    fn publish_input_ready_intent(&mut self) {
        self.input_ready_intent_pending = true;
    }

    fn acknowledge_receive_interrupt(&mut self) {
        if self.interrupt_identification == SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE
        {
            self.interrupt_identification = SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING;
        }
        self.receive_interrupt_intent_pending = false;
    }

    fn read_receive_byte(&mut self) -> u8 {
        let had_receive_data = !self.receive_fifo.is_empty();
        self.acknowledge_receive_interrupt();
        let byte = self.receive_fifo.pop_front().unwrap_or_default();
        if self.receive_fifo.is_empty() {
            self.line_status &= !SERIAL_LINE_STATUS_DATA_READY;
            if had_receive_data {
                self.publish_input_ready_intent();
            }
        }
        self.metrics.record_read();
        byte
    }

    fn read_interrupt_identification(&mut self) -> u8 {
        let value = self.interrupt_identification | SERIAL_INTERRUPT_IDENTIFICATION_FIFO_ENABLED;
        self.acknowledge_receive_interrupt();
        value
    }

    fn read_line_status(&mut self) -> u8 {
        let value = self.line_status;
        self.line_status &= !SERIAL_LINE_STATUS_OVERRUN_ERROR;
        value
    }

    fn clear_receive_fifo(&mut self) {
        let had_receive_data = !self.receive_fifo.is_empty();
        self.receive_fifo.clear();
        self.line_status &= !(SERIAL_LINE_STATUS_DATA_READY | SERIAL_LINE_STATUS_OVERRUN_ERROR);
        self.acknowledge_receive_interrupt();
        self.metrics.record_flush();
        if had_receive_data {
            self.publish_input_ready_intent();
        }
    }
}

impl SerialMmioDevice<SerialOutputBuffer> {
    pub fn buffered() -> Self {
        Self::buffered_with_limit(SERIAL_OUTPUT_BUFFER_DEFAULT_LIMIT)
    }

    pub fn buffered_with_limit(limit: usize) -> Self {
        Self::new(SerialOutputBuffer::new(limit))
    }
}

impl SerialMmioDevice<DiscardSerialOutput> {
    pub fn discarding() -> Self {
        Self::new(DiscardSerialOutput)
    }
}

impl SerialMmioDevice<SharedSerialOutput> {
    pub fn with_shared_output(output: SharedSerialOutput) -> Self {
        let metrics = output.metrics.clone();
        Self::new_with_metrics(output, metrics)
    }

    pub fn from_state_with_shared_output(
        output: SharedSerialOutput,
        state: SerialMmioState,
    ) -> Self {
        let metrics = output.metrics.clone();
        Self::from_state_with_metrics(output, state, metrics)
    }

    pub fn from_capture_state_with_shared_output(
        output: SharedSerialOutput,
        state: SerialMmioCaptureState,
    ) -> Self {
        let metrics = output.metrics.clone();
        Self::from_capture_state_with_metrics(output, state, metrics)
    }
}

impl<O: SerialOutput> SerialMmioDevice<O> {
    pub fn read_byte(&mut self, offset: u64) -> Result<u8, SerialMmioError> {
        validate_register_offset(offset)?;

        match offset {
            SERIAL_TRANSMIT_REGISTER_OFFSET if self.divisor_latch_access_enabled() => {
                Ok(self.divisor_latch_low)
            }
            SERIAL_TRANSMIT_REGISTER_OFFSET => Ok(self.read_receive_byte()),
            SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET if self.divisor_latch_access_enabled() => {
                Ok(self.divisor_latch_high)
            }
            SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET => Ok(self.interrupt_enable),
            SERIAL_INTERRUPT_IDENTIFICATION_REGISTER_OFFSET => {
                Ok(self.read_interrupt_identification())
            }
            SERIAL_LINE_CONTROL_REGISTER_OFFSET => Ok(self.line_control),
            SERIAL_MODEM_CONTROL_REGISTER_OFFSET => Ok(self.modem_control),
            SERIAL_LINE_STATUS_REGISTER_OFFSET => Ok(self.read_line_status()),
            SERIAL_MODEM_STATUS_REGISTER_OFFSET => Ok(self.modem_status),
            SERIAL_SCRATCH_REGISTER_OFFSET => Ok(self.scratch),
            _ => Err(SerialMmioError::InvalidRegisterOffset {
                offset,
                register_space_size: SERIAL_REGISTER_SPACE_SIZE,
            }),
        }
    }

    pub fn write_byte(&mut self, offset: u64, value: u8) -> Result<(), SerialMmioError> {
        validate_register_offset(offset)?;

        match offset {
            SERIAL_TRANSMIT_REGISTER_OFFSET if self.divisor_latch_access_enabled() => {
                self.divisor_latch_low = value;
                Ok(())
            }
            SERIAL_TRANSMIT_REGISTER_OFFSET => self
                .output
                .write_byte(value)
                .map_err(|source| SerialMmioError::Output { source }),
            SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET if self.divisor_latch_access_enabled() => {
                self.divisor_latch_high = value;
                Ok(())
            }
            SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET => {
                self.interrupt_enable = value;
                Ok(())
            }
            SERIAL_FIFO_CONTROL_REGISTER_OFFSET => {
                if value & SERIAL_FIFO_CONTROL_CLEAR_RECEIVE != 0 {
                    self.clear_receive_fifo();
                }
                Ok(())
            }
            SERIAL_LINE_CONTROL_REGISTER_OFFSET => {
                self.line_control = value;
                Ok(())
            }
            SERIAL_MODEM_CONTROL_REGISTER_OFFSET => {
                self.modem_control = value;
                Ok(())
            }
            SERIAL_SCRATCH_REGISTER_OFFSET => {
                self.scratch = value;
                Ok(())
            }
            SERIAL_LINE_STATUS_REGISTER_OFFSET | SERIAL_MODEM_STATUS_REGISTER_OFFSET => {
                Err(SerialMmioError::ReadOnlyRegisterWrite { offset })
            }
            _ => Err(SerialMmioError::InvalidRegisterOffset {
                offset,
                register_space_size: SERIAL_REGISTER_SPACE_SIZE,
            }),
        }
    }
}

impl<O: SerialOutput> MmioHandler for SerialMmioDevice<O> {
    fn read(&mut self, access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
        self.read_access(access).map_err(|source| {
            self.metrics.record_missed_read();
            MmioHandlerError::from(source)
        })
    }

    fn write(&mut self, access: MmioAccess, data: MmioAccessBytes) -> Result<(), MmioHandlerError> {
        self.write_access(access, data).map_err(|source| {
            if !matches!(source, SerialMmioError::Output { .. }) {
                self.metrics.record_missed_write();
            }
            MmioHandlerError::from(source)
        })
    }
}

impl<O: SerialOutput> SerialMmioDevice<O> {
    fn read_access(&mut self, access: MmioAccess) -> Result<MmioAccessBytes, SerialMmioError> {
        let offset = validate_byte_access(access)?;
        let value = self.read_byte(offset)?;

        MmioAccessBytes::new(&[value]).map_err(|source| SerialMmioError::BuildReadBytes { source })
    }

    fn write_access(
        &mut self,
        access: MmioAccess,
        data: MmioAccessBytes,
    ) -> Result<(), SerialMmioError> {
        let offset = validate_byte_access(access)?;
        let value = single_data_byte(offset, data)?;

        self.write_byte(offset, value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialMmioError {
    UnsupportedAccessSize {
        offset: u64,
        size: u64,
    },
    UnsupportedWriteDataLength {
        offset: u64,
        len: usize,
    },
    InvalidRegisterOffset {
        offset: u64,
        register_space_size: u64,
    },
    ReadOnlyRegisterWrite {
        offset: u64,
    },
    BuildReadBytes {
        source: MmioAccessBytesError,
    },
    Output {
        source: SerialOutputError,
    },
}

impl fmt::Display for SerialMmioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAccessSize { offset, size } => {
                write!(
                    f,
                    "serial MMIO offset 0x{offset:x} only supports 1-byte accesses, got {size} bytes"
                )
            }
            Self::UnsupportedWriteDataLength { offset, len } => {
                write!(
                    f,
                    "serial MMIO offset 0x{offset:x} write requires 1 data byte, got {len}"
                )
            }
            Self::InvalidRegisterOffset {
                offset,
                register_space_size,
            } => {
                write!(
                    f,
                    "serial MMIO offset 0x{offset:x} is outside the {register_space_size}-byte register space"
                )
            }
            Self::ReadOnlyRegisterWrite { offset } => {
                write!(f, "serial MMIO offset 0x{offset:x} is read-only")
            }
            Self::BuildReadBytes { source } => {
                write!(f, "failed to build serial MMIO read bytes: {source}")
            }
            Self::Output { source } => write!(f, "serial output failed: {source}"),
        }
    }
}

impl std::error::Error for SerialMmioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BuildReadBytes { source } => Some(source),
            Self::Output { source } => Some(source),
            Self::UnsupportedAccessSize { .. }
            | Self::UnsupportedWriteDataLength { .. }
            | Self::InvalidRegisterOffset { .. }
            | Self::ReadOnlyRegisterWrite { .. } => None,
        }
    }
}

impl From<SerialMmioError> for MmioHandlerError {
    fn from(source: SerialMmioError) -> Self {
        Self::new(source.to_string())
    }
}

fn validate_register_offset(offset: u64) -> Result<(), SerialMmioError> {
    if offset < SERIAL_REGISTER_SPACE_SIZE {
        Ok(())
    } else {
        Err(SerialMmioError::InvalidRegisterOffset {
            offset,
            register_space_size: SERIAL_REGISTER_SPACE_SIZE,
        })
    }
}

fn validate_byte_access(access: MmioAccess) -> Result<u64, SerialMmioError> {
    let offset = access.offset();
    let size = access.range().size();
    if size == 1 {
        Ok(offset)
    } else {
        Err(SerialMmioError::UnsupportedAccessSize { offset, size })
    }
}

fn single_data_byte(offset: u64, data: MmioAccessBytes) -> Result<u8, SerialMmioError> {
    if data.len() != 1 {
        return Err(SerialMmioError::UnsupportedWriteDataLength {
            offset,
            len: data.len(),
        });
    }

    data.as_slice()
        .first()
        .copied()
        .ok_or(SerialMmioError::UnsupportedWriteDataLength {
            offset,
            len: data.len(),
        })
}

fn has_control_character(value: &str) -> bool {
    value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read as _, Write as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::{
        RateLimitedSerialOutput, SERIAL_FIFO_CONTROL_CLEAR_RECEIVE,
        SERIAL_FIFO_CONTROL_REGISTER_OFFSET, SERIAL_INTERRUPT_ENABLE_RECEIVED_DATA_AVAILABLE,
        SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET, SERIAL_INTERRUPT_IDENTIFICATION_FIFO_ENABLED,
        SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING,
        SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE,
        SERIAL_INTERRUPT_IDENTIFICATION_REGISTER_OFFSET, SERIAL_LINE_CONTROL_DLAB,
        SERIAL_LINE_CONTROL_REGISTER_OFFSET, SERIAL_LINE_STATUS_DATA_READY,
        SERIAL_LINE_STATUS_DEFAULT, SERIAL_LINE_STATUS_OVERRUN_ERROR,
        SERIAL_LINE_STATUS_REGISTER_OFFSET, SERIAL_MMIO_DEVICE_WINDOW_SIZE,
        SERIAL_MODEM_CONTROL_REGISTER_OFFSET, SERIAL_MODEM_STATUS_REGISTER_OFFSET,
        SERIAL_OUTPUT_BUFFER_DEFAULT_LIMIT, SERIAL_RECEIVE_FIFO_CAPACITY,
        SERIAL_SCRATCH_REGISTER_OFFSET, SERIAL_TRANSMIT_REGISTER_OFFSET, SerialConfigError,
        SerialConfigInput, SerialInputReadyIntent, SerialInterruptIntent, SerialMmioCaptureError,
        SerialMmioCaptureState, SerialMmioCaptureStateError, SerialMmioCaptureStateParts,
        SerialMmioDevice, SerialMmioError, SerialMmioLegacyStateError, SerialMmioState,
        SerialOutput, SerialOutputBuffer, SerialOutputError, SerialOutputFile, SerialOutputMetrics,
        SerialRateLimiterConfig, SerialStdio, SharedSerialOutput, SharedSerialOutputBuffer,
        SharedSerialOutputMetrics,
    };
    use crate::memory::GuestAddress;
    use crate::mmio::{
        MmioAccess, MmioAccessBytes, MmioDispatchError, MmioDispatchOutcome, MmioDispatcher,
        MmioHandler, MmioOperation, MmioRegionId,
    };
    use crate::token_bucket::{TokenBucket, TokenBucketConfig};
    use vm_superio::{Serial as VmSuperioSerial, Trigger};

    fn access(offset: u64, size: u64) -> MmioAccess {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                MmioRegionId::new(7),
                GuestAddress::new(0x1000),
                SERIAL_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("test region should insert");
        dispatcher
            .lookup(GuestAddress::new(0x1000 + offset), size)
            .expect("test lookup should succeed")
    }

    #[test]
    fn serial_mmio_state_round_trips_into_fresh_output() {
        let state = SerialMmioState::new(1, 0x83, 3, 0x5a, 0x34, 0x12);
        let device = SerialMmioDevice::from_state(SerialOutputBuffer::default(), state);

        assert_eq!(device.state(), state);
        assert!(device.output().bytes().is_empty());
        assert_eq!(
            format!("{state:?}"),
            "SerialMmioState { registers: \"<redacted>\" }"
        );
        assert!(!format!("{state:?}").contains("5a"));
    }

    fn bytes(data: &[u8]) -> MmioAccessBytes {
        MmioAccessBytes::new(data).expect("test bytes should be valid")
    }

    fn unique_serial_output_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "bangbang-serial-test-{}-{nanos}-{name}",
            std::process::id()
        ))
    }

    fn pipe_files() -> (File, File) {
        let mut descriptors = [-1; 2];
        // SAFETY: `descriptors` provides space for both descriptors returned by
        // a successful `pipe` call.
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0);
        // SAFETY: A successful `pipe` returned two fresh owned descriptors.
        let reader = unsafe { File::from_raw_fd(descriptors[0]) };
        // SAFETY: A successful `pipe` returned two fresh owned descriptors.
        let writer = unsafe { File::from_raw_fd(descriptors[1]) };
        (reader, writer)
    }

    #[test]
    fn serial_stdio_pipe_round_trip_and_final_drop_restore_status_flags() {
        let (input_reader, mut input_writer) = pipe_files();
        let (mut output_reader, output_writer) = pipe_files();
        let original_input_flags =
            super::descriptor_status_flags(input_reader.as_raw_fd()).expect("input flags");
        let original_output_flags =
            super::descriptor_status_flags(output_writer.as_raw_fd()).expect("output flags");

        let (mut output, input) =
            SerialStdio::from_descriptors(input_reader.as_raw_fd(), output_writer.as_raw_fd())
                .expect("pipe stdio should prepare")
                .into_parts();
        let mut input = input.expect("pipe stdin should attach");

        assert_ne!(
            super::descriptor_status_flags(input_reader.as_raw_fd()).expect("input flags")
                & libc::O_NONBLOCK,
            0
        );
        assert_ne!(
            super::descriptor_status_flags(output_writer.as_raw_fd()).expect("output flags")
                & libc::O_NONBLOCK,
            0
        );

        output.write_byte(b'O').expect("stdout byte should write");
        let mut output_byte = [0];
        output_reader
            .read_exact(&mut output_byte)
            .expect("stdout pipe should receive byte");
        assert_eq!(output_byte, *b"O");

        input_writer
            .write_all(b"input")
            .expect("stdin pipe should accept bytes");
        let mut input_bytes = [0; 5];
        assert_eq!(
            input.read(&mut input_bytes).expect("stdin should read"),
            input_bytes.len()
        );
        assert_eq!(&input_bytes, b"input");

        assert_eq!(Arc::strong_count(&output.descriptors), 2);
        drop(output);
        assert_eq!(Arc::strong_count(&input.descriptors), 1);
        assert_ne!(
            super::descriptor_status_flags(output_writer.as_raw_fd()).expect("output flags")
                & libc::O_NONBLOCK,
            0,
            "the remaining input owner must retain the shared stdio lifetime"
        );
        drop(input);
        assert_eq!(
            super::descriptor_status_flags(input_reader.as_raw_fd()).expect("restored input flags")
                & (libc::O_ACCMODE | libc::O_NONBLOCK),
            original_input_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)
        );
        assert_eq!(
            super::descriptor_status_flags(output_writer.as_raw_fd())
                .expect("restored output flags")
                & (libc::O_ACCMODE | libc::O_NONBLOCK),
            original_output_flags & (libc::O_ACCMODE | libc::O_NONBLOCK)
        );
    }

    #[test]
    fn serial_stdio_ignores_non_pollable_or_invalid_input() {
        let unsupported_input = File::open("/dev/null").expect("null input should open");
        let (_output_reader, output_writer) = pipe_files();

        let (_, input) =
            SerialStdio::from_descriptors(unsupported_input.as_raw_fd(), output_writer.as_raw_fd())
                .expect("unsupported stdin should not prevent stdout setup")
                .into_parts();
        assert!(input.is_none());

        let (_, input) = SerialStdio::from_descriptors(-1, output_writer.as_raw_fd())
            .expect("closed stdin should not prevent stdout setup")
            .into_parts();
        assert!(input.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn serial_stdio_terminal_is_raw_until_final_drop_then_restored() {
        let mut master_descriptor = -1;
        let mut slave_descriptor = -1;
        assert_eq!(
            // SAFETY: Both output pointers are valid and null optional settings
            // ask `openpty` to apply its defaults.
            unsafe {
                libc::openpty(
                    &raw mut master_descriptor,
                    &raw mut slave_descriptor,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        // SAFETY: Successful `openpty` returned fresh owned descriptors.
        let _master = unsafe { File::from_raw_fd(master_descriptor) };
        // SAFETY: Successful `openpty` returned a fresh owned descriptor.
        let slave = unsafe { File::from_raw_fd(slave_descriptor) };
        let original = super::terminal_attributes(slave.as_raw_fd()).expect("terminal state");

        let (output, input) = SerialStdio::from_descriptors(slave.as_raw_fd(), slave.as_raw_fd())
            .expect("terminal stdio should prepare")
            .into_parts();
        let input = input.expect("terminal stdin should attach");
        let raw = super::terminal_attributes(slave.as_raw_fd()).expect("raw terminal state");
        assert_eq!(raw.c_lflag & (libc::ICANON | libc::ECHO), 0);

        drop(output);
        assert_eq!(
            super::terminal_attributes(slave.as_raw_fd())
                .expect("retained raw terminal state")
                .c_lflag
                & (libc::ICANON | libc::ECHO),
            0
        );
        drop(input);
        let restored =
            super::terminal_attributes(slave.as_raw_fd()).expect("restored terminal state");
        assert_eq!(restored.c_iflag, original.c_iflag);
        assert_eq!(restored.c_oflag, original.c_oflag);
        assert_eq!(restored.c_cflag, original.c_cflag);
        assert_eq!(
            restored.c_lflag & !libc::PENDIN,
            original.c_lflag & !libc::PENDIN
        );
        assert_eq!(restored.c_cc, original.c_cc);
        assert_eq!(restored.c_ispeed, original.c_ispeed);
        assert_eq!(restored.c_ospeed, original.c_ospeed);
    }

    fn write(
        device: &mut SerialMmioDevice<SerialOutputBuffer>,
        offset: u64,
        value: u8,
    ) -> Result<(), crate::mmio::MmioHandlerError> {
        device.write(access(offset, 1), bytes(&[value]))
    }

    fn read(device: &mut SerialMmioDevice<SerialOutputBuffer>, offset: u64) -> Vec<u8> {
        device
            .read(access(offset, 1))
            .expect("test read should succeed")
            .as_slice()
            .to_vec()
    }

    #[derive(Debug, Clone, Default)]
    struct RecordingTrigger {
        count: Arc<AtomicU64>,
    }

    impl RecordingTrigger {
        fn count(&self) -> u64 {
            self.count.load(Ordering::Relaxed)
        }
    }

    impl Trigger for RecordingTrigger {
        type E = io::Error;

        fn trigger(&self) -> Result<(), Self::E> {
            self.count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn assert_receive_invariants<O>(device: &SerialMmioDevice<O>) {
        assert!(device.receive_fifo_len() <= SERIAL_RECEIVE_FIFO_CAPACITY);
        assert_eq!(
            device.line_status() & SERIAL_LINE_STATUS_DATA_READY != 0,
            device.receive_fifo_len() != 0
        );
        assert_eq!(
            device.line_status() & SERIAL_LINE_STATUS_DEFAULT,
            SERIAL_LINE_STATUS_DEFAULT
        );
        assert_eq!(
            device.line_status()
                & !(SERIAL_LINE_STATUS_DEFAULT
                    | SERIAL_LINE_STATUS_DATA_READY
                    | SERIAL_LINE_STATUS_OVERRUN_ERROR),
            0
        );
        assert!(matches!(
            device.interrupt_identification(),
            SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING
                | SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE
        ));
        if device.interrupt_identification()
            == SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE
        {
            assert_ne!(device.receive_fifo_len(), 0);
        }
        if device.pending_interrupt_intent().is_some() {
            assert_eq!(
                device.interrupt_identification(),
                SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE
            );
        }
        if device.pending_input_ready_intent().is_some() {
            assert_eq!(device.receive_fifo_len(), 0);
        }
    }

    fn valid_capture_parts() -> SerialMmioCaptureStateParts {
        SerialMmioCaptureStateParts {
            legacy_state: SerialMmioState::new(0, 0, 0, 0, 0, 0),
            interrupt_identification: SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING,
            line_status: SERIAL_LINE_STATUS_DEFAULT,
            modem_status: 0,
            receive_bytes: Vec::new(),
            receive_interrupt_intent_pending: false,
            input_ready_intent_pending: false,
        }
    }

    fn allocation_failure() -> std::collections::TryReserveError {
        let mut bytes = Vec::<u8>::new();
        bytes
            .try_reserve_exact(usize::MAX)
            .expect_err("maximum reserve should fail")
    }

    #[test]
    fn transmit_register_write_appends_output_byte() {
        let mut device = SerialMmioDevice::buffered();

        write(&mut device, SERIAL_TRANSMIT_REGISTER_OFFSET, b'A')
            .expect("transmit write should succeed");
        write(&mut device, SERIAL_TRANSMIT_REGISTER_OFFSET, b'B')
            .expect("transmit write should succeed");

        assert_eq!(device.output().bytes(), b"AB");
    }

    #[test]
    fn transmit_writes_preserve_device_instance_boundaries() {
        let mut first = SerialMmioDevice::buffered();
        let mut second = SerialMmioDevice::buffered();

        write(&mut first, SERIAL_TRANSMIT_REGISTER_OFFSET, b'a')
            .expect("first transmit should succeed");
        write(&mut second, SERIAL_TRANSMIT_REGISTER_OFFSET, b'b')
            .expect("second transmit should succeed");

        assert_eq!(first.output().bytes(), b"a");
        assert_eq!(second.output().bytes(), b"b");
    }

    #[test]
    fn divisor_latch_access_does_not_transmit_bytes() {
        let mut device = SerialMmioDevice::buffered();

        write(
            &mut device,
            SERIAL_LINE_CONTROL_REGISTER_OFFSET,
            SERIAL_LINE_CONTROL_DLAB,
        )
        .expect("line control write should succeed");
        write(&mut device, SERIAL_TRANSMIT_REGISTER_OFFSET, 0x34)
            .expect("divisor low write should succeed");
        write(&mut device, SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET, 0x12)
            .expect("divisor high write should succeed");

        assert_eq!(device.output().bytes(), b"");
        assert_eq!(read(&mut device, SERIAL_TRANSMIT_REGISTER_OFFSET), [0x34]);
        assert_eq!(
            read(&mut device, SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET),
            [0x12]
        );
        assert_eq!(device.divisor_latch_low(), 0x34);
        assert_eq!(device.divisor_latch_high(), 0x12);
    }

    #[test]
    fn line_control_clear_resumes_transmit_writes() {
        let mut device = SerialMmioDevice::buffered();

        write(
            &mut device,
            SERIAL_LINE_CONTROL_REGISTER_OFFSET,
            SERIAL_LINE_CONTROL_DLAB,
        )
        .expect("line control write should succeed");
        write(&mut device, SERIAL_TRANSMIT_REGISTER_OFFSET, 0x34)
            .expect("divisor low write should succeed");
        write(&mut device, SERIAL_LINE_CONTROL_REGISTER_OFFSET, 0)
            .expect("line control write should succeed");
        write(&mut device, SERIAL_TRANSMIT_REGISTER_OFFSET, b'Z')
            .expect("transmit write should succeed");

        assert_eq!(device.output().bytes(), b"Z");
    }

    #[test]
    fn supported_reads_return_uart_shaped_status_values() {
        let mut device = SerialMmioDevice::buffered();

        assert_eq!(read(&mut device, SERIAL_TRANSMIT_REGISTER_OFFSET), [0]);
        assert_eq!(
            read(&mut device, SERIAL_INTERRUPT_IDENTIFICATION_REGISTER_OFFSET),
            [SERIAL_INTERRUPT_IDENTIFICATION_FIFO_ENABLED
                | SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING]
        );
        assert_eq!(
            read(&mut device, SERIAL_LINE_STATUS_REGISTER_OFFSET),
            [SERIAL_LINE_STATUS_DEFAULT]
        );
        assert_eq!(read(&mut device, SERIAL_MODEM_STATUS_REGISTER_OFFSET), [0]);
    }

    #[test]
    fn writable_registers_round_trip_state() {
        let mut device = SerialMmioDevice::buffered();

        write(&mut device, SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET, 0x03)
            .expect("interrupt enable write should succeed");
        write(&mut device, SERIAL_LINE_CONTROL_REGISTER_OFFSET, 0x1b)
            .expect("line control write should succeed");
        write(&mut device, SERIAL_MODEM_CONTROL_REGISTER_OFFSET, 0x0b)
            .expect("modem control write should succeed");
        write(&mut device, SERIAL_SCRATCH_REGISTER_OFFSET, 0xa5)
            .expect("scratch write should succeed");

        assert_eq!(
            read(&mut device, SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET),
            [0x03]
        );
        assert_eq!(
            read(&mut device, SERIAL_LINE_CONTROL_REGISTER_OFFSET),
            [0x1b]
        );
        assert_eq!(
            read(&mut device, SERIAL_MODEM_CONTROL_REGISTER_OFFSET),
            [0x0b]
        );
        assert_eq!(read(&mut device, SERIAL_SCRATCH_REGISTER_OFFSET), [0xa5]);
        assert_eq!(device.interrupt_enable(), 0x03);
        assert_eq!(device.line_control(), 0x1b);
        assert_eq!(device.modem_control(), 0x0b);
        assert_eq!(device.scratch(), 0xa5);
    }

    #[test]
    fn fifo_control_write_is_accepted_without_output() {
        let mut device = SerialMmioDevice::buffered();

        write(
            &mut device,
            super::SERIAL_FIFO_CONTROL_REGISTER_OFFSET,
            0x07,
        )
        .expect("fifo control write should succeed");

        assert_eq!(device.output().bytes(), b"");
        assert_eq!(
            read(&mut device, SERIAL_INTERRUPT_IDENTIFICATION_REGISTER_OFFSET),
            [SERIAL_INTERRUPT_IDENTIFICATION_FIFO_ENABLED
                | SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING]
        );
    }

    #[test]
    fn receive_fifo_saturates_reports_overrun_and_recovers_in_order() {
        let mut device = SerialMmioDevice::buffered();
        let input: Vec<u8> = (0..=SERIAL_RECEIVE_FIFO_CAPACITY)
            .map(|value| value as u8)
            .collect();

        let outcome = device
            .inject_receive_bytes(&input)
            .expect("bounded receive injection should succeed");

        assert_eq!(outcome.accepted_bytes(), SERIAL_RECEIVE_FIFO_CAPACITY);
        assert_eq!(outcome.rejected_bytes(), 1);
        assert_eq!(outcome.remaining_capacity(), 0);
        assert!(outcome.is_backpressured());
        assert_eq!(device.receive_fifo_len(), SERIAL_RECEIVE_FIFO_CAPACITY);
        assert_eq!(
            device.read_byte(SERIAL_LINE_STATUS_REGISTER_OFFSET),
            Ok(SERIAL_LINE_STATUS_DEFAULT
                | SERIAL_LINE_STATUS_DATA_READY
                | SERIAL_LINE_STATUS_OVERRUN_ERROR)
        );
        assert_eq!(
            device.read_byte(SERIAL_LINE_STATUS_REGISTER_OFFSET),
            Ok(SERIAL_LINE_STATUS_DEFAULT | SERIAL_LINE_STATUS_DATA_READY)
        );

        assert_eq!(device.read_byte(SERIAL_TRANSMIT_REGISTER_OFFSET), Ok(0));
        assert_eq!(device.receive_fifo_capacity(), 1);
        assert_eq!(device.pending_input_ready_intent(), None);

        let recovery = device
            .inject_receive_bytes(&[64, 65])
            .expect("one recovered slot should accept one byte");
        assert_eq!(recovery.accepted_bytes(), 1);
        assert_eq!(recovery.rejected_bytes(), 1);
        assert!(recovery.is_backpressured());

        for expected in 1..=64 {
            assert_eq!(
                device.read_byte(SERIAL_TRANSMIT_REGISTER_OFFSET),
                Ok(expected)
            );
        }
        assert_eq!(device.receive_fifo_len(), 0);
        assert_eq!(
            device.take_input_ready_intent(),
            Some(SerialInputReadyIntent::ReceiveBufferEmpty)
        );
        assert_eq!(device.take_input_ready_intent(), None);
        assert_eq!(device.metrics().input_count(), 65);
        assert_eq!(device.metrics().overrun_count(), 2);
        assert_receive_invariants(&device);
    }

    #[test]
    fn receive_interrupt_intent_is_committed_coalesced_and_acknowledged() {
        let mut device = SerialMmioDevice::buffered();
        device
            .write_byte(
                SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET,
                SERIAL_INTERRUPT_ENABLE_RECEIVED_DATA_AVAILABLE,
            )
            .expect("IER write should succeed");

        device
            .inject_receive_bytes(b"abc")
            .expect("receive injection should succeed");
        assert_eq!(
            device.interrupt_identification(),
            SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE
        );
        assert_ne!(
            device.line_status() & SERIAL_LINE_STATUS_DATA_READY,
            0,
            "guest-visible data-ready state must commit before intent observation"
        );
        assert_eq!(
            device.take_interrupt_intent(),
            Some(SerialInterruptIntent::ReceivedDataAvailable)
        );
        assert_eq!(device.take_interrupt_intent(), None);

        device
            .inject_receive_bytes(b"d")
            .expect("coalesced injection should succeed");
        assert_eq!(device.pending_interrupt_intent(), None);
        assert_eq!(device.metrics().interrupt_count(), 1);
        assert_eq!(
            device.read_byte(SERIAL_INTERRUPT_IDENTIFICATION_REGISTER_OFFSET),
            Ok(SERIAL_INTERRUPT_IDENTIFICATION_FIFO_ENABLED
                | SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE)
        );
        assert_eq!(
            device.interrupt_identification(),
            SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING
        );

        device
            .inject_receive_bytes(b"e")
            .expect("new input after IIR acknowledgement should interrupt");
        assert_eq!(device.metrics().interrupt_count(), 2);
        device
            .write_byte(SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET, 0)
            .expect("IER disable should succeed");
        let capture = device
            .capture_state()
            .expect("latched RDA after IER disable is valid capture state");
        assert_eq!(capture.legacy_state().interrupt_enable(), 0);
        assert_eq!(
            capture.interrupt_identification(),
            SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE
        );

        assert_eq!(device.read_byte(SERIAL_TRANSMIT_REGISTER_OFFSET), Ok(b'a'));
        assert_eq!(device.pending_interrupt_intent(), None);
        assert_eq!(
            device.interrupt_identification(),
            SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING
        );
        assert_receive_invariants(&device);
    }

    #[test]
    fn enabling_receive_interrupt_does_not_retroactively_interrupt_buffered_data() {
        let mut device = SerialMmioDevice::buffered();
        device
            .inject_receive_bytes(b"x")
            .expect("receive injection should succeed");

        device
            .write_byte(
                SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET,
                SERIAL_INTERRUPT_ENABLE_RECEIVED_DATA_AVAILABLE,
            )
            .expect("IER write should succeed");

        assert_eq!(device.pending_interrupt_intent(), None);
        assert_eq!(
            device.interrupt_identification(),
            SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING
        );
        assert_ne!(device.line_status() & SERIAL_LINE_STATUS_DATA_READY, 0);
    }

    #[test]
    fn refill_cancels_stale_input_ready_intent_and_fifo_clear_republishes_it() {
        let mut device = SerialMmioDevice::buffered();
        device
            .inject_receive_bytes(b"a")
            .expect("receive injection should succeed");
        assert_eq!(device.read_byte(SERIAL_TRANSMIT_REGISTER_OFFSET), Ok(b'a'));
        assert_eq!(
            device.pending_input_ready_intent(),
            Some(SerialInputReadyIntent::ReceiveBufferEmpty)
        );

        device
            .inject_receive_bytes(b"bc")
            .expect("refill should succeed");
        assert_eq!(device.pending_input_ready_intent(), None);
        device
            .write_byte(
                SERIAL_FIFO_CONTROL_REGISTER_OFFSET,
                SERIAL_FIFO_CONTROL_CLEAR_RECEIVE,
            )
            .expect("RX FIFO clear should succeed");

        assert_eq!(device.receive_fifo_len(), 0);
        assert_eq!(device.line_status(), SERIAL_LINE_STATUS_DEFAULT);
        assert_eq!(
            device.take_input_ready_intent(),
            Some(SerialInputReadyIntent::ReceiveBufferEmpty)
        );
        assert_eq!(device.metrics().flush_count(), 1);
        assert_receive_invariants(&device);
    }

    #[test]
    fn dlab_reads_do_not_drain_receive_fifo() {
        let mut device = SerialMmioDevice::buffered();
        device
            .inject_receive_bytes(b"z")
            .expect("receive injection should succeed");
        device
            .write_byte(
                SERIAL_LINE_CONTROL_REGISTER_OFFSET,
                SERIAL_LINE_CONTROL_DLAB,
            )
            .expect("DLAB write should succeed");

        assert_eq!(device.read_byte(SERIAL_TRANSMIT_REGISTER_OFFSET), Ok(0));
        assert_eq!(device.receive_fifo_len(), 1);

        device
            .write_byte(SERIAL_LINE_CONTROL_REGISTER_OFFSET, 0)
            .expect("DLAB clear should succeed");
        assert_eq!(device.read_byte(SERIAL_TRANSMIT_REGISTER_OFFSET), Ok(b'z'));
    }

    #[test]
    fn complete_capture_round_trips_into_a_fresh_output() {
        let mut device = SerialMmioDevice::buffered();
        device
            .write_byte(
                SERIAL_LINE_CONTROL_REGISTER_OFFSET,
                SERIAL_LINE_CONTROL_DLAB,
            )
            .expect("DLAB write should succeed");
        device
            .write_byte(SERIAL_TRANSMIT_REGISTER_OFFSET, 0x34)
            .expect("divisor low write should succeed");
        device
            .write_byte(SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET, 0x12)
            .expect("divisor high write should succeed");
        device
            .write_byte(SERIAL_LINE_CONTROL_REGISTER_OFFSET, 0x1b)
            .expect("line control write should succeed");
        device
            .write_byte(SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET, 0xa1)
            .expect("interrupt enable write should succeed");
        device
            .write_byte(SERIAL_MODEM_CONTROL_REGISTER_OFFSET, 0x0b)
            .expect("modem control write should succeed");
        device
            .write_byte(SERIAL_SCRATCH_REGISTER_OFFSET, 0x5a)
            .expect("scratch write should succeed");
        device
            .write_byte(SERIAL_TRANSMIT_REGISTER_OFFSET, b'X')
            .expect("TX write should succeed");
        device
            .inject_receive_bytes(b"secret")
            .expect("receive injection should succeed");

        let capture = device.capture_state().expect("capture should succeed");
        assert_eq!(capture.receive_bytes(), b"secret");
        assert_eq!(
            capture.interrupt_identification(),
            SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE
        );
        assert!(capture.receive_interrupt_intent_pending());
        assert_eq!(
            format!("{capture:?}"),
            "SerialMmioCaptureState { state: \"<redacted>\" }"
        );
        assert!(!format!("{capture:?}").contains("secret"));

        let mut restored =
            SerialMmioDevice::from_capture_state(SerialOutputBuffer::default(), capture.clone());
        assert_eq!(
            restored
                .capture_state()
                .expect("restored capture should succeed"),
            capture
        );
        assert!(restored.output().bytes().is_empty());
        assert_eq!(device.output().bytes(), b"X");
        assert!(restored.metrics().is_empty());
        assert_eq!(
            restored.take_interrupt_intent(),
            Some(SerialInterruptIntent::ReceivedDataAvailable)
        );
        for expected in b"secret" {
            assert_eq!(
                restored.read_byte(SERIAL_TRANSMIT_REGISTER_OFFSET),
                Ok(*expected)
            );
        }
        assert_receive_invariants(&restored);
    }

    #[test]
    fn capture_state_rejects_malformed_register_and_intent_combinations() {
        let mut parts = valid_capture_parts();
        parts.receive_bytes = vec![0; SERIAL_RECEIVE_FIFO_CAPACITY + 1];
        assert!(matches!(
            SerialMmioCaptureState::try_from_parts(parts),
            Err(SerialMmioCaptureStateError::ReceiveBufferTooLarge { .. })
        ));

        let mut parts = valid_capture_parts();
        parts.interrupt_identification = 0xff;
        assert!(matches!(
            SerialMmioCaptureState::try_from_parts(parts),
            Err(SerialMmioCaptureStateError::InvalidInterruptIdentification { .. })
        ));

        let mut parts = valid_capture_parts();
        parts.line_status = SERIAL_LINE_STATUS_DATA_READY;
        assert!(matches!(
            SerialMmioCaptureState::try_from_parts(parts),
            Err(SerialMmioCaptureStateError::InvalidLineStatus { .. })
        ));

        let mut parts = valid_capture_parts();
        parts.modem_status = 1;
        assert!(matches!(
            SerialMmioCaptureState::try_from_parts(parts),
            Err(SerialMmioCaptureStateError::InvalidModemStatus { .. })
        ));

        let mut parts = valid_capture_parts();
        parts.receive_bytes = vec![b'a'];
        assert_eq!(
            SerialMmioCaptureState::try_from_parts(parts),
            Err(SerialMmioCaptureStateError::DataReadyMismatch)
        );

        let mut parts = valid_capture_parts();
        parts.interrupt_identification = SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE;
        assert_eq!(
            SerialMmioCaptureState::try_from_parts(parts),
            Err(SerialMmioCaptureStateError::ReceiveInterruptWithoutData)
        );

        let mut parts = valid_capture_parts();
        parts.receive_interrupt_intent_pending = true;
        assert_eq!(
            SerialMmioCaptureState::try_from_parts(parts),
            Err(SerialMmioCaptureStateError::ReceiveInterruptIntentMismatch)
        );

        let mut parts = valid_capture_parts();
        parts.receive_bytes = vec![b'a'];
        parts.line_status |= SERIAL_LINE_STATUS_DATA_READY;
        parts.input_ready_intent_pending = true;
        assert_eq!(
            SerialMmioCaptureState::try_from_parts(parts),
            Err(SerialMmioCaptureStateError::InputReadyIntentMismatch)
        );
    }

    #[test]
    fn receive_and_capture_allocation_failures_do_not_mutate_uart_state() {
        let mut device = SerialMmioDevice::buffered();
        let before = device
            .capture_state()
            .expect("baseline capture should succeed");

        let inject_error = device
            .inject_receive_bytes_with(b"x", |_fifo, _len| Err(allocation_failure()))
            .expect_err("injected receive allocation failure should reject");
        assert!(matches!(
            inject_error,
            super::SerialReceiveInjectionError::BufferAllocation { .. }
        ));
        assert_eq!(
            device
                .capture_state()
                .expect("state should remain capturable"),
            before
        );

        let capture_error = device
            .capture_state_with(|_bytes, _len| Err(allocation_failure()))
            .expect_err("injected capture allocation failure should reject");
        assert!(matches!(
            capture_error,
            SerialMmioCaptureError::BufferAllocation { .. }
        ));
        assert_eq!(
            device
                .capture_state()
                .expect("state should remain capturable"),
            before
        );
        assert_eq!(device.metrics().error_count(), 2);
    }

    #[test]
    fn legacy_state_rejects_receive_state_until_every_transient_is_drained() {
        let mut device = SerialMmioDevice::buffered();
        assert_eq!(device.legacy_state(), Ok(device.state()));

        device
            .inject_receive_bytes(b"x")
            .expect("receive injection should succeed");
        assert_eq!(
            device.legacy_state(),
            Err(SerialMmioLegacyStateError::ReceiveStateNotRepresentable)
        );
        device
            .write_byte(
                SERIAL_FIFO_CONTROL_REGISTER_OFFSET,
                SERIAL_FIFO_CONTROL_CLEAR_RECEIVE,
            )
            .expect("RX clear should succeed");
        assert_eq!(
            device.legacy_state(),
            Err(SerialMmioLegacyStateError::ReceiveStateNotRepresentable)
        );
        assert_eq!(
            device.take_input_ready_intent(),
            Some(SerialInputReadyIntent::ReceiveBufferEmpty)
        );
        assert_eq!(device.legacy_state(), Ok(device.state()));
    }

    #[test]
    fn vm_superio_receive_vectors_match_the_shared_uart_surface() {
        let trigger = RecordingTrigger::default();
        let mut upstream = VmSuperioSerial::new(trigger.clone(), Vec::<u8>::new());
        let mut device = SerialMmioDevice::buffered();

        upstream
            .write(1, SERIAL_INTERRUPT_ENABLE_RECEIVED_DATA_AVAILABLE)
            .expect("upstream IER write should succeed");
        device
            .write_byte(
                SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET,
                SERIAL_INTERRUPT_ENABLE_RECEIVED_DATA_AVAILABLE,
            )
            .expect("Bangbang IER write should succeed");

        let upstream_written = upstream
            .enqueue_raw_bytes(b"abc")
            .expect("upstream injection should succeed");
        let outcome = device
            .inject_receive_bytes(b"abc")
            .expect("Bangbang injection should succeed");
        assert_eq!(outcome.accepted_bytes(), upstream_written);
        assert_eq!(device.receive_fifo_capacity(), upstream.fifo_capacity());
        assert_eq!(
            device
                .read_byte(SERIAL_LINE_STATUS_REGISTER_OFFSET)
                .expect("Bangbang LSR read should succeed")
                & !SERIAL_LINE_STATUS_OVERRUN_ERROR,
            upstream.read(5)
        );
        assert_eq!(
            device
                .read_byte(SERIAL_INTERRUPT_IDENTIFICATION_REGISTER_OFFSET)
                .expect("Bangbang IIR read should succeed"),
            upstream.read(2)
        );
        assert_eq!(trigger.count(), 1);
        assert_eq!(
            device.take_interrupt_intent(),
            None,
            "IIR acknowledgement must cancel an undelivered intent"
        );

        for expected in b"abc" {
            assert_eq!(upstream.read(0), *expected);
            assert_eq!(
                device.read_byte(SERIAL_TRANSMIT_REGISTER_OFFSET),
                Ok(*expected)
            );
        }
        assert_eq!(device.receive_fifo_capacity(), upstream.fifo_capacity());
        assert_eq!(
            device
                .read_byte(SERIAL_LINE_STATUS_REGISTER_OFFSET)
                .expect("Bangbang LSR read should succeed"),
            upstream.read(5)
        );

        let fill = vec![0x5a; SERIAL_RECEIVE_FIFO_CAPACITY - 1];
        assert_eq!(
            device
                .inject_receive_bytes(&fill)
                .expect("Bangbang fill should succeed")
                .accepted_bytes(),
            upstream
                .enqueue_raw_bytes(&fill)
                .expect("upstream fill should succeed")
        );
        assert_eq!(
            device
                .inject_receive_bytes(&[0xa5, 0xff])
                .expect("Bangbang partial fill should succeed")
                .accepted_bytes(),
            upstream
                .enqueue_raw_bytes(&[0xa5, 0xff])
                .expect("upstream partial fill should succeed")
        );
        assert_eq!(device.receive_fifo_capacity(), upstream.fifo_capacity());
        for _ in 0..SERIAL_RECEIVE_FIFO_CAPACITY {
            assert_eq!(
                device.read_byte(SERIAL_TRANSMIT_REGISTER_OFFSET),
                Ok(upstream.read(0))
            );
        }
        assert_receive_invariants(&device);
    }

    #[test]
    fn bounded_operation_sequences_preserve_receive_invariants() {
        const OPERATION_COUNT: usize = 8;
        const SEQUENCE_LENGTH: u32 = 5;

        for mut sequence in 0..OPERATION_COUNT.pow(SEQUENCE_LENGTH) {
            let mut device = SerialMmioDevice::buffered();
            for _ in 0..SEQUENCE_LENGTH {
                match sequence % OPERATION_COUNT {
                    0 => {
                        device
                            .inject_receive_bytes(b"x")
                            .expect("single-byte injection should succeed");
                    }
                    1 => {
                        device
                            .inject_receive_bytes(&[0x5a; SERIAL_RECEIVE_FIFO_CAPACITY + 1])
                            .expect("oversized bounded injection should succeed");
                    }
                    2 => {
                        device
                            .read_byte(SERIAL_TRANSMIT_REGISTER_OFFSET)
                            .expect("RBR read should succeed");
                    }
                    3 => {
                        device
                            .read_byte(SERIAL_INTERRUPT_IDENTIFICATION_REGISTER_OFFSET)
                            .expect("IIR read should succeed");
                    }
                    4 => {
                        device
                            .read_byte(SERIAL_LINE_STATUS_REGISTER_OFFSET)
                            .expect("LSR read should succeed");
                    }
                    5 => {
                        device
                            .write_byte(
                                SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET,
                                device.interrupt_enable()
                                    ^ SERIAL_INTERRUPT_ENABLE_RECEIVED_DATA_AVAILABLE,
                            )
                            .expect("IER toggle should succeed");
                    }
                    6 => {
                        device
                            .write_byte(
                                SERIAL_FIFO_CONTROL_REGISTER_OFFSET,
                                SERIAL_FIFO_CONTROL_CLEAR_RECEIVE,
                            )
                            .expect("RX clear should succeed");
                    }
                    _ => {
                        device.take_interrupt_intent();
                        device.take_input_ready_intent();
                    }
                }
                assert_receive_invariants(&device);
                device
                    .capture_state()
                    .expect("every reachable state should be capturable");
                sequence /= OPERATION_COUNT;
            }
        }
    }

    #[test]
    fn shared_output_and_uart_core_publish_one_coherent_metric_generation() {
        let buffer = SharedSerialOutputBuffer::default();
        let output = SharedSerialOutput::from(buffer.clone());
        let mut device = SerialMmioDevice::with_shared_output(output.clone());

        device
            .write_byte(SERIAL_TRANSMIT_REGISTER_OFFSET, b'T')
            .expect("TX write should succeed");
        device
            .write_byte(
                SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET,
                SERIAL_INTERRUPT_ENABLE_RECEIVED_DATA_AVAILABLE,
            )
            .expect("IER write should succeed");
        device
            .inject_receive_bytes(&[0x5a; SERIAL_RECEIVE_FIFO_CAPACITY + 1])
            .expect("bounded injection should succeed");
        device
            .read_byte(SERIAL_TRANSMIT_REGISTER_OFFSET)
            .expect("RBR read should succeed");
        device
            .write_byte(
                SERIAL_FIFO_CONTROL_REGISTER_OFFSET,
                SERIAL_FIFO_CONTROL_CLEAR_RECEIVE,
            )
            .expect("RX clear should succeed");
        device
            .read(access(SERIAL_TRANSMIT_REGISTER_OFFSET, 4))
            .expect_err("wide read should fail");
        device
            .write(access(SERIAL_LINE_STATUS_REGISTER_OFFSET, 1), bytes(&[0]))
            .expect_err("read-only write should fail");

        let expected = SerialOutputMetrics::default()
            .with_flush_count(1)
            .with_input_count(SERIAL_RECEIVE_FIFO_CAPACITY as u64)
            .with_interrupt_count(1)
            .with_missed_read_count(1)
            .with_missed_write_count(1)
            .with_overrun_count(1)
            .with_read_count(1)
            .with_write_count(1);
        assert_eq!(device.metrics(), expected);
        assert_eq!(output.metrics(), expected);
        assert_eq!(buffer.bytes().expect("shared output should read"), b"T");
    }

    #[test]
    fn unsupported_read_width_returns_error() {
        let mut device = SerialMmioDevice::buffered();

        let err = device
            .read(access(SERIAL_TRANSMIT_REGISTER_OFFSET, 4))
            .expect_err("wide serial reads should fail");

        assert_eq!(
            err.message(),
            "serial MMIO offset 0x0 only supports 1-byte accesses, got 4 bytes"
        );
    }

    #[test]
    fn unsupported_write_data_length_returns_error() {
        let mut device = SerialMmioDevice::buffered();

        let err = device
            .write(access(SERIAL_TRANSMIT_REGISTER_OFFSET, 1), bytes(&[1, 2]))
            .expect_err("wide serial writes should fail");

        assert_eq!(
            err.message(),
            "serial MMIO offset 0x0 write requires 1 data byte, got 2"
        );
    }

    #[test]
    fn invalid_register_offset_returns_error() {
        let mut device = SerialMmioDevice::buffered();

        let err = device
            .read(access(8, 1))
            .expect_err("invalid serial register offset should fail");

        assert_eq!(
            err.message(),
            "serial MMIO offset 0x8 is outside the 8-byte register space"
        );
    }

    #[test]
    fn read_only_status_write_returns_error() {
        let mut device = SerialMmioDevice::buffered();

        let err = write(&mut device, SERIAL_LINE_STATUS_REGISTER_OFFSET, 0)
            .expect_err("status register write should fail");

        assert_eq!(err.message(), "serial MMIO offset 0x5 is read-only");
    }

    #[test]
    fn buffered_output_rejects_writes_after_limit() {
        let mut device = SerialMmioDevice::buffered_with_limit(1);

        write(&mut device, SERIAL_TRANSMIT_REGISTER_OFFSET, b'a').expect("first byte should fit");
        let err = write(&mut device, SERIAL_TRANSMIT_REGISTER_OFFSET, b'b')
            .expect_err("second byte should exceed the buffer limit");

        assert_eq!(
            err.message(),
            "serial output failed: serial output buffer reached its 1-byte limit"
        );
        assert_eq!(device.output().bytes(), b"a");
    }

    #[test]
    fn buffered_output_with_zero_limit_rejects_first_write() {
        let mut device = SerialMmioDevice::buffered_with_limit(0);

        let err = write(&mut device, SERIAL_TRANSMIT_REGISTER_OFFSET, b'a')
            .expect_err("zero-length buffer should reject the first byte");

        assert_eq!(
            err.message(),
            "serial output failed: serial output buffer reached its 0-byte limit"
        );
        assert_eq!(device.output().bytes(), b"");
    }

    #[test]
    fn buffered_output_uses_default_limit() {
        let device = SerialMmioDevice::buffered();

        assert_eq!(device.output().limit(), SERIAL_OUTPUT_BUFFER_DEFAULT_LIMIT);
    }

    #[test]
    fn shared_output_buffer_clones_share_bounded_output() {
        let mut first = SharedSerialOutputBuffer::new(1);
        let mut second = first.clone();

        first
            .write_byte(b'a')
            .expect("first byte should fit shared buffer");
        let err = second
            .write_byte(b'b')
            .expect_err("shared buffer should enforce one-byte limit");

        assert_eq!(
            err.message(),
            "serial output buffer reached its 1-byte limit"
        );
        assert_eq!(first.bytes().expect("shared bytes should read"), b"a");
        assert_eq!(second.bytes().expect("shared bytes should read"), b"a");
        assert_eq!(first.limit().expect("shared limit should read"), 1);
    }

    #[test]
    fn shared_serial_output_clones_share_wrapped_output() {
        let buffer = SharedSerialOutputBuffer::default();
        let mut first = SharedSerialOutput::from(buffer.clone());
        let mut second = first.clone();

        first.write_byte(b'a').expect("first byte should write");
        second.write_byte(b'b').expect("second byte should write");

        assert_eq!(buffer.bytes().expect("shared bytes should read"), b"ab");
    }

    #[test]
    fn shared_serial_output_counts_successful_writes() {
        let buffer = SharedSerialOutputBuffer::default();
        let mut output = SharedSerialOutput::from(buffer.clone());

        output.write_byte(b'a').expect("first byte should write");
        output.write_byte(b'b').expect("second byte should write");

        assert_eq!(buffer.bytes().expect("shared bytes should read"), b"ab");
        assert_eq!(
            output.metrics(),
            SerialOutputMetrics::default().with_write_count(2)
        );
    }

    #[test]
    fn token_bucket_consumes_burst_budget_and_refills_by_time() {
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
    fn token_bucket_disables_zero_or_overflowing_configs() {
        let now = Instant::now();

        for config in [
            SerialRateLimiterConfig::new(0, None, 1),
            SerialRateLimiterConfig::new(1, None, 0),
            SerialRateLimiterConfig::new(1, None, u64::MAX),
        ] {
            assert!(
                TokenBucket::new_at(
                    TokenBucketConfig::new(
                        config.size(),
                        config.one_time_burst(),
                        config.refill_time(),
                    ),
                    now,
                )
                .is_none()
            );
        }
    }

    #[test]
    fn rate_limited_output_drops_exhausted_bytes_without_output_error() {
        let buffer = SharedSerialOutputBuffer::new(1);
        let mut output = RateLimitedSerialOutput::new(
            buffer.clone(),
            SerialRateLimiterConfig::new(1, None, 100),
        )
        .expect("bucket should be enabled");

        output.write_byte(b'a').expect("first byte should write");
        output
            .write_byte(b'b')
            .expect("exhausted byte should be dropped");

        assert_eq!(buffer.bytes().expect("shared bytes should read"), b"a");
        assert_eq!(output.metrics().rate_limiter_dropped_bytes(), 1);
    }

    #[test]
    fn shared_serial_output_counts_rate_limited_dropped_bytes() {
        let buffer = SharedSerialOutputBuffer::new(1);
        let mut output = SharedSerialOutput::with_rate_limiter(
            buffer.clone(),
            Some(SerialRateLimiterConfig::new(1, None, 100)),
        );

        output.write_byte(b'a').expect("first byte should write");
        output
            .write_byte(b'b')
            .expect("first exhausted byte should be dropped");
        output
            .write_byte(b'c')
            .expect("second exhausted byte should be dropped");

        assert_eq!(buffer.bytes().expect("shared bytes should read"), b"a");
        assert_eq!(
            output.metrics(),
            SerialOutputMetrics::default()
                .with_write_count(1)
                .with_rate_limiter_dropped_bytes(2)
        );
    }

    #[test]
    fn shared_serial_output_metrics_saturates_counters() {
        let metrics = SharedSerialOutputMetrics::default();

        metrics.record_rate_limiter_dropped_bytes(u64::MAX - 1);
        metrics.record_rate_limiter_dropped_bytes(2);
        metrics.record_write();
        metrics.record_write();

        assert_eq!(
            metrics.snapshot(),
            SerialOutputMetrics::default()
                .with_write_count(2)
                .with_rate_limiter_dropped_bytes(u64::MAX)
        );
    }

    #[test]
    fn shared_serial_output_skips_disabled_rate_limiter() {
        let buffer = SharedSerialOutputBuffer::new(1);
        let mut output = SharedSerialOutput::with_rate_limiter(
            buffer.clone(),
            Some(SerialRateLimiterConfig::new(0, None, 1)),
        );

        output.write_byte(b'a').expect("first byte should write");
        let err = output
            .write_byte(b'b')
            .expect_err("disabled limiter should expose buffer errors");

        assert_eq!(
            err.message(),
            "serial output buffer reached its 1-byte limit"
        );
        assert_eq!(buffer.bytes().expect("shared bytes should read"), b"a");
        assert_eq!(
            output.metrics(),
            SerialOutputMetrics::default()
                .with_error_count(1)
                .with_missed_write_count(1)
                .with_write_count(1)
        );
    }

    #[test]
    fn validates_serial_config_output_path() {
        let config = SerialConfigInput::new()
            .with_serial_out_path("/tmp/serial.out")
            .with_rate_limiter(SerialRateLimiterConfig::new(2, Some(1), 3))
            .validate()
            .expect("serial config should validate");

        assert_eq!(config.serial_out_path(), Some(Path::new("/tmp/serial.out")));
        assert_eq!(
            config.rate_limiter(),
            Some(SerialRateLimiterConfig::new(2, Some(1), 3))
        );
    }

    #[test]
    fn validates_serial_config_clear_request() {
        let config = SerialConfigInput::new()
            .validate()
            .expect("serial clear request should validate");

        assert_eq!(config.serial_out_path(), None);
        assert_eq!(config.rate_limiter(), None);
    }

    #[test]
    fn rejects_invalid_serial_config_without_path_echo() {
        for (input, expected, message) in [
            (
                SerialConfigInput::new().with_serial_out_path(""),
                SerialConfigError::EmptyOutputPath,
                "serial output path must not be empty",
            ),
            (
                SerialConfigInput::new().with_serial_out_path("/tmp/bad\npath"),
                SerialConfigError::InvalidOutputPath,
                "serial output path must not contain control characters",
            ),
        ] {
            let err = input
                .validate()
                .expect_err("invalid serial config should fail");

            assert_eq!(err, expected);
            assert_eq!(err.to_string(), message);
            assert!(!err.to_string().contains("/tmp/bad"));
        }
    }

    #[test]
    fn file_output_writes_serial_bytes() {
        let path = unique_serial_output_path("file-output");
        let mut output = SerialOutputFile::open(&path).expect("serial output file should open");

        output.write_byte(b'a').expect("first byte should write");
        output.write_byte(b'b').expect("second byte should write");

        assert_eq!(fs::read(&path).expect("serial output should read"), b"ab");
        fs::remove_file(path).expect("serial output should clean up");
    }

    #[test]
    fn file_output_adopts_write_only_file_and_appends_bytes() {
        let path = unique_serial_output_path("provided-file-output");
        fs::write(&path, b"seed").expect("fixture should write");
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("write-only fixture should open");
        let mut output =
            SerialOutputFile::from_file(file).expect("provided serial output should adopt");
        assert_eq!(format!("{output:?}"), "SerialOutputFile { .. }");

        output.write_byte(b'a').expect("first byte should write");
        output.write_byte(b'b').expect("second byte should write");

        assert_eq!(
            fs::read(&path).expect("serial output should read"),
            b"seedab"
        );
        fs::remove_file(path).expect("serial output should clean up");
    }

    #[test]
    fn file_output_open_error_is_redacted() {
        let path = unique_serial_output_path("missing-parent").join("serial.out");

        let err = SerialOutputFile::open(&path)
            .expect_err("serial output with missing parent should fail");

        assert_eq!(
            err,
            SerialConfigError::OpenOutput(std::io::ErrorKind::NotFound)
        );
        assert_eq!(
            err.to_string(),
            "serial output could not be initialized: NotFound"
        );
        assert!(!err.to_string().contains("missing-parent"));
    }

    #[derive(Debug)]
    struct FailingOutput;

    impl SerialOutput for FailingOutput {
        fn write_byte(&mut self, _byte: u8) -> Result<(), SerialOutputError> {
            Err(SerialOutputError::new("sink failed"))
        }
    }

    #[test]
    fn sink_failure_propagates_to_handler_error() {
        let mut device = SerialMmioDevice::new(FailingOutput);

        let err = device
            .write(access(SERIAL_TRANSMIT_REGISTER_OFFSET, 1), bytes(b"x"))
            .expect_err("sink failure should propagate");

        assert_eq!(err.message(), "serial output failed: sink failed");
    }

    #[test]
    fn shared_sink_failure_is_counted_once_by_the_output_generation() {
        let output = SharedSerialOutput::new(FailingOutput);
        let mut device = SerialMmioDevice::with_shared_output(output.clone());

        let err = device
            .write(access(SERIAL_TRANSMIT_REGISTER_OFFSET, 1), bytes(b"x"))
            .expect_err("sink failure should propagate");

        assert_eq!(err.message(), "serial output failed: sink failed");
        let expected = SerialOutputMetrics::default()
            .with_error_count(1)
            .with_missed_write_count(1);
        assert_eq!(device.metrics(), expected);
        assert_eq!(output.metrics(), expected);
    }

    #[derive(Debug, Clone)]
    struct SharedOutput {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl SerialOutput for SharedOutput {
        fn write_byte(&mut self, byte: u8) -> Result<(), SerialOutputError> {
            self.bytes
                .lock()
                .expect("shared output lock should not be poisoned")
                .push(byte);
            Ok(())
        }
    }

    #[test]
    fn dispatcher_routes_serial_transmit_write() {
        let shared = Arc::new(Mutex::new(Vec::new()));
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                MmioRegionId::new(9),
                GuestAddress::new(0x2000),
                SERIAL_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("region should insert");
        dispatcher
            .register_handler(
                MmioRegionId::new(9),
                SerialMmioDevice::new(SharedOutput {
                    bytes: Arc::clone(&shared),
                }),
            )
            .expect("handler should register");

        let access = dispatcher
            .lookup(GuestAddress::new(0x2000), 1)
            .expect("lookup should succeed");
        let outcome = dispatcher
            .dispatch(
                MmioOperation::write(access, bytes(b"q")).expect("write operation should build"),
            )
            .expect("dispatch should succeed");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
        assert_eq!(
            shared
                .lock()
                .expect("shared output lock should not be poisoned")
                .as_slice(),
            b"q"
        );
    }

    #[test]
    fn dispatcher_wraps_serial_handler_errors() {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                MmioRegionId::new(9),
                GuestAddress::new(0x2000),
                SERIAL_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("region should insert");
        dispatcher
            .register_handler(MmioRegionId::new(9), SerialMmioDevice::new(FailingOutput))
            .expect("handler should register");

        let access = dispatcher
            .lookup(GuestAddress::new(0x2000), 1)
            .expect("lookup should succeed");
        let err = dispatcher
            .dispatch(
                MmioOperation::write(access, bytes(b"q")).expect("write operation should build"),
            )
            .expect_err("dispatch should fail");

        assert_eq!(
            err,
            MmioDispatchError::HandlerFailed {
                region_id: MmioRegionId::new(9),
                kind: crate::mmio::MmioOperationKind::Write,
                source: crate::mmio::MmioHandlerError::new("serial output failed: sink failed")
            }
        );
    }

    #[test]
    fn direct_error_preserves_source_for_output_failures() {
        let source = SerialOutputError::new("disk full");
        let err = SerialMmioError::Output {
            source: source.clone(),
        };

        assert_eq!(err.to_string(), "serial output failed: disk full");
        assert_eq!(
            err.source().map(ToString::to_string),
            Some(source.to_string())
        );
    }
}
