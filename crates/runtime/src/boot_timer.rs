//! Firecracker-compatible pseudo-MMIO boot timer device.

use std::fmt;
use std::mem::MaybeUninit;

use crate::logger::{BootTimerLogger, LoggerWriteError};
use crate::memory::GuestAddress;
use crate::mmio::{
    MmioAccess, MmioAccessBytes, MmioAccessBytesError, MmioBusError, MmioDispatchError,
    MmioDispatcher, MmioHandler, MmioHandlerError, MmioRegionId,
};

pub const BOOT_TIMER_MMIO_DEVICE_WINDOW_SIZE: u64 = 0x1000;
pub const BOOT_TIMER_MAGIC_VALUE: u8 = 123;
const BOOT_TIMER_REGISTER_SPACE_SIZE: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootTimerMmioLayout {
    address: GuestAddress,
    region_id: MmioRegionId,
}

impl BootTimerMmioLayout {
    pub const fn new(address: GuestAddress, region_id: MmioRegionId) -> Self {
        Self { address, region_id }
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootTimerTimestamp {
    wall_time_us: u64,
    cpu_time_us: u64,
}

impl BootTimerTimestamp {
    pub const fn new(wall_time_us: u64, cpu_time_us: u64) -> Self {
        Self {
            wall_time_us,
            cpu_time_us,
        }
    }

    pub const fn wall_time_us(self) -> u64 {
        self.wall_time_us
    }

    pub const fn cpu_time_us(self) -> u64 {
        self.cpu_time_us
    }

    const fn elapsed_since(self, start: Self) -> Self {
        Self {
            wall_time_us: self.wall_time_us.saturating_sub(start.wall_time_us),
            cpu_time_us: self.cpu_time_us.saturating_sub(start.cpu_time_us),
        }
    }
}

pub trait BootTimerClock: fmt::Debug + Send {
    fn timestamp(&mut self) -> Result<BootTimerTimestamp, BootTimerClockError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemBootTimerClock;

impl BootTimerClock for SystemBootTimerClock {
    fn timestamp(&mut self) -> Result<BootTimerTimestamp, BootTimerClockError> {
        Ok(BootTimerTimestamp::new(
            clock_time_us(libc::CLOCK_MONOTONIC).map_err(BootTimerClockError::Monotonic)?,
            clock_time_us(libc::CLOCK_PROCESS_CPUTIME_ID)
                .map_err(BootTimerClockError::ProcessCpu)?,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootTimerClockError {
    Monotonic(std::io::ErrorKind),
    ProcessCpu(std::io::ErrorKind),
}

impl fmt::Display for BootTimerClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Monotonic(kind) => write!(f, "failed to read monotonic clock: {kind:?}"),
            Self::ProcessCpu(kind) => write!(f, "failed to read process CPU clock: {kind:?}"),
        }
    }
}

impl std::error::Error for BootTimerClockError {}

#[derive(Debug)]
pub struct BootTimerMmioDevice<C = SystemBootTimerClock>
where
    C: BootTimerClock,
{
    clock: C,
    start: BootTimerTimestamp,
    logger: BootTimerLogger,
}

impl BootTimerMmioDevice<SystemBootTimerClock> {
    pub fn system(logger: BootTimerLogger) -> Result<Self, BootTimerClockError> {
        Self::with_clock(SystemBootTimerClock, logger)
    }
}

impl<C> BootTimerMmioDevice<C>
where
    C: BootTimerClock,
{
    pub fn with_clock(mut clock: C, logger: BootTimerLogger) -> Result<Self, BootTimerClockError> {
        let start = clock.timestamp()?;
        Ok(Self {
            clock,
            start,
            logger,
        })
    }

    fn read_access(&self, access: MmioAccess) -> Result<MmioAccessBytes, BootTimerMmioError> {
        validate_byte_access(access)?;
        validate_register_offset(access.offset())?;

        MmioAccessBytes::zeroed(1).map_err(|source| BootTimerMmioError::BuildReadBytes { source })
    }

    fn write_access(
        &mut self,
        access: MmioAccess,
        data: MmioAccessBytes,
    ) -> Result<(), BootTimerMmioError> {
        let offset = validate_byte_access(access)?;
        validate_register_offset(offset)?;
        let value = single_data_byte(offset, data)?;
        if value != BOOT_TIMER_MAGIC_VALUE {
            return Ok(());
        }

        let elapsed = self
            .clock
            .timestamp()
            .map_err(|source| BootTimerMmioError::Clock { source })?
            .elapsed_since(self.start);
        self.logger
            .log_boot_time(elapsed.wall_time_us(), elapsed.cpu_time_us())
            .map_err(|source| BootTimerMmioError::Logger { source })?;
        Ok(())
    }
}

impl<C> MmioHandler for BootTimerMmioDevice<C>
where
    C: BootTimerClock + 'static,
{
    fn read(&mut self, access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
        self.read_access(access).map_err(MmioHandlerError::from)
    }

    fn write(&mut self, access: MmioAccess, data: MmioAccessBytes) -> Result<(), MmioHandlerError> {
        self.write_access(access, data)
            .map_err(MmioHandlerError::from)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootTimerMmioError {
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
    BuildReadBytes {
        source: MmioAccessBytesError,
    },
    Clock {
        source: BootTimerClockError,
    },
    Logger {
        source: LoggerWriteError,
    },
}

impl fmt::Display for BootTimerMmioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAccessSize { offset, size } => {
                write!(
                    f,
                    "boot timer MMIO offset 0x{offset:x} only supports 1-byte accesses, got {size} bytes"
                )
            }
            Self::UnsupportedWriteDataLength { offset, len } => {
                write!(
                    f,
                    "boot timer MMIO offset 0x{offset:x} write requires 1 data byte, got {len}"
                )
            }
            Self::InvalidRegisterOffset {
                offset,
                register_space_size,
            } => {
                write!(
                    f,
                    "boot timer MMIO offset 0x{offset:x} is outside the {register_space_size}-byte register space"
                )
            }
            Self::BuildReadBytes { source } => {
                write!(f, "failed to build boot timer MMIO read bytes: {source}")
            }
            Self::Clock { source } => write!(f, "failed to read boot timer clock: {source}"),
            Self::Logger { source } => {
                write!(f, "failed to write boot timer logger output: {source}")
            }
        }
    }
}

impl std::error::Error for BootTimerMmioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BuildReadBytes { source } => Some(source),
            Self::Clock { source } => Some(source),
            Self::Logger { source } => Some(source),
            Self::UnsupportedAccessSize { .. }
            | Self::UnsupportedWriteDataLength { .. }
            | Self::InvalidRegisterOffset { .. } => None,
        }
    }
}

