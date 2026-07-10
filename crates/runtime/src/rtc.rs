//! Backend-neutral ARM PL031 RTC MMIO device model.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::memory::GuestAddress;
use crate::metrics::SharedRtcDeviceMetrics;
use crate::mmio::{
    MmioAccess, MmioAccessBytes, MmioAccessBytesError, MmioHandler, MmioHandlerError, MmioRegionId,
};

pub const RTC_MMIO_DEVICE_WINDOW_SIZE: u64 = 0x1000;
pub const RTC_DATA_REGISTER_OFFSET: u64 = 0x000;
pub const RTC_MATCH_REGISTER_OFFSET: u64 = 0x004;
pub const RTC_LOAD_REGISTER_OFFSET: u64 = 0x008;
pub const RTC_CONTROL_REGISTER_OFFSET: u64 = 0x00c;
pub const RTC_INTERRUPT_MASK_REGISTER_OFFSET: u64 = 0x010;
pub const RTC_RAW_INTERRUPT_STATUS_REGISTER_OFFSET: u64 = 0x014;
pub const RTC_MASKED_INTERRUPT_STATUS_REGISTER_OFFSET: u64 = 0x018;
pub const RTC_INTERRUPT_CLEAR_REGISTER_OFFSET: u64 = 0x01c;
const RTC_PERIPHERAL_ID_BASE_OFFSET: u64 = 0xfe0;
const RTC_PRIMECELL_ID_BASE_OFFSET: u64 = 0xff0;
const RTC_ID_REGISTER_STRIDE: u64 = 4;
const RTC_REGISTER_ACCESS_SIZE: u64 = 4;
const RTC_REGISTER_ACCESS_LEN: usize = 4;
const RTC_PERIPHERAL_ID: [u8; 4] = [0x31, 0x10, 0x14, 0x00];
const RTC_PRIMECELL_ID: [u8; 4] = [0x0d, 0xf0, 0x05, 0xb1];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtcMmioLayout {
    base: GuestAddress,
    region_id: MmioRegionId,
}

impl RtcMmioLayout {
    pub const fn new(base: GuestAddress, region_id: MmioRegionId) -> Self {
        Self { base, region_id }
    }

    pub const fn base(self) -> GuestAddress {
        self.base
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region_id
    }
}

pub trait RtcTimeSource: fmt::Debug + Send {
    fn unix_time_seconds(&self) -> u64;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemRtcTimeSource;

impl RtcTimeSource for SystemRtcTimeSource {
    fn unix_time_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs())
    }
}

#[derive(Debug, Clone)]
pub struct Pl031RtcDevice<T = SystemRtcTimeSource> {
    time_source: T,
    metrics: SharedRtcDeviceMetrics,
    load_value: u32,
    load_source_seconds: u32,
    match_value: u32,
    control: u32,
    interrupt_mask: u32,
}

impl Pl031RtcDevice<SystemRtcTimeSource> {
    pub fn system() -> Self {
        Self::new(SystemRtcTimeSource)
    }

    pub fn system_with_metrics(metrics: SharedRtcDeviceMetrics) -> Self {
        Self::new_with_metrics(SystemRtcTimeSource, metrics)
    }
}

impl<T: RtcTimeSource> Pl031RtcDevice<T> {
    pub fn new(time_source: T) -> Self {
        Self::new_with_metrics(time_source, SharedRtcDeviceMetrics::default())
    }

    pub fn new_with_metrics(time_source: T, metrics: SharedRtcDeviceMetrics) -> Self {
        let now = rtc_seconds_u32(time_source.unix_time_seconds());
        Self {
            time_source,
            metrics,
            load_value: now,
            load_source_seconds: now,
            match_value: 0,
            control: 0,
            interrupt_mask: 0,
        }
    }

    pub const fn time_source(&self) -> &T {
        &self.time_source
    }

    pub fn time_source_mut(&mut self) -> &mut T {
        &mut self.time_source
    }

    pub fn shared_metrics(&self) -> SharedRtcDeviceMetrics {
        self.metrics.clone()
    }

