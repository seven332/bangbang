//! Backend-neutral TX-only serial MMIO output device model.

use std::collections::TryReserveError;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::mmio::{
    MmioAccess, MmioAccessBytes, MmioAccessBytesError, MmioHandler, MmioHandlerError,
};

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
pub const SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING: u8 = 0x01;
pub const SERIAL_LINE_STATUS_TRANSMIT_HOLDING_REGISTER_EMPTY: u8 = 0x20;
pub const SERIAL_LINE_STATUS_TRANSMITTER_EMPTY: u8 = 0x40;
pub const SERIAL_LINE_STATUS_DEFAULT: u8 =
    SERIAL_LINE_STATUS_TRANSMIT_HOLDING_REGISTER_EMPTY | SERIAL_LINE_STATUS_TRANSMITTER_EMPTY;
pub const SERIAL_OUTPUT_BUFFER_DEFAULT_LIMIT: usize = 64 * 1024;

pub trait SerialOutput: fmt::Debug + Send {
    fn write_byte(&mut self, byte: u8) -> Result<(), SerialOutputError>;
}

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
        Self::new("serial output buffer lock was poisoned")
    }
}

impl fmt::Display for SerialOutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SerialOutputError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialMmioDevice<O> {
    output: O,
    interrupt_enable: u8,
    line_control: u8,
    modem_control: u8,
    scratch: u8,
    divisor_latch_low: u8,
    divisor_latch_high: u8,
}

impl<O> SerialMmioDevice<O> {
    pub const fn new(output: O) -> Self {
        Self {
            output,
            interrupt_enable: 0,
            line_control: 0,
            modem_control: 0,
            scratch: 0,
            divisor_latch_low: 0,
            divisor_latch_high: 0,
        }
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

    pub const fn interrupt_enable(&self) -> u8 {
        self.interrupt_enable
    }

    pub const fn line_control(&self) -> u8 {
        self.line_control
    }

    pub const fn modem_control(&self) -> u8 {
        self.modem_control
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
}

impl SerialMmioDevice<SerialOutputBuffer> {
    pub const fn buffered() -> Self {
        Self::buffered_with_limit(SERIAL_OUTPUT_BUFFER_DEFAULT_LIMIT)
    }

    pub const fn buffered_with_limit(limit: usize) -> Self {
        Self::new(SerialOutputBuffer::new(limit))
    }
}

impl SerialMmioDevice<DiscardSerialOutput> {
    pub const fn discarding() -> Self {
        Self::new(DiscardSerialOutput)
    }
}

impl<O: SerialOutput> SerialMmioDevice<O> {
    pub fn read_byte(&self, offset: u64) -> Result<u8, SerialMmioError> {
        validate_register_offset(offset)?;

        match offset {
            SERIAL_TRANSMIT_REGISTER_OFFSET if self.divisor_latch_access_enabled() => {
                Ok(self.divisor_latch_low)
            }
            SERIAL_TRANSMIT_REGISTER_OFFSET => Ok(0),
            SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET if self.divisor_latch_access_enabled() => {
                Ok(self.divisor_latch_high)
            }
            SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET => Ok(self.interrupt_enable),
            SERIAL_INTERRUPT_IDENTIFICATION_REGISTER_OFFSET => {
                Ok(SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING)
            }
            SERIAL_LINE_CONTROL_REGISTER_OFFSET => Ok(self.line_control),
            SERIAL_MODEM_CONTROL_REGISTER_OFFSET => Ok(self.modem_control),
            SERIAL_LINE_STATUS_REGISTER_OFFSET => Ok(SERIAL_LINE_STATUS_DEFAULT),
            SERIAL_MODEM_STATUS_REGISTER_OFFSET => Ok(0),
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
            SERIAL_FIFO_CONTROL_REGISTER_OFFSET => Ok(()),
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
        self.read_access(access).map_err(MmioHandlerError::from)
    }

    fn write(&mut self, access: MmioAccess, data: MmioAccessBytes) -> Result<(), MmioHandlerError> {
        self.write_access(access, data)
            .map_err(MmioHandlerError::from)
    }
}

impl<O: SerialOutput> SerialMmioDevice<O> {
    fn read_access(&self, access: MmioAccess) -> Result<MmioAccessBytes, SerialMmioError> {
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

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::sync::{Arc, Mutex};

    use super::{
        SERIAL_INTERRUPT_ENABLE_REGISTER_OFFSET,
        SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING,
        SERIAL_INTERRUPT_IDENTIFICATION_REGISTER_OFFSET, SERIAL_LINE_CONTROL_DLAB,
        SERIAL_LINE_CONTROL_REGISTER_OFFSET, SERIAL_LINE_STATUS_DEFAULT,
        SERIAL_LINE_STATUS_REGISTER_OFFSET, SERIAL_MMIO_DEVICE_WINDOW_SIZE,
        SERIAL_MODEM_CONTROL_REGISTER_OFFSET, SERIAL_MODEM_STATUS_REGISTER_OFFSET,
        SERIAL_OUTPUT_BUFFER_DEFAULT_LIMIT, SERIAL_SCRATCH_REGISTER_OFFSET,
        SERIAL_TRANSMIT_REGISTER_OFFSET, SerialMmioDevice, SerialMmioError, SerialOutput,
        SerialOutputBuffer, SerialOutputError, SharedSerialOutputBuffer,
    };
    use crate::memory::GuestAddress;
    use crate::mmio::{
        MmioAccess, MmioAccessBytes, MmioDispatchError, MmioDispatchOutcome, MmioDispatcher,
        MmioHandler, MmioOperation, MmioRegionId,
    };

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

    fn bytes(data: &[u8]) -> MmioAccessBytes {
        MmioAccessBytes::new(data).expect("test bytes should be valid")
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
            [SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING]
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
            [SERIAL_INTERRUPT_IDENTIFICATION_NO_INTERRUPT_PENDING]
        );
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