impl From<BootTimerMmioError> for MmioHandlerError {
    fn from(source: BootTimerMmioError) -> Self {
        Self::new(source.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootTimerMmioRegistrationError {
    CreateDevice {
        source: BootTimerClockError,
    },
    InsertRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: MmioBusError,
    },
    RegisterHandler {
        region_id: MmioRegionId,
        source: MmioDispatchError,
    },
}

impl fmt::Display for BootTimerMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateDevice { source } => {
                write!(f, "failed to create boot timer MMIO device: {source}")
            }
            Self::InsertRegion {
                region_id,
                address,
                source,
            } => write!(
                f,
                "failed to insert boot timer MMIO region id={region_id} at {address}: {source}"
            ),
            Self::RegisterHandler { region_id, source } => write!(
                f,
                "failed to register boot timer MMIO handler for region id={region_id}: {source}"
            ),
        }
    }
}

impl std::error::Error for BootTimerMmioRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateDevice { source } => Some(source),
            Self::InsertRegion { source, .. } => Some(source),
            Self::RegisterHandler { source, .. } => Some(source),
        }
    }
}

pub fn register_boot_timer_mmio(
    dispatcher: &mut MmioDispatcher,
    layout: BootTimerMmioLayout,
    logger: BootTimerLogger,
) -> Result<(), BootTimerMmioRegistrationError> {
    let device = BootTimerMmioDevice::system(logger)
        .map_err(|source| BootTimerMmioRegistrationError::CreateDevice { source })?;

    dispatcher
        .insert_region(
            layout.region_id(),
            layout.address(),
            BOOT_TIMER_MMIO_DEVICE_WINDOW_SIZE,
        )
        .map_err(|source| BootTimerMmioRegistrationError::InsertRegion {
            region_id: layout.region_id(),
            address: layout.address(),
            source,
        })?;
    dispatcher
        .register_handler(layout.region_id(), device)
        .map_err(|source| BootTimerMmioRegistrationError::RegisterHandler {
            region_id: layout.region_id(),
            source,
        })
}

fn validate_register_offset(offset: u64) -> Result<(), BootTimerMmioError> {
    if offset == 0 {
        Ok(())
    } else {
        Err(BootTimerMmioError::InvalidRegisterOffset {
            offset,
            register_space_size: BOOT_TIMER_REGISTER_SPACE_SIZE,
        })
    }
}

fn validate_byte_access(access: MmioAccess) -> Result<u64, BootTimerMmioError> {
    let offset = access.offset();
    let size = access.range().size();
    if size == 1 {
        Ok(offset)
    } else {
        Err(BootTimerMmioError::UnsupportedAccessSize { offset, size })
    }
}