    pub fn read_register(&self, offset: u64) -> Result<u32, Pl031RtcError> {
        match offset {
            RTC_DATA_REGISTER_OFFSET => Ok(self.current_value()),
            RTC_MATCH_REGISTER_OFFSET => Ok(self.match_value),
            RTC_LOAD_REGISTER_OFFSET => Ok(self.load_value),
            RTC_CONTROL_REGISTER_OFFSET => Ok(self.control),
            RTC_INTERRUPT_MASK_REGISTER_OFFSET => Ok(self.interrupt_mask),
            RTC_RAW_INTERRUPT_STATUS_REGISTER_OFFSET
            | RTC_MASKED_INTERRUPT_STATUS_REGISTER_OFFSET
            | RTC_INTERRUPT_CLEAR_REGISTER_OFFSET => Ok(0),
            _ => self.read_id_register(offset),
        }
    }

    pub fn write_register(&mut self, offset: u64, value: u32) -> Result<(), Pl031RtcError> {
        match offset {
            RTC_MATCH_REGISTER_OFFSET => {
                self.match_value = value;
                Ok(())
            }
            RTC_LOAD_REGISTER_OFFSET => {
                self.load_value = value;
                self.load_source_seconds = rtc_seconds_u32(self.time_source.unix_time_seconds());
                Ok(())
            }
            RTC_CONTROL_REGISTER_OFFSET => {
                self.control = value;
                Ok(())
            }
            RTC_INTERRUPT_MASK_REGISTER_OFFSET => {
                self.interrupt_mask = value;
                Ok(())
            }
            RTC_INTERRUPT_CLEAR_REGISTER_OFFSET => Ok(()),
            RTC_DATA_REGISTER_OFFSET
            | RTC_RAW_INTERRUPT_STATUS_REGISTER_OFFSET
            | RTC_MASKED_INTERRUPT_STATUS_REGISTER_OFFSET => {
                Err(Pl031RtcError::ReadOnlyRegisterWrite { offset })
            }
            _ if is_id_register(offset) => Err(Pl031RtcError::ReadOnlyRegisterWrite { offset }),
            _ => Err(Pl031RtcError::InvalidRegisterOffset { offset }),
        }
    }

    fn current_value(&self) -> u32 {
        let now = rtc_seconds_u32(self.time_source.unix_time_seconds());
        let elapsed = now.saturating_sub(self.load_source_seconds);
        self.load_value.wrapping_add(elapsed)
    }

    fn read_id_register(&self, offset: u64) -> Result<u32, Pl031RtcError> {
        if let Some(value) =
            id_register_value(offset, RTC_PERIPHERAL_ID_BASE_OFFSET, RTC_PERIPHERAL_ID)
        {
            return Ok(u32::from(value));
        }
        if let Some(value) =
            id_register_value(offset, RTC_PRIMECELL_ID_BASE_OFFSET, RTC_PRIMECELL_ID)
        {
            return Ok(u32::from(value));
        }

        Err(Pl031RtcError::InvalidRegisterOffset { offset })
    }
}

impl<T: RtcTimeSource> MmioHandler for Pl031RtcDevice<T> {
    fn read(&mut self, access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
        self.read_access(access).map_err(|err| {
            self.metrics.record_read_error();
            MmioHandlerError::from(err)
        })
    }

    fn write(&mut self, access: MmioAccess, data: MmioAccessBytes) -> Result<(), MmioHandlerError> {
        self.write_access(access, data).map_err(|err| {
            self.metrics.record_write_error();
            MmioHandlerError::from(err)
        })
    }
}

impl<T: PartialEq> PartialEq for Pl031RtcDevice<T> {
    fn eq(&self, other: &Self) -> bool {
        self.time_source == other.time_source
            && self.load_value == other.load_value
            && self.load_source_seconds == other.load_source_seconds
            && self.match_value == other.match_value
            && self.control == other.control
            && self.interrupt_mask == other.interrupt_mask
    }
}

impl<T: Eq> Eq for Pl031RtcDevice<T> {}

impl<T: RtcTimeSource> Pl031RtcDevice<T> {
    fn read_access(&self, access: MmioAccess) -> Result<MmioAccessBytes, Pl031RtcError> {
        validate_word_access(access)?;
        let value = self.read_register(access.offset())?;

        MmioAccessBytes::new(&value.to_le_bytes())
            .map_err(|source| Pl031RtcError::BuildReadBytes { source })
    }

