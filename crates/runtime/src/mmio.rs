//! Backend-neutral MMIO region ownership, lookup, and operation metadata.

use std::collections::{BTreeMap, btree_map::Entry};
use std::fmt;

use crate::memory::{GuestAddress, GuestMemoryError, GuestMemoryRange};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MmioRegionId(u64);

impl MmioRegionId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn raw_value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for MmioRegionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioRegion {
    id: MmioRegionId,
    range: GuestMemoryRange,
}

impl MmioRegion {
    pub fn new(id: MmioRegionId, start: GuestAddress, size: u64) -> Result<Self, GuestMemoryError> {
        Ok(Self {
            id,
            range: GuestMemoryRange::new(start, size)?,
        })
    }

    pub const fn id(self) -> MmioRegionId {
        self.id
    }

    pub const fn range(self) -> GuestMemoryRange {
        self.range
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioAccess {
    region: MmioRegion,
    range: GuestMemoryRange,
    offset: u64,
}

impl MmioAccess {
    pub const fn region(self) -> MmioRegion {
        self.region
    }

    pub const fn range(self) -> GuestMemoryRange {
        self.range
    }

    pub const fn offset(self) -> u64 {
        self.offset
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region.id()
    }
}

pub const MAX_MMIO_ACCESS_BYTES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioOperationKind {
    Read,
    Write,
}

impl fmt::Display for MmioOperationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => f.write_str("read"),
            Self::Write => f.write_str("write"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioAccessBytes {
    bytes: [u8; MAX_MMIO_ACCESS_BYTES],
    len: usize,
}

impl MmioAccessBytes {
    pub fn new(bytes: &[u8]) -> Result<Self, MmioAccessBytesError> {
        validate_mmio_access_byte_len(bytes.len())?;

        let mut copied = [0; MAX_MMIO_ACCESS_BYTES];
        let (destination, _) = copied.split_at_mut(bytes.len());
        destination.copy_from_slice(bytes);

        Ok(Self {
            bytes: copied,
            len: bytes.len(),
        })
    }

    pub fn zeroed(len: usize) -> Result<Self, MmioAccessBytesError> {
        validate_mmio_access_byte_len(len)?;

        Ok(Self {
            bytes: [0; MAX_MMIO_ACCESS_BYTES],
            len,
        })
    }

    pub const fn len(self) -> usize {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        let (bytes, _) = self.bytes.split_at(self.len);
        bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioAccessBytesError {
    Empty,
    UnsupportedLength { len: usize },
    TooLarge { len: usize, max: usize },
}

impl fmt::Display for MmioAccessBytesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("MMIO access bytes cannot be empty"),
            Self::UnsupportedLength { len } => {
                write!(
                    f,
                    "unsupported MMIO access byte length {len}; supported lengths are 1, 2, 4, or 8"
                )
            }
            Self::TooLarge { len, max } => {
                write!(f, "MMIO access byte length {len} exceeds the maximum {max}")
            }
        }
    }
}

impl std::error::Error for MmioAccessBytesError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioOperation {
    Read {
        access: MmioAccess,
        data: MmioAccessBytes,
    },
    Write {
        access: MmioAccess,
        data: MmioAccessBytes,
    },
}

impl MmioOperation {
    pub fn read(access: MmioAccess) -> Result<Self, MmioOperationError> {
        let len = mmio_operation_len(access)?;

        Ok(Self::Read {
            access,
            data: MmioAccessBytes {
                bytes: [0; MAX_MMIO_ACCESS_BYTES],
                len,
            },
        })
    }

    pub fn write(access: MmioAccess, data: MmioAccessBytes) -> Result<Self, MmioOperationError> {
        let expected = mmio_operation_len(access)?;
        if data.len() != expected {
            return Err(MmioOperationError::DataLengthMismatch {
                access,
                expected,
                actual: data.len(),
            });
        }

        Ok(Self::Write { access, data })
    }

    pub const fn kind(self) -> MmioOperationKind {
        match self {
            Self::Read { .. } => MmioOperationKind::Read,
            Self::Write { .. } => MmioOperationKind::Write,
        }
    }

    pub const fn access(self) -> MmioAccess {
        match self {
            Self::Read { access, .. } | Self::Write { access, .. } => access,
        }
    }

    pub const fn data(self) -> MmioAccessBytes {
        match self {
            Self::Read { data, .. } | Self::Write { data, .. } => data,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioOperationError {
    UnsupportedAccessSize {
        access: MmioAccess,
        size: u64,
    },
    DataLengthMismatch {
        access: MmioAccess,
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for MmioOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAccessSize { access, size } => {
                write!(
                    f,
                    "unsupported MMIO operation size {size} for range {}; supported sizes are 1, 2, 4, or 8 bytes",
                    access.range()
                )
            }
            Self::DataLengthMismatch {
                access,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "MMIO operation data length {actual} does not match access size {expected} for range {}",
                    access.range()
                )
            }
        }
    }
}

impl std::error::Error for MmioOperationError {}

pub trait MmioHandler: fmt::Debug + Send {
    fn read(&mut self, access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError>;

    fn write(&mut self, access: MmioAccess, data: MmioAccessBytes) -> Result<(), MmioHandlerError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmioHandlerError {
    message: String,
}

impl MmioHandlerError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for MmioHandlerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MmioHandlerError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioDispatchOutcome {
    Read { data: MmioAccessBytes },
    Write,
}

#[derive(Debug)]
pub struct MmioDispatcher {
    bus: MmioBus,
    handlers: BTreeMap<MmioRegionId, Box<dyn MmioHandler>>,
}

impl MmioDispatcher {
    pub fn new() -> Self {
        Self {
            bus: MmioBus::new(),
            handlers: BTreeMap::new(),
        }
    }

    pub fn bus(&self) -> &MmioBus {
        &self.bus
    }

    pub fn regions(&self) -> &[MmioRegion] {
        self.bus.regions()
    }

    pub fn insert_region(
        &mut self,
        id: MmioRegionId,
        start: GuestAddress,
        size: u64,
    ) -> Result<MmioRegion, MmioBusError> {
        self.bus.insert(id, start, size)
    }

    pub fn lookup(&self, start: GuestAddress, size: u64) -> Result<MmioAccess, MmioBusError> {
        self.bus.lookup(start, size)
    }

    pub fn register_handler(
        &mut self,
        region_id: MmioRegionId,
        handler: impl MmioHandler + 'static,
    ) -> Result<(), MmioDispatchError> {
        match self.handlers.entry(region_id) {
            Entry::Vacant(entry) => {
                entry.insert(Box::new(handler));
                Ok(())
            }
            Entry::Occupied(_) => Err(MmioDispatchError::DuplicateHandler { region_id }),
        }
    }

    pub fn dispatch(
        &mut self,
        operation: MmioOperation,
    ) -> Result<MmioDispatchOutcome, MmioDispatchError> {
        let region_id = operation.access().region_id();
        let handler = self
            .handlers
            .get_mut(&region_id)
            .ok_or(MmioDispatchError::MissingHandler { region_id })?;

        match operation {
            MmioOperation::Read { access, data } => {
                let expected = data.len();
                let data =
                    handler
                        .read(access)
                        .map_err(|source| MmioDispatchError::HandlerFailed {
                            region_id,
                            kind: MmioOperationKind::Read,
                            source,
                        })?;
                if data.len() != expected {
                    return Err(MmioDispatchError::ReadDataLengthMismatch {
                        access,
                        expected,
                        actual: data.len(),
                    });
                }

                Ok(MmioDispatchOutcome::Read { data })
            }
            MmioOperation::Write { access, data } => {
                handler
                    .write(access, data)
                    .map_err(|source| MmioDispatchError::HandlerFailed {
                        region_id,
                        kind: MmioOperationKind::Write,
                        source,
                    })?;

                Ok(MmioDispatchOutcome::Write)
            }
        }
    }
}

impl Default for MmioDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmioDispatchError {
    DuplicateHandler {
        region_id: MmioRegionId,
    },
    MissingHandler {
        region_id: MmioRegionId,
    },
    HandlerFailed {
        region_id: MmioRegionId,
        kind: MmioOperationKind,
        source: MmioHandlerError,
    },
    ReadDataLengthMismatch {
        access: MmioAccess,
        expected: usize,
        actual: usize,
    },
}

impl fmt::Display for MmioDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateHandler { region_id } => {
                write!(
                    f,
                    "MMIO handler for region id={region_id} is already registered"
                )
            }
            Self::MissingHandler { region_id } => {
                write!(
                    f,
                    "MMIO access for region id={region_id} has no registered handler"
                )
            }
            Self::HandlerFailed {
                region_id,
                kind,
                source,
            } => {
                write!(
                    f,
                    "MMIO {kind} handler for region id={region_id} failed: {source}"
                )
            }
            Self::ReadDataLengthMismatch {
                access,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "MMIO read handler returned {actual} bytes for range {}; expected {expected}",
                    access.range()
                )
            }
        }
    }
}

impl std::error::Error for MmioDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HandlerFailed { source, .. } => Some(source),
            Self::DuplicateHandler { .. }
            | Self::MissingHandler { .. }
            | Self::ReadDataLengthMismatch { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmioBus {
    regions: Vec<MmioRegion>,
}

impl MmioBus {
    pub const fn new() -> Self {
        Self {
            regions: Vec::new(),
        }
    }

    pub fn regions(&self) -> &[MmioRegion] {
        &self.regions
    }

    pub fn insert(
        &mut self,
        id: MmioRegionId,
        start: GuestAddress,
        size: u64,
    ) -> Result<MmioRegion, MmioBusError> {
        let region = MmioRegion::new(id, start, size).map_err(|source| {
            MmioBusError::InvalidRegionRange {
                start,
                size,
                source,
            }
        })?;
        let insertion_index = self
            .regions
            .partition_point(|existing| existing.range().start() < region.range().start());

        if let Some(existing) = insertion_index
            .checked_sub(1)
            .and_then(|previous_index| self.regions.get(previous_index).copied())
        {
            reject_overlap(existing, region)?;
        }

        if let Some(existing) = self.regions.get(insertion_index).copied() {
            reject_overlap(existing, region)?;
        }

        self.regions.insert(insertion_index, region);

        Ok(region)
    }

    pub fn lookup(&self, start: GuestAddress, size: u64) -> Result<MmioAccess, MmioBusError> {
        let access_range = GuestMemoryRange::new(start, size).map_err(|source| {
            MmioBusError::InvalidAccessRange {
                start,
                size,
                source,
            }
        })?;
        let candidate_index = self
            .regions
            .partition_point(|region| region.range().start() <= access_range.start());
        let Some(candidate_index) = candidate_index.checked_sub(1) else {
            return Err(MmioBusError::UnownedAccess {
                range: access_range,
            });
        };
        let Some(region) = self.regions.get(candidate_index).copied() else {
            return Err(MmioBusError::UnownedAccess {
                range: access_range,
            });
        };

        if !range_contains_range(region.range(), access_range) {
            return Err(MmioBusError::UnownedAccess {
                range: access_range,
            });
        }

        Ok(MmioAccess {
            region,
            range: access_range,
            offset: access_range.start().raw_value() - region.range().start().raw_value(),
        })
    }
}

impl Default for MmioBus {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioBusError {
    InvalidRegionRange {
        start: GuestAddress,
        size: u64,
        source: GuestMemoryError,
    },
    InvalidAccessRange {
        start: GuestAddress,
        size: u64,
        source: GuestMemoryError,
    },
    OverlappingRegion {
        existing: MmioRegion,
        new: MmioRegion,
    },
    UnownedAccess {
        range: GuestMemoryRange,
    },
}

impl fmt::Display for MmioBusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegionRange {
                start,
                size,
                source,
            } => {
                write!(
                    f,
                    "invalid MMIO region range at {start} with size {size}: {source}"
                )
            }
            Self::InvalidAccessRange {
                start,
                size,
                source,
            } => {
                write!(
                    f,
                    "invalid MMIO access range at {start} with size {size}: {source}"
                )
            }
            Self::OverlappingRegion { existing, new } => {
                write!(
                    f,
                    "MMIO region id={} range {} overlaps existing id={} range {}",
                    new.id(),
                    new.range(),
                    existing.id(),
                    existing.range()
                )
            }
            Self::UnownedAccess { range } => {
                write!(f, "MMIO access range {range} is not owned by any region")
            }
        }
    }
}