fn single_data_byte(offset: u64, data: MmioAccessBytes) -> Result<u8, BootTimerMmioError> {
    if data.len() != 1 {
        return Err(BootTimerMmioError::UnsupportedWriteDataLength {
            offset,
            len: data.len(),
        });
    }

    data.as_slice()
        .first()
        .copied()
        .ok_or(BootTimerMmioError::UnsupportedWriteDataLength {
            offset,
            len: data.len(),
        })
}

fn clock_time_us(clock_id: libc::clockid_t) -> Result<u64, std::io::ErrorKind> {
    let mut time = MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: `clock_gettime` writes a valid `timespec` to the provided pointer
    // when it returns 0. The pointer is valid for writes and properly aligned.
    let result = unsafe { libc::clock_gettime(clock_id, time.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().kind());
    }
    // SAFETY: `clock_gettime` returned success, so the `timespec` was initialized.
    let time = unsafe { time.assume_init() };

    timespec_time_us(time)
}

fn timespec_time_us(time: libc::timespec) -> Result<u64, std::io::ErrorKind> {
    let seconds = u64::try_from(time.tv_sec).map_err(|_| std::io::ErrorKind::InvalidData)?;
    let nanoseconds = u64::try_from(time.tv_nsec).map_err(|_| std::io::ErrorKind::InvalidData)?;

    Ok(seconds
        .saturating_mul(1_000_000)
        .saturating_add(nanoseconds / 1_000))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        BOOT_TIMER_MAGIC_VALUE, BOOT_TIMER_MMIO_DEVICE_WINDOW_SIZE, BOOT_TIMER_REGISTER_SPACE_SIZE,
        BootTimerClock, BootTimerClockError, BootTimerMmioDevice, BootTimerMmioError,
        BootTimerMmioLayout, BootTimerTimestamp, register_boot_timer_mmio,
    };
    use crate::logger::{LoggerConfigInput, LoggerState};
    use crate::memory::GuestAddress;
    use crate::mmio::{
        MmioAccessBytes, MmioDispatchError, MmioDispatchOutcome, MmioDispatcher, MmioOperation,
        MmioRegionId,
    };

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct ScriptedClock {
        timestamps: VecDeque<Result<BootTimerTimestamp, BootTimerClockError>>,
    }

    impl ScriptedClock {
        fn new(
            timestamps: impl Into<VecDeque<Result<BootTimerTimestamp, BootTimerClockError>>>,
        ) -> Self {
            Self {
                timestamps: timestamps.into(),
            }
        }
    }

    impl BootTimerClock for ScriptedClock {
        fn timestamp(&mut self) -> Result<BootTimerTimestamp, BootTimerClockError> {
            self.timestamps
                .pop_front()
                .expect("test clock should have a timestamp")
        }
    }

    fn unique_logger_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "bangbang-boot-timer-test-{}-{nanos}-{id}-{name}",
            std::process::id()
        ))
    }

    fn logger_with_path(path: &PathBuf) -> crate::logger::BootTimerLogger {
        let mut state = LoggerState::default();
        state
            .configure(LoggerConfigInput::new().with_log_path(path))
            .expect("logger should configure");
        state.boot_timer_logger()
    }

    fn bytes(data: &[u8]) -> MmioAccessBytes {
        MmioAccessBytes::new(data).expect("test bytes should be valid")
    }

    fn dispatcher_with_device<C: BootTimerClock + 'static>(
        device: BootTimerMmioDevice<C>,
    ) -> MmioDispatcher {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                MmioRegionId::new(1),
                GuestAddress::new(0x1000),
                BOOT_TIMER_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("boot timer region should insert");
        dispatcher
            .register_handler(MmioRegionId::new(1), device)
            .expect("boot timer handler should register");
        dispatcher
    }

    #[test]
    fn magic_write_logs_elapsed_boot_time() {
        let path = unique_logger_path("magic");
        let logger = logger_with_path(&path);
        let device = BootTimerMmioDevice::with_clock(
            ScriptedClock::new(VecDeque::from([
                Ok(BootTimerTimestamp::new(10_000, 1_000)),
                Ok(BootTimerTimestamp::new(17_123, 2_456)),
            ])),
            logger,
        )
        .expect("boot timer should build");
        let mut dispatcher = dispatcher_with_device(device);
        let access = dispatcher
            .lookup(GuestAddress::new(0x1000), 1)
            .expect("boot timer access should resolve");

        let operation = MmioOperation::write(access, bytes(&[BOOT_TIMER_MAGIC_VALUE]))
            .expect("write operation should build");
        assert_eq!(
            dispatcher
                .dispatch(operation)
                .expect("write should dispatch"),
            MmioDispatchOutcome::Write
        );

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(
            output,
            "Guest-boot-time =   7123 us 7 ms,   1456 CPU us 1 CPU ms\n"
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn non_magic_write_is_ignored() {
        let path = unique_logger_path("non-magic");
        let logger = logger_with_path(&path);
        let device = BootTimerMmioDevice::with_clock(
            ScriptedClock::new(VecDeque::from([Ok(BootTimerTimestamp::new(1, 1))])),
            logger,
        )
        .expect("boot timer should build");
        let mut dispatcher = dispatcher_with_device(device);
        let access = dispatcher
            .lookup(GuestAddress::new(0x1000), 1)
            .expect("boot timer access should resolve");
        let operation =
            MmioOperation::write(access, bytes(&[0])).expect("write operation should build");

        dispatcher
            .dispatch(operation)
            .expect("write should dispatch");

        let output = fs::read_to_string(&path).expect("logger output should be readable");
        assert_eq!(output, "");
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn read_returns_zero_byte() {
        let path = unique_logger_path("read");
        let logger = logger_with_path(&path);
        let device = BootTimerMmioDevice::with_clock(
            ScriptedClock::new(VecDeque::from([Ok(BootTimerTimestamp::new(1, 1))])),
            logger,
        )
        .expect("boot timer should build");
        let mut dispatcher = dispatcher_with_device(device);
        let access = dispatcher
            .lookup(GuestAddress::new(0x1000), 1)
            .expect("boot timer access should resolve");
        let operation = MmioOperation::read(access).expect("read operation should build");

        let MmioDispatchOutcome::Read { data } = dispatcher
            .dispatch(operation)
            .expect("read should dispatch")
        else {
            panic!("read should return data");
        };

        assert_eq!(data.as_slice(), &[0]);
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn invalid_offset_returns_handler_error() {
        let path = unique_logger_path("invalid-offset");
        let logger = logger_with_path(&path);
        let device = BootTimerMmioDevice::with_clock(
            ScriptedClock::new(VecDeque::from([Ok(BootTimerTimestamp::new(1, 1))])),
            logger,
        )
        .expect("boot timer should build");
        let mut dispatcher = dispatcher_with_device(device);
        let access = dispatcher
            .lookup(GuestAddress::new(0x1001), 1)
            .expect("boot timer access should resolve");
        let operation =
            MmioOperation::write(access, bytes(&[0])).expect("write operation should build");

        let err = dispatcher
            .dispatch(operation)
            .expect_err("invalid offset should fail");

        assert!(matches!(
            err,
            MmioDispatchError::HandlerFailed {
                source,
                ..
            } if source.message().contains("boot timer MMIO offset 0x1")
        ));
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn registration_inserts_region_and_handler() {
        let path = unique_logger_path("register");
        let logger = logger_with_path(&path);
        let mut dispatcher = MmioDispatcher::new();
        let layout = BootTimerMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(7));

        register_boot_timer_mmio(&mut dispatcher, layout, logger)
            .expect("boot timer MMIO should register");

        assert_eq!(
            dispatcher
                .lookup(GuestAddress::new(0x4000_0000), 1)
                .expect("registered access should resolve")
                .region_id(),
            MmioRegionId::new(7)
        );
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn clock_error_is_reported_on_magic_write() {
        let path = unique_logger_path("clock-error");
        let logger = logger_with_path(&path);
        let device = BootTimerMmioDevice::with_clock(
            ScriptedClock::new(VecDeque::from([
                Ok(BootTimerTimestamp::new(10_000, 1_000)),
                Err(BootTimerClockError::ProcessCpu(std::io::ErrorKind::Other)),
            ])),
            logger,
        )
        .expect("boot timer should build");
        let mut dispatcher = dispatcher_with_device(device);
        let access = dispatcher
            .lookup(GuestAddress::new(0x1000), 1)
            .expect("boot timer access should resolve");
        let operation = MmioOperation::write(access, bytes(&[BOOT_TIMER_MAGIC_VALUE]))
            .expect("write operation should build");

        let err = dispatcher
            .dispatch(operation)
            .expect_err("clock failure should fail");

        assert!(matches!(
            err,
            MmioDispatchError::HandlerFailed {
                source,
                ..
            } if source.message().contains("failed to read boot timer clock")
        ));
        fs::remove_file(path).expect("fixture should clean up");
    }

    #[test]
    fn error_display_does_not_include_paths() {
        let err = BootTimerMmioError::UnsupportedAccessSize { offset: 0, size: 4 };

        assert_eq!(
            err.to_string(),
            "boot timer MMIO offset 0x0 only supports 1-byte accesses, got 4 bytes"
        );

        let err = BootTimerMmioError::InvalidRegisterOffset {
            offset: 1,
            register_space_size: BOOT_TIMER_REGISTER_SPACE_SIZE,
        };

        assert_eq!(
            err.to_string(),
            "boot timer MMIO offset 0x1 is outside the 1-byte register space"
        );
    }
}