    fn write_access(
        &mut self,
        access: MmioAccess,
        data: MmioAccessBytes,
    ) -> Result<(), Pl031RtcError> {
        validate_word_access(access)?;
        let value = word_from_data(access.offset(), data)?;
        self.write_register(access.offset(), value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pl031RtcError {
    UnsupportedAccessSize { offset: u64, size: u64 },
    UnsupportedWriteDataLength { offset: u64, len: usize },
    InvalidRegisterOffset { offset: u64 },
    ReadOnlyRegisterWrite { offset: u64 },
    BuildReadBytes { source: MmioAccessBytesError },
}

impl fmt::Display for Pl031RtcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAccessSize { offset, size } => {
                write!(
                    f,
                    "PL031 RTC MMIO offset 0x{offset:x} only supports 4-byte accesses, got {size} bytes"
                )
            }
            Self::UnsupportedWriteDataLength { offset, len } => {
                write!(
                    f,
                    "PL031 RTC MMIO offset 0x{offset:x} write requires 4 data bytes, got {len}"
                )
            }
            Self::InvalidRegisterOffset { offset } => {
                write!(
                    f,
                    "PL031 RTC MMIO offset 0x{offset:x} is not a supported register"
                )
            }
            Self::ReadOnlyRegisterWrite { offset } => {
                write!(f, "PL031 RTC MMIO offset 0x{offset:x} is read-only")
            }
            Self::BuildReadBytes { source } => {
                write!(f, "failed to build PL031 RTC MMIO read bytes: {source}")
            }
        }
    }
}

impl std::error::Error for Pl031RtcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BuildReadBytes { source } => Some(source),
            Self::UnsupportedAccessSize { .. }
            | Self::UnsupportedWriteDataLength { .. }
            | Self::InvalidRegisterOffset { .. }
            | Self::ReadOnlyRegisterWrite { .. } => None,
        }
    }
}

impl From<Pl031RtcError> for MmioHandlerError {
    fn from(source: Pl031RtcError) -> Self {
        Self::new(source.to_string())
    }
}

fn validate_word_access(access: MmioAccess) -> Result<(), Pl031RtcError> {
    let offset = access.offset();
    let size = access.range().size();
    if size == RTC_REGISTER_ACCESS_SIZE {
        Ok(())
    } else {
        Err(Pl031RtcError::UnsupportedAccessSize { offset, size })
    }
}

fn word_from_data(offset: u64, data: MmioAccessBytes) -> Result<u32, Pl031RtcError> {
    let bytes: [u8; RTC_REGISTER_ACCESS_LEN] =
        data.as_slice()
            .try_into()
            .map_err(|_| Pl031RtcError::UnsupportedWriteDataLength {
                offset,
                len: data.len(),
            })?;
    Ok(u32::from_le_bytes(bytes))
}

fn id_register_value<const N: usize>(offset: u64, base: u64, values: [u8; N]) -> Option<u8> {
    let index = offset
        .checked_sub(base)?
        .checked_div(RTC_ID_REGISTER_STRIDE)?;
    if index.checked_mul(RTC_ID_REGISTER_STRIDE)? != offset.checked_sub(base)? {
        return None;
    }
    let index = usize::try_from(index).ok()?;
    values.get(index).copied()
}

fn is_id_register(offset: u64) -> bool {
    id_register_value(offset, RTC_PERIPHERAL_ID_BASE_OFFSET, RTC_PERIPHERAL_ID).is_some()
        || id_register_value(offset, RTC_PRIMECELL_ID_BASE_OFFSET, RTC_PRIMECELL_ID).is_some()
}