impl std::error::Error for MmioBusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRegionRange { source, .. } | Self::InvalidAccessRange { source, .. } => {
                Some(source)
            }
            Self::OverlappingRegion { .. } | Self::UnownedAccess { .. } => None,
        }
    }
}

fn reject_overlap(existing: MmioRegion, new: MmioRegion) -> Result<(), MmioBusError> {
    if existing.range().overlaps(new.range()) {
        Err(MmioBusError::OverlappingRegion { existing, new })
    } else {
        Ok(())
    }
}

const fn range_contains_range(container: GuestMemoryRange, candidate: GuestMemoryRange) -> bool {
    container.start().raw_value() <= candidate.start().raw_value()
        && candidate.end_exclusive().raw_value() <= container.end_exclusive().raw_value()
}

fn validate_mmio_access_byte_len(len: usize) -> Result<(), MmioAccessBytesError> {
    match len {
        0 => Err(MmioAccessBytesError::Empty),
        1 | 2 | 4 | MAX_MMIO_ACCESS_BYTES => Ok(()),
        len if len > MAX_MMIO_ACCESS_BYTES => Err(MmioAccessBytesError::TooLarge {
            len,
            max: MAX_MMIO_ACCESS_BYTES,
        }),
        len => Err(MmioAccessBytesError::UnsupportedLength { len }),
    }
}