fn rtc_seconds_u32(seconds: u64) -> u32 {
    u32::try_from(seconds).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        Pl031RtcDevice, Pl031RtcError, RTC_CONTROL_REGISTER_OFFSET, RTC_DATA_REGISTER_OFFSET,
        RTC_INTERRUPT_CLEAR_REGISTER_OFFSET, RTC_INTERRUPT_MASK_REGISTER_OFFSET,
        RTC_LOAD_REGISTER_OFFSET, RTC_MASKED_INTERRUPT_STATUS_REGISTER_OFFSET,
        RTC_MATCH_REGISTER_OFFSET, RTC_MMIO_DEVICE_WINDOW_SIZE,
        RTC_RAW_INTERRUPT_STATUS_REGISTER_OFFSET, RtcTimeSource,
    };
    use crate::memory::GuestAddress;
    use crate::metrics::RtcDeviceMetrics;
    use crate::mmio::{MmioAccess, MmioAccessBytes, MmioDispatcher, MmioHandler, MmioRegionId};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FixedRtcTimeSource {
        seconds: u64,
    }

    impl FixedRtcTimeSource {
        const fn new(seconds: u64) -> Self {
            Self { seconds }
        }

        const fn set_seconds(&mut self, seconds: u64) {
            self.seconds = seconds;
        }
    }

    impl RtcTimeSource for FixedRtcTimeSource {
        fn unix_time_seconds(&self) -> u64 {
            self.seconds
        }
    }

    fn access(offset: u64, size: u64) -> MmioAccess {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                MmioRegionId::new(7),
                GuestAddress::new(0x1000),
                RTC_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("test region should insert");
        dispatcher
            .lookup(GuestAddress::new(0x1000 + offset), size)
            .expect("test lookup should succeed")
    }

    fn bytes(value: u32) -> MmioAccessBytes {
        MmioAccessBytes::new(&value.to_le_bytes()).expect("test bytes should be valid")
    }

    fn read(device: &mut Pl031RtcDevice<FixedRtcTimeSource>, offset: u64) -> u32 {
        let data = device
            .read(access(offset, 4))
            .expect("test read should succeed");
        let bytes: [u8; 4] = data
            .as_slice()
            .try_into()
            .expect("RTC read should return 4 bytes");
        u32::from_le_bytes(bytes)
    }

    fn write(device: &mut Pl031RtcDevice<FixedRtcTimeSource>, offset: u64, value: u32) {
        device
            .write(access(offset, 4), bytes(value))
            .expect("test write should succeed");
    }

    #[test]
    fn data_register_reads_current_time() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));

        assert_eq!(read(&mut device, RTC_DATA_REGISTER_OFFSET), 100);
        device.time_source_mut().set_seconds(105);
        assert_eq!(read(&mut device, RTC_DATA_REGISTER_OFFSET), 105);
    }

    #[test]
    fn load_register_rebases_current_time() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));

        write(&mut device, RTC_LOAD_REGISTER_OFFSET, 500);
        assert_eq!(read(&mut device, RTC_DATA_REGISTER_OFFSET), 500);

        device.time_source_mut().set_seconds(103);
        assert_eq!(read(&mut device, RTC_DATA_REGISTER_OFFSET), 503);
        assert_eq!(read(&mut device, RTC_LOAD_REGISTER_OFFSET), 500);
    }

    #[test]
    fn current_time_saturates_to_register_width() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));

        device.time_source_mut().set_seconds(u64::MAX);

        assert_eq!(read(&mut device, RTC_DATA_REGISTER_OFFSET), u32::MAX);
    }

    #[test]
    fn backwards_time_source_does_not_underflow() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));

        device.time_source_mut().set_seconds(90);

        assert_eq!(read(&mut device, RTC_DATA_REGISTER_OFFSET), 100);
    }

    #[test]
    fn loaded_counter_wraps_at_register_width() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));

        write(&mut device, RTC_LOAD_REGISTER_OFFSET, u32::MAX - 1);
        device.time_source_mut().set_seconds(103);

        assert_eq!(read(&mut device, RTC_DATA_REGISTER_OFFSET), 1);
    }

    #[test]
    fn writable_registers_round_trip() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));

        write(&mut device, RTC_MATCH_REGISTER_OFFSET, 0x0102_0304);
        write(&mut device, RTC_CONTROL_REGISTER_OFFSET, 0x01);
        write(&mut device, RTC_INTERRUPT_MASK_REGISTER_OFFSET, 0x01);

        assert_eq!(read(&mut device, RTC_MATCH_REGISTER_OFFSET), 0x0102_0304);
        assert_eq!(read(&mut device, RTC_CONTROL_REGISTER_OFFSET), 0x01);
        assert_eq!(read(&mut device, RTC_INTERRUPT_MASK_REGISTER_OFFSET), 0x01);
    }

    #[test]
    fn no_interrupt_status_is_reported() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));

        assert_eq!(
            read(&mut device, RTC_RAW_INTERRUPT_STATUS_REGISTER_OFFSET),
            0
        );
        assert_eq!(
            read(&mut device, RTC_MASKED_INTERRUPT_STATUS_REGISTER_OFFSET),
            0
        );
        write(&mut device, RTC_INTERRUPT_CLEAR_REGISTER_OFFSET, 1);
        assert_eq!(read(&mut device, RTC_INTERRUPT_CLEAR_REGISTER_OFFSET), 0);
    }

    #[test]
    fn primecell_identification_registers_are_exposed() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));

        assert_eq!(read(&mut device, 0xfe0), 0x31);
        assert_eq!(read(&mut device, 0xfe4), 0x10);
        assert_eq!(read(&mut device, 0xfe8), 0x14);
        assert_eq!(read(&mut device, 0xfec), 0x00);
        assert_eq!(read(&mut device, 0xff0), 0x0d);
        assert_eq!(read(&mut device, 0xff4), 0xf0);
        assert_eq!(read(&mut device, 0xff8), 0x05);
        assert_eq!(read(&mut device, 0xffc), 0xb1);
    }

    #[test]
    fn unsupported_access_width_fails() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));
        let err = device
            .read(access(RTC_DATA_REGISTER_OFFSET, 1))
            .expect_err("byte access should fail");

        assert!(err.to_string().contains("only supports 4-byte accesses"));
    }

    #[test]
    fn invalid_offsets_fail() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));
        let err = device
            .read(access(0x020, 4))
            .expect_err("invalid register should fail");

        assert!(err.to_string().contains("not a supported register"));
    }

    #[test]
    fn unaligned_identification_register_offsets_fail() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));
        let err = device
            .read(access(0xfe2, 4))
            .expect_err("unaligned ID register should fail");

        assert!(err.to_string().contains("not a supported register"));
    }

    #[test]
    fn read_only_register_writes_fail() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));
        let err = device
            .write(access(RTC_DATA_REGISTER_OFFSET, 4), bytes(123))
            .expect_err("data register write should fail");

        assert_eq!(
            err,
            Pl031RtcError::ReadOnlyRegisterWrite {
                offset: RTC_DATA_REGISTER_OFFSET,
            }
            .into()
        );
    }

    #[test]
    fn write_data_length_mismatch_fails() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));
        let short_data = MmioAccessBytes::new(&[1, 2]).expect("test bytes should be valid");
        let err = device
            .write(access(RTC_MATCH_REGISTER_OFFSET, 4), short_data)
            .expect_err("short write data should fail");

        assert!(err.to_string().contains("write requires 4 data bytes"));
    }

    #[test]
    fn successful_mmio_accesses_do_not_record_errors() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));
        let metrics = device.shared_metrics();

        assert_eq!(read(&mut device, RTC_DATA_REGISTER_OFFSET), 100);
        write(&mut device, RTC_MATCH_REGISTER_OFFSET, 123);

        assert_eq!(metrics.snapshot(), RtcDeviceMetrics::default());
    }

    #[test]
    fn read_errors_record_missed_read_and_error_counts() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));
        let metrics = device.shared_metrics();

        let _ = device
            .read(access(RTC_DATA_REGISTER_OFFSET, 1))
            .expect_err("byte read should fail");
        let _ = device
            .read(access(0x020, 4))
            .expect_err("invalid register read should fail");

        assert_eq!(
            metrics.snapshot(),
            RtcDeviceMetrics::default()
                .with_error_count(2)
                .with_missed_read_count(2)
        );
    }

    #[test]
    fn write_errors_record_missed_write_and_error_counts() {
        let mut device = Pl031RtcDevice::new(FixedRtcTimeSource::new(100));
        let metrics = device.shared_metrics();
        let short_data = MmioAccessBytes::new(&[1, 2]).expect("test bytes should be valid");

        let _ = device
            .write(access(RTC_DATA_REGISTER_OFFSET, 4), bytes(123))
            .expect_err("read-only register write should fail");
        let _ = device
            .write(access(RTC_MATCH_REGISTER_OFFSET, 4), short_data)
            .expect_err("short write data should fail");

        assert_eq!(
            metrics.snapshot(),
            RtcDeviceMetrics::default()
                .with_error_count(2)
                .with_missed_write_count(2)
        );
    }
}