fn mmio_operation_len(access: MmioAccess) -> Result<usize, MmioOperationError> {
    match access.range().size() {
        1 => Ok(1),
        2 => Ok(2),
        4 => Ok(4),
        8 => Ok(MAX_MMIO_ACCESS_BYTES),
        size => Err(MmioOperationError::UnsupportedAccessSize { access, size }),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::sync::{Arc, Mutex};

    use super::{
        MAX_MMIO_ACCESS_BYTES, MmioAccess, MmioAccessBytes, MmioAccessBytesError, MmioBus,
        MmioBusError, MmioDispatchError, MmioDispatchOutcome, MmioDispatcher, MmioHandler,
        MmioHandlerError, MmioOperation, MmioOperationError, MmioOperationKind, MmioRegion,
        MmioRegionId,
    };
    use crate::memory::{GuestAddress, GuestMemoryError, GuestMemoryRange};

    fn id(value: u64) -> MmioRegionId {
        MmioRegionId::new(value)
    }

    fn address(value: u64) -> GuestAddress {
        GuestAddress::new(value)
    }

    fn range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(address(start), size).expect("test range should be valid")
    }

    fn region(id_value: u64, start: u64, size: u64) -> MmioRegion {
        MmioRegion::new(id(id_value), address(start), size).expect("test region should be valid")
    }

    fn insert(bus: &mut MmioBus, id_value: u64, start: u64, size: u64) -> MmioRegion {
        bus.insert(id(id_value), address(start), size)
            .expect("test region insert should succeed")
    }

    fn lookup(bus: &MmioBus, start: u64, size: u64) -> MmioAccess {
        bus.lookup(address(start), size)
            .expect("test lookup should succeed")
    }

    fn lookup_access(start: u64, size: u64) -> MmioAccess {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, start, 0x100);
        lookup(&bus, start, size)
    }

    #[derive(Debug, Default)]
    struct HandlerState {
        reads: Vec<MmioAccess>,
        writes: Vec<(MmioAccess, MmioAccessBytes)>,
    }

    #[derive(Debug)]
    struct ScriptedHandler {
        state: Arc<Mutex<HandlerState>>,
        read_result: Result<MmioAccessBytes, MmioHandlerError>,
        write_result: Result<(), MmioHandlerError>,
    }

    impl ScriptedHandler {
        fn returning(bytes: &[u8]) -> (Arc<Mutex<HandlerState>>, Self) {
            let state = Arc::new(Mutex::new(HandlerState::default()));
            (
                Arc::clone(&state),
                Self {
                    state,
                    read_result: Ok(
                        MmioAccessBytes::new(bytes).expect("scripted read bytes should be valid")
                    ),
                    write_result: Ok(()),
                },
            )
        }

        fn failing(
            read_result: Result<MmioAccessBytes, MmioHandlerError>,
            write_result: Result<(), MmioHandlerError>,
        ) -> (Arc<Mutex<HandlerState>>, Self) {
            let state = Arc::new(Mutex::new(HandlerState::default()));
            (
                Arc::clone(&state),
                Self {
                    state,
                    read_result,
                    write_result,
                },
            )
        }
    }

    impl MmioHandler for ScriptedHandler {
        fn read(&mut self, access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
            self.state
                .lock()
                .expect("handler state lock should not be poisoned")
                .reads
                .push(access);

            self.read_result.clone()
        }

        fn write(
            &mut self,
            access: MmioAccess,
            data: MmioAccessBytes,
        ) -> Result<(), MmioHandlerError> {
            self.state
                .lock()
                .expect("handler state lock should not be poisoned")
                .writes
                .push((access, data));

            self.write_result.clone()
        }
    }

    #[test]
    fn region_creation_rejects_zero_size() {
        let err =
            MmioRegion::new(id(1), address(0x1000), 0).expect_err("zero-sized region should fail");

        assert_eq!(
            err,
            GuestMemoryError::EmptyRange {
                start: address(0x1000)
            }
        );
    }

    #[test]
    fn region_creation_rejects_overflow() {
        let err = MmioRegion::new(id(1), address(u64::MAX), 2)
            .expect_err("overflowing region should fail");

        assert_eq!(
            err,
            GuestMemoryError::AddressOverflow {
                start: address(u64::MAX),
                size: 2
            }
        );
    }

    #[test]
    fn region_creation_rejects_one_byte_at_max_address() {
        let err = MmioRegion::new(id(1), address(u64::MAX), 1)
            .expect_err("end-exclusive max-address range should fail");

        assert_eq!(
            err,
            GuestMemoryError::AddressOverflow {
                start: address(u64::MAX),
                size: 1
            }
        );
    }

    #[test]
    fn mmio_bus_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<MmioBus>();
    }

    #[test]
    fn insertion_keeps_regions_sorted() {
        let mut bus = MmioBus::new();

        let high = insert(&mut bus, 2, 0x3000, 0x100);
        let low = insert(&mut bus, 1, 0x1000, 0x100);
        let middle = insert(&mut bus, 3, 0x2000, 0x100);

        assert_eq!(bus.regions(), &[low, middle, high]);
    }

    #[test]
    fn insertion_accepts_adjacent_regions() {
        let mut bus = MmioBus::new();

        let first = insert(&mut bus, 1, 0x1000, 0x100);
        let second = insert(&mut bus, 2, 0x1100, 0x100);
        let before = insert(&mut bus, 3, 0x0f00, 0x100);

        assert_eq!(bus.regions(), &[before, first, second]);
    }

    #[test]
    fn insertion_accepts_same_owner_for_multiple_regions() {
        let mut bus = MmioBus::new();

        let first = insert(&mut bus, 1, 0x1000, 0x100);
        let second = insert(&mut bus, 1, 0x2000, 0x100);

        assert_eq!(bus.regions(), &[first, second]);
        assert_eq!(lookup(&bus, 0x1000, 4).region_id(), id(1));
        assert_eq!(lookup(&bus, 0x2000, 4).region_id(), id(1));
    }

    #[test]
    fn insertion_rejects_exact_duplicate() {
        let mut bus = MmioBus::new();
        let existing = insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .insert(id(2), address(0x1000), 0x100)
            .expect_err("duplicate region should fail");

        assert_eq!(
            err,
            MmioBusError::OverlappingRegion {
                existing,
                new: region(2, 0x1000, 0x100)
            }
        );
    }

    #[test]
    fn insertion_rejects_start_overlap() {
        let mut bus = MmioBus::new();
        let existing = insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .insert(id(2), address(0x1080), 0x100)
            .expect_err("start overlap should fail");

        assert_eq!(
            err,
            MmioBusError::OverlappingRegion {
                existing,
                new: region(2, 0x1080, 0x100)
            }
        );
    }

    #[test]
    fn insertion_rejects_end_overlap() {
        let mut bus = MmioBus::new();
        let existing = insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .insert(id(2), address(0x0f80), 0x100)
            .expect_err("end overlap should fail");

        assert_eq!(
            err,
            MmioBusError::OverlappingRegion {
                existing,
                new: region(2, 0x0f80, 0x100)
            }
        );
    }

    #[test]
    fn insertion_rejects_contained_region() {
        let mut bus = MmioBus::new();
        let existing = insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .insert(id(2), address(0x1040), 0x20)
            .expect_err("contained region should fail");

        assert_eq!(
            err,
            MmioBusError::OverlappingRegion {
                existing,
                new: region(2, 0x1040, 0x20)
            }
        );
    }

    #[test]
    fn insertion_rejects_containing_region() {
        let mut bus = MmioBus::new();
        let existing = insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .insert(id(2), address(0x0f00), 0x300)
            .expect_err("containing region should fail");

        assert_eq!(
            err,
            MmioBusError::OverlappingRegion {
                existing,
                new: region(2, 0x0f00, 0x300)
            }
        );
    }

    #[test]
    fn insertion_reports_invalid_region_range() {
        let mut bus = MmioBus::new();

        let err = bus
            .insert(id(1), address(u64::MAX), 2)
            .expect_err("invalid insert range should fail");

        assert_eq!(
            err,
            MmioBusError::InvalidRegionRange {
                start: address(u64::MAX),
                size: 2,
                source: GuestMemoryError::AddressOverflow {
                    start: address(u64::MAX),
                    size: 2
                }
            }
        );
    }

    #[test]
    fn lookup_rejects_empty_bus() {
        let bus = MmioBus::new();

        let err = bus
            .lookup(address(0x1000), 4)
            .expect_err("empty bus lookup should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x1000, 4)
            }
        );
    }

    #[test]
    fn lookup_returns_owner_and_offset() {
        let mut bus = MmioBus::new();
        let registered = insert(&mut bus, 7, 0x1000, 0x100);

        assert_eq!(
            lookup(&bus, 0x1040, 4),
            MmioAccess {
                region: registered,
                range: range(0x1040, 4),
                offset: 0x40
            }
        );
    }

    #[test]
    fn lookup_succeeds_for_first_byte() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let access = lookup(&bus, 0x1000, 1);

        assert_eq!(access.region_id(), id(1));
        assert_eq!(access.offset(), 0);
    }

    #[test]
    fn lookup_succeeds_for_last_byte() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let access = lookup(&bus, 0x10ff, 1);

        assert_eq!(access.region_id(), id(1));
        assert_eq!(access.offset(), 0xff);
    }

    #[test]
    fn lookup_succeeds_for_exact_end_contained_access() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let access = lookup(&bus, 0x10fc, 4);

        assert_eq!(access.range(), range(0x10fc, 4));
        assert_eq!(access.offset(), 0xfc);
    }

    #[test]
    fn lookup_succeeds_for_whole_region() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let access = lookup(&bus, 0x1000, 0x100);

        assert_eq!(access.region_id(), id(1));
        assert_eq!(access.range(), range(0x1000, 0x100));
        assert_eq!(access.offset(), 0);
    }

    #[test]
    fn lookup_rejects_hole_before_first_region() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .lookup(address(0x0fff), 1)
            .expect_err("hole before first region should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x0fff, 1)
            }
        );
    }

    #[test]
    fn lookup_rejects_hole_between_regions() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);
        insert(&mut bus, 2, 0x1200, 0x100);

        let err = bus
            .lookup(address(0x1100), 1)
            .expect_err("hole between regions should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x1100, 1)
            }
        );
    }

    #[test]
    fn lookup_rejects_hole_after_last_region() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .lookup(address(0x1100), 1)
            .expect_err("hole after last region should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x1100, 1)
            }
        );
    }

    #[test]
    fn lookup_rejects_access_ending_one_byte_past_region() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .lookup(address(0x10fc), 5)
            .expect_err("access ending past region should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x10fc, 5)
            }
        );
    }

    #[test]
    fn lookup_rejects_access_crossing_adjacent_regions() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);
        insert(&mut bus, 2, 0x1100, 0x100);

        let err = bus
            .lookup(address(0x10ff), 2)
            .expect_err("cross-region access should fail");

        assert_eq!(
            err,
            MmioBusError::UnownedAccess {
                range: range(0x10ff, 2)
            }
        );
    }

    #[test]
    fn lookup_rejects_zero_sized_access() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, 0x1000, 0x100);

        let err = bus
            .lookup(address(0x1000), 0)
            .expect_err("zero-sized access should fail");

        assert_eq!(
            err,
            MmioBusError::InvalidAccessRange {
                start: address(0x1000),
                size: 0,
                source: GuestMemoryError::EmptyRange {
                    start: address(0x1000)
                }
            }
        );
    }

    #[test]
    fn lookup_rejects_access_overflow() {
        let mut bus = MmioBus::new();
        insert(&mut bus, 1, u64::MAX - 4, 4);

        let err = bus
            .lookup(address(u64::MAX - 1), 4)
            .expect_err("overflowing access should fail");

        assert_eq!(
            err,
            MmioBusError::InvalidAccessRange {
                start: address(u64::MAX - 1),
                size: 4,
                source: GuestMemoryError::AddressOverflow {
                    start: address(u64::MAX - 1),
                    size: 4
                }
            }
        );
    }

    #[test]
    fn access_bytes_copy_from_slice_preserves_data() {
        let mut source = [1, 2, 3, 4];
        let bytes = MmioAccessBytes::new(&source).expect("access bytes should be valid");

        source.fill(0xff);

        assert_eq!(bytes.len(), 4);
        assert!(!bytes.is_empty());
        assert_eq!(bytes.as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    fn access_bytes_zeroed_returns_requested_length() {
        let bytes =
            MmioAccessBytes::zeroed(MAX_MMIO_ACCESS_BYTES).expect("zeroed bytes should be valid");

        assert_eq!(bytes.len(), MAX_MMIO_ACCESS_BYTES);
        assert_eq!(bytes.as_slice(), &[0; MAX_MMIO_ACCESS_BYTES]);
    }

    #[test]
    fn access_bytes_reject_empty_data() {
        assert_eq!(
            MmioAccessBytes::new(&[]).expect_err("empty bytes should fail"),
            MmioAccessBytesError::Empty
        );
        assert_eq!(
            MmioAccessBytes::zeroed(0).expect_err("zeroed empty bytes should fail"),
            MmioAccessBytesError::Empty
        );
    }

    #[test]
    fn access_bytes_reject_unsupported_length() {
        assert_eq!(
            MmioAccessBytes::new(&[1, 2, 3]).expect_err("three-byte data should fail"),
            MmioAccessBytesError::UnsupportedLength { len: 3 }
        );
        assert_eq!(
            MmioAccessBytes::zeroed(3).expect_err("three-byte zeroed data should fail"),
            MmioAccessBytesError::UnsupportedLength { len: 3 }
        );
    }

    #[test]
    fn access_bytes_reject_oversized_data() {
        assert_eq!(
            MmioAccessBytes::new(&[0; MAX_MMIO_ACCESS_BYTES + 1])
                .expect_err("oversized bytes should fail"),
            MmioAccessBytesError::TooLarge {
                len: MAX_MMIO_ACCESS_BYTES + 1,
                max: MAX_MMIO_ACCESS_BYTES
            }
        );
        assert_eq!(
            MmioAccessBytes::zeroed(MAX_MMIO_ACCESS_BYTES + 1)
                .expect_err("oversized zeroed bytes should fail"),
            MmioAccessBytesError::TooLarge {
                len: MAX_MMIO_ACCESS_BYTES + 1,
                max: MAX_MMIO_ACCESS_BYTES
            }
        );
    }

    #[test]
    fn read_operation_creates_zeroed_data_for_access_size() {
        let access = lookup_access(0x2000, 4);
        let operation = MmioOperation::read(access).expect("read operation should be valid");

        assert_eq!(operation.kind(), MmioOperationKind::Read);
        assert_eq!(operation.access(), access);
        assert_eq!(operation.data().len(), 4);
        assert_eq!(operation.data().as_slice(), &[0, 0, 0, 0]);
    }

    #[test]
    fn write_operation_preserves_access_and_data() {
        let access = lookup_access(0x3000, 4);
        let data = MmioAccessBytes::new(&[1, 2, 3, 4]).expect("write data should be valid");
        let operation =
            MmioOperation::write(access, data).expect("write operation should be valid");

        assert_eq!(operation.kind(), MmioOperationKind::Write);
        assert_eq!(operation.access(), access);
        assert_eq!(operation.data(), data);
    }

    #[test]
    fn write_operation_rejects_data_length_mismatch() {
        let access = lookup_access(0x4000, 4);
        let data = MmioAccessBytes::new(&[1, 2]).expect("write data should be valid");
        let err =
            MmioOperation::write(access, data).expect_err("mismatched data length should fail");

        assert_eq!(
            err,
            MmioOperationError::DataLengthMismatch {
                access,
                expected: 4,
                actual: 2
            }
        );
    }

    #[test]
    fn operation_rejects_unsupported_access_size() {
        let access = lookup_access(0x5000, 3);
        let err = MmioOperation::read(access).expect_err("three-byte access should fail");

        assert_eq!(
            err,
            MmioOperationError::UnsupportedAccessSize { access, size: 3 }
        );
    }

    #[test]
    fn operation_rejects_access_larger_than_maximum() {
        let access = lookup_access(0x6000, 16);
        let err = MmioOperation::read(access).expect_err("oversized access should fail");

        assert_eq!(
            err,
            MmioOperationError::UnsupportedAccessSize { access, size: 16 }
        );
    }

    #[test]
    fn dispatcher_is_send() {
        fn assert_send<T: Send>() {}

        assert_send::<MmioDispatcher>();
    }

    #[test]
    fn dispatcher_delegates_region_registration_and_lookup() {
        let mut dispatcher = MmioDispatcher::new();
        let region = dispatcher
            .insert_region(id(7), address(0x1000), 0x100)
            .expect("dispatcher region insert should succeed");

        assert_eq!(dispatcher.bus().regions(), &[region]);
        assert_eq!(dispatcher.regions(), &[region]);
        assert_eq!(
            dispatcher
                .lookup(address(0x1040), 4)
                .expect("dispatcher lookup should succeed"),
            MmioAccess {
                region,
                range: range(0x1040, 4),
                offset: 0x40
            }
        );
    }

    #[test]
    fn dispatcher_dispatches_read_to_registered_handler() {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(id(7), address(0x1000), 0x100)
            .expect("dispatcher region insert should succeed");
        let (state, handler) = ScriptedHandler::returning(&[0xaa, 0xbb, 0xcc, 0xdd]);
        dispatcher
            .register_handler(id(7), handler)
            .expect("handler registration should succeed");
        let access = dispatcher
            .lookup(address(0x1040), 4)
            .expect("dispatcher lookup should succeed");

        let outcome = dispatcher
            .dispatch(MmioOperation::read(access).expect("read operation should be valid"))
            .expect("read dispatch should succeed");

        assert_eq!(
            outcome,
            MmioDispatchOutcome::Read {
                data: MmioAccessBytes::new(&[0xaa, 0xbb, 0xcc, 0xdd])
                    .expect("read bytes should be valid")
            }
        );
        assert_eq!(
            state
                .lock()
                .expect("handler state lock should not be poisoned")
                .reads,
            [access]
        );
    }

    #[test]
    fn dispatcher_dispatches_write_to_registered_handler() {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(id(7), address(0x2000), 0x100)
            .expect("dispatcher region insert should succeed");
        let (state, handler) = ScriptedHandler::returning(&[0]);
        dispatcher
            .register_handler(id(7), handler)
            .expect("handler registration should succeed");
        let access = dispatcher
            .lookup(address(0x2040), 4)
            .expect("dispatcher lookup should succeed");
        let data = MmioAccessBytes::new(&[1, 2, 3, 4]).expect("write bytes should be valid");

        let outcome = dispatcher
            .dispatch(MmioOperation::write(access, data).expect("write operation should be valid"))
            .expect("write dispatch should succeed");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
        assert_eq!(
            state
                .lock()
                .expect("handler state lock should not be poisoned")
                .writes,
            [(access, data)]
        );
    }

    #[test]
    fn dispatcher_rejects_duplicate_handlers() {
        let mut dispatcher = MmioDispatcher::new();
        let (_, first) = ScriptedHandler::returning(&[0]);
        let (_, second) = ScriptedHandler::returning(&[0]);

        dispatcher
            .register_handler(id(3), first)
            .expect("first handler registration should succeed");
        let err = dispatcher
            .register_handler(id(3), second)
            .expect_err("duplicate handler should fail");

        assert_eq!(
            err,
            MmioDispatchError::DuplicateHandler { region_id: id(3) }
        );
    }

    #[test]
    fn dispatcher_rejects_missing_handler() {
        let mut dispatcher = MmioDispatcher::new();
        let access = lookup_access(0x3000, 4);
        let operation = MmioOperation::read(access).expect("read operation should be valid");

        let err = dispatcher
            .dispatch(operation)
            .expect_err("missing handler should fail");

        assert_eq!(
            err,
            MmioDispatchError::MissingHandler {
                region_id: access.region_id()
            }
        );
    }

    #[test]
    fn dispatcher_surfaces_read_handler_failure() {
        let handler_error = MmioHandlerError::new("read failed");
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(id(4), address(0x4000), 0x100)
            .expect("dispatcher region insert should succeed");
        let (_, handler) = ScriptedHandler::failing(Err(handler_error.clone()), Ok(()));
        dispatcher
            .register_handler(id(4), handler)
            .expect("handler registration should succeed");
        let access = dispatcher
            .lookup(address(0x4000), 4)
            .expect("dispatcher lookup should succeed");

        let err = dispatcher
            .dispatch(MmioOperation::read(access).expect("read operation should be valid"))
            .expect_err("handler failure should fail dispatch");

        assert_eq!(
            err,
            MmioDispatchError::HandlerFailed {
                region_id: id(4),
                kind: MmioOperationKind::Read,
                source: handler_error
            }
        );
        assert!(err.source().is_some());
    }

    #[test]
    fn dispatcher_surfaces_write_handler_failure() {
        let handler_error = MmioHandlerError::new("write failed");
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(id(5), address(0x5000), 0x100)
            .expect("dispatcher region insert should succeed");
        let (_, handler) = ScriptedHandler::failing(
            Ok(MmioAccessBytes::new(&[0, 0, 0, 0]).expect("read bytes should be valid")),
            Err(handler_error.clone()),
        );
        dispatcher
            .register_handler(id(5), handler)
            .expect("handler registration should succeed");
        let access = dispatcher
            .lookup(address(0x5000), 4)
            .expect("dispatcher lookup should succeed");
        let data = MmioAccessBytes::new(&[1, 2, 3, 4]).expect("write bytes should be valid");

        let err = dispatcher
            .dispatch(MmioOperation::write(access, data).expect("write operation should be valid"))
            .expect_err("handler failure should fail dispatch");

        assert_eq!(
            err,
            MmioDispatchError::HandlerFailed {
                region_id: id(5),
                kind: MmioOperationKind::Write,
                source: handler_error
            }
        );
        assert!(err.source().is_some());
    }

    #[test]
    fn dispatcher_rejects_mismatched_read_data_length() {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(id(6), address(0x6000), 0x100)
            .expect("dispatcher region insert should succeed");
        let (_, handler) = ScriptedHandler::returning(&[1, 2]);
        dispatcher
            .register_handler(id(6), handler)
            .expect("handler registration should succeed");
        let access = dispatcher
            .lookup(address(0x6000), 4)
            .expect("dispatcher lookup should succeed");

        let err = dispatcher
            .dispatch(MmioOperation::read(access).expect("read operation should be valid"))
            .expect_err("short read data should fail dispatch");

        assert_eq!(
            err,
            MmioDispatchError::ReadDataLengthMismatch {
                access,
                expected: 4,
                actual: 2
            }
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn dispatcher_allows_one_handler_for_multiple_owner_regions() {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(id(8), address(0x8000), 0x100)
            .expect("first region insert should succeed");
        dispatcher
            .insert_region(id(8), address(0x9000), 0x100)
            .expect("second region insert should succeed");
        let (state, handler) = ScriptedHandler::returning(&[0]);
        dispatcher
            .register_handler(id(8), handler)
            .expect("handler registration should succeed");
        let first = dispatcher
            .lookup(address(0x8004), 1)
            .expect("first region lookup should succeed");
        let second = dispatcher
            .lookup(address(0x9008), 1)
            .expect("second region lookup should succeed");
        let data = MmioAccessBytes::new(&[0xee]).expect("write bytes should be valid");

        dispatcher
            .dispatch(MmioOperation::write(first, data).expect("first write should be valid"))
            .expect("first write dispatch should succeed");
        dispatcher
            .dispatch(MmioOperation::write(second, data).expect("second write should be valid"))
            .expect("second write dispatch should succeed");

        assert_eq!(
            state
                .lock()
                .expect("handler state lock should not be poisoned")
                .writes,
            [(first, data), (second, data)]
        );
    }

    #[test]
    fn displays_mmio_bus_errors() {
        let err = MmioBusError::UnownedAccess {
            range: range(0x1000, 4),
        };

        assert_eq!(
            err.to_string(),
            "MMIO access range [0x1000..0x1004) (4 bytes) is not owned by any region"
        );
    }

    #[test]
    fn displays_access_bytes_errors() {
        let err = MmioAccessBytesError::TooLarge { len: 9, max: 8 };

        assert_eq!(
            err.to_string(),
            "MMIO access byte length 9 exceeds the maximum 8"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn displays_handler_and_dispatch_errors() {
        let handler_error = MmioHandlerError::new("device failed");
        assert_eq!(handler_error.message(), "device failed");
        assert_eq!(handler_error.to_string(), "device failed");
        assert!(handler_error.source().is_none());

        let handler_failure = MmioDispatchError::HandlerFailed {
            region_id: id(9),
            kind: MmioOperationKind::Write,
            source: handler_error,
        };
        assert_eq!(
            handler_failure.to_string(),
            "MMIO write handler for region id=9 failed: device failed"
        );
        assert_eq!(
            handler_failure
                .source()
                .expect("handler failure should preserve source")
                .to_string(),
            "device failed"
        );

        assert_eq!(
            MmioDispatchError::DuplicateHandler { region_id: id(9) }.to_string(),
            "MMIO handler for region id=9 is already registered"
        );
        assert_eq!(
            MmioDispatchError::MissingHandler { region_id: id(9) }.to_string(),
            "MMIO access for region id=9 has no registered handler"
        );

        let access = lookup_access(0x8000, 4);
        let mismatch = MmioDispatchError::ReadDataLengthMismatch {
            access,
            expected: 4,
            actual: 2,
        };
        assert_eq!(
            mismatch.to_string(),
            "MMIO read handler returned 2 bytes for range [0x8000..0x8004) (4 bytes); expected 4"
        );
        assert!(mismatch.source().is_none());
    }

    #[test]
    fn displays_operation_errors() {
        let access = lookup_access(0x7000, 4);
        let err = MmioOperationError::DataLengthMismatch {
            access,
            expected: 4,
            actual: 2,
        };

        assert_eq!(
            err.to_string(),
            "MMIO operation data length 2 does not match access size 4 for range [0x7000..0x7004) (4 bytes)"
        );
        assert!(err.source().is_none());
    }
}
