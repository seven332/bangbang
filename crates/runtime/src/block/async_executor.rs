//! Bounded regular-file I/O execution for the virtio-block owner.
//!
//! This module deliberately stops below public `DriveIoEngine::Async`
//! activation. Workers own only file operations and staging buffers; guest
//! memory, rings, rate limiters, metrics, interrupts, and configuration remain
//! owner-thread concerns.

use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::FileExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use super::{BlockFileBacking, DriveCacheType};
use crate::memory::{GuestAddress, GuestMemory, GuestMemoryRange};

/// Fixed production worker count for regular-file block operations.
pub const BLOCK_ASYNC_WORKER_COUNT: usize = 4;
/// Maximum queued, running, or completed-but-unapplied host tasks process-wide.
pub const BLOCK_ASYNC_GLOBAL_TASK_LIMIT: usize = 128;
/// Maximum device-owned operations for one drive generation.
pub const BLOCK_ASYNC_PER_DRIVE_OPERATION_LIMIT: usize = 128;
/// Largest staging allocation owned by one host task.
pub const BLOCK_ASYNC_CHUNK_SIZE: usize = 1024 * 1024;
/// Maximum aggregate staging memory retained by the executor.
pub const BLOCK_ASYNC_BUFFER_BUDGET: usize = 16 * 1024 * 1024;

const NOTIFICATION_BYTES: usize = 8;
const NOTIFICATION_DRAIN_UNITS: usize = 64;

/// Validated fixed resource limits for one executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockAsyncExecutorConfig {
    worker_count: usize,
    global_task_limit: usize,
    per_drive_operation_limit: usize,
    chunk_size: usize,
    buffer_budget: usize,
}

impl BlockAsyncExecutorConfig {
    /// Describe executor limits; executor construction validates every bound.
    pub const fn new(
        worker_count: usize,
        global_task_limit: usize,
        per_drive_operation_limit: usize,
        chunk_size: usize,
        buffer_budget: usize,
    ) -> Self {
        Self {
            worker_count,
            global_task_limit,
            per_drive_operation_limit,
            chunk_size,
            buffer_budget,
        }
    }

    /// Number of fixed worker threads.
    pub const fn worker_count(self) -> usize {
        self.worker_count
    }

    /// Process-wide host-task limit.
    pub const fn global_task_limit(self) -> usize {
        self.global_task_limit
    }

    /// Per-drive admitted-operation limit.
    pub const fn per_drive_operation_limit(self) -> usize {
        self.per_drive_operation_limit
    }

    /// Maximum bytes in one staged chunk.
    pub const fn chunk_size(self) -> usize {
        self.chunk_size
    }

    /// Aggregate staged-buffer byte limit.
    pub const fn buffer_budget(self) -> usize {
        self.buffer_budget
    }

    fn validate(self) -> Result<Self, BlockAsyncExecutorBuildError> {
        if self.worker_count == 0 || self.worker_count > BLOCK_ASYNC_WORKER_COUNT {
            return Err(BlockAsyncExecutorBuildError::InvalidWorkerCount);
        }
        if self.global_task_limit == 0 || self.global_task_limit > BLOCK_ASYNC_GLOBAL_TASK_LIMIT {
            return Err(BlockAsyncExecutorBuildError::InvalidTaskLimit);
        }
        if self.per_drive_operation_limit == 0
            || self.per_drive_operation_limit > BLOCK_ASYNC_PER_DRIVE_OPERATION_LIMIT
        {
            return Err(BlockAsyncExecutorBuildError::InvalidPerDriveLimit);
        }
        if self.buffer_budget == 0 || self.buffer_budget > BLOCK_ASYNC_BUFFER_BUDGET {
            return Err(BlockAsyncExecutorBuildError::InvalidBufferBudget);
        }
        if self.chunk_size == 0
            || self.chunk_size > BLOCK_ASYNC_CHUNK_SIZE
            || self.chunk_size > self.buffer_budget
            || u32::try_from(self.chunk_size).is_err()
        {
            return Err(BlockAsyncExecutorBuildError::InvalidChunkSize);
        }
        Ok(self)
    }
}

impl Default for BlockAsyncExecutorConfig {
    fn default() -> Self {
        Self::new(
            BLOCK_ASYNC_WORKER_COUNT,
            BLOCK_ASYNC_GLOBAL_TASK_LIMIT,
            BLOCK_ASYNC_PER_DRIVE_OPERATION_LIMIT,
            BLOCK_ASYNC_CHUNK_SIZE,
            BLOCK_ASYNC_BUFFER_BUDGET,
        )
    }
}

/// Failure while building the fixed executor resources.
#[derive(Debug)]
pub enum BlockAsyncExecutorBuildError {
    InvalidWorkerCount,
    InvalidTaskLimit,
    InvalidPerDriveLimit,
    InvalidChunkSize,
    InvalidBufferBudget,
    CreateNotifier(io::ErrorKind),
    SpawnWorker(io::ErrorKind),
    WorkerPanicked,
}

impl fmt::Display for BlockAsyncExecutorBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWorkerCount => {
                formatter.write_str("invalid asynchronous block worker count")
            }
            Self::InvalidTaskLimit => formatter.write_str("invalid asynchronous block task limit"),
            Self::InvalidPerDriveLimit => {
                formatter.write_str("invalid asynchronous block per-drive limit")
            }
            Self::InvalidChunkSize => formatter.write_str("invalid asynchronous block chunk size"),
            Self::InvalidBufferBudget => {
                formatter.write_str("invalid asynchronous block buffer budget")
            }
            Self::CreateNotifier(_) => {
                formatter.write_str("failed to create asynchronous block notifier")
            }
            Self::SpawnWorker(_) => {
                formatter.write_str("failed to spawn asynchronous block worker")
            }
            Self::WorkerPanicked => {
                formatter.write_str("asynchronous block worker panicked during startup rollback")
            }
        }
    }
}

impl std::error::Error for BlockAsyncExecutorBuildError {}

/// Failure while operating or stopping an executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncExecutorError {
    Stopped,
    TaskQueueDisconnected,
    CompletionQueueDisconnected,
    Notification(io::ErrorKind),
    WorkerPanicked,
}

impl fmt::Display for BlockAsyncExecutorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => formatter.write_str("asynchronous block executor is stopped"),
            Self::TaskQueueDisconnected => {
                formatter.write_str("asynchronous block task queue is disconnected")
            }
            Self::CompletionQueueDisconnected => {
                formatter.write_str("asynchronous block completion queue is disconnected")
            }
            Self::Notification(_) => formatter.write_str("asynchronous block notification failed"),
            Self::WorkerPanicked => formatter.write_str("asynchronous block worker panicked"),
        }
    }
}

impl std::error::Error for BlockAsyncExecutorError {}

/// Reason a ready operation could not yet enter the host executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncPressure {
    GlobalTaskLimit,
    BufferBudget,
    TaskQueueFull,
}

/// Stable generation assigned to one backing lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockAsyncDriveGeneration(u64);

impl BlockAsyncDriveGeneration {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Owner-side identity needed to finish one exact virtio descriptor.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BlockAsyncRequestIdentity {
    queue_index: usize,
    descriptor_head: u16,
    status_address: GuestAddress,
}

impl BlockAsyncRequestIdentity {
    pub const fn new(
        queue_index: usize,
        descriptor_head: u16,
        status_address: GuestAddress,
    ) -> Self {
        Self {
            queue_index,
            descriptor_head,
            status_address,
        }
    }

    pub const fn queue_index(self) -> usize {
        self.queue_index
    }

    pub const fn descriptor_head(self) -> u16 {
        self.descriptor_head
    }

    pub const fn status_address(self) -> GuestAddress {
        self.status_address
    }
}

impl fmt::Debug for BlockAsyncRequestIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockAsyncRequestIdentity")
            .field("identity", &"<redacted>")
            .finish()
    }
}

/// Host operation kind supported by the internal executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncOperationKind {
    Read,
    Write,
    Flush,
}

/// Validated owner request admitted to a drive coordinator.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BlockAsyncOperation {
    identity: BlockAsyncRequestIdentity,
    kind: BlockAsyncOperationKind,
    guest_address: Option<GuestAddress>,
    host_offset: u64,
    len: u32,
}

impl BlockAsyncOperation {
    pub fn read(
        identity: BlockAsyncRequestIdentity,
        guest_address: GuestAddress,
        host_offset: u64,
        len: u32,
    ) -> Result<Self, BlockAsyncOperationBuildError> {
        Self::data(
            identity,
            BlockAsyncOperationKind::Read,
            guest_address,
            host_offset,
            len,
        )
    }

    pub fn write(
        identity: BlockAsyncRequestIdentity,
        guest_address: GuestAddress,
        host_offset: u64,
        len: u32,
    ) -> Result<Self, BlockAsyncOperationBuildError> {
        Self::data(
            identity,
            BlockAsyncOperationKind::Write,
            guest_address,
            host_offset,
            len,
        )
    }

    pub const fn flush(identity: BlockAsyncRequestIdentity) -> Self {
        Self {
            identity,
            kind: BlockAsyncOperationKind::Flush,
            guest_address: None,
            host_offset: 0,
            len: 0,
        }
    }

    fn internal_flush() -> Self {
        Self::flush(BlockAsyncRequestIdentity::new(
            usize::MAX,
            u16::MAX,
            GuestAddress::new(u64::MAX),
        ))
    }

    fn data(
        identity: BlockAsyncRequestIdentity,
        kind: BlockAsyncOperationKind,
        guest_address: GuestAddress,
        host_offset: u64,
        len: u32,
    ) -> Result<Self, BlockAsyncOperationBuildError> {
        if len == 0 {
            return Err(BlockAsyncOperationBuildError::EmptyData);
        }
        let len_u64 = u64::from(len);
        if host_offset.checked_add(len_u64).is_none()
            || guest_address.checked_add(len_u64).is_none()
        {
            return Err(BlockAsyncOperationBuildError::RangeOverflow);
        }
        Ok(Self {
            identity,
            kind,
            guest_address: Some(guest_address),
            host_offset,
            len,
        })
    }

    pub const fn identity(self) -> BlockAsyncRequestIdentity {
        self.identity
    }

    pub const fn kind(self) -> BlockAsyncOperationKind {
        self.kind
    }

    pub const fn guest_address(self) -> Option<GuestAddress> {
        self.guest_address
    }

    pub const fn host_offset(self) -> u64 {
        self.host_offset
    }

    pub const fn len(self) -> u32 {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

impl fmt::Debug for BlockAsyncOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockAsyncOperation")
            .field("kind", &self.kind)
            .field("len", &self.len)
            .field("identity", &"<redacted>")
            .field("ranges", &"<redacted>")
            .finish()
    }
}

/// Invalid asynchronous block request geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncOperationBuildError {
    EmptyData,
    RangeOverflow,
}

impl fmt::Display for BlockAsyncOperationBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyData => formatter.write_str("asynchronous block data request is empty"),
            Self::RangeOverflow => {
                formatter.write_str("asynchronous block request range overflows")
            }
        }
    }
}

impl std::error::Error for BlockAsyncOperationBuildError {}

/// Exact logical operation key retained across chunk submissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockAsyncOperationKey {
    generation: BlockAsyncDriveGeneration,
    operation_id: u64,
    sequence: u64,
}

impl BlockAsyncOperationKey {
    pub const fn generation(self) -> BlockAsyncDriveGeneration {
        self.generation
    }

    pub const fn operation_id(self) -> u64 {
        self.operation_id
    }

    pub const fn sequence(self) -> u64 {
        self.sequence
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BlockAsyncChunkKey {
    operation: BlockAsyncOperationKey,
    offset: u32,
    len: u32,
    host_offset: u64,
}

impl fmt::Debug for BlockAsyncChunkKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockAsyncChunkKey")
            .field("operation", &self.operation)
            .field("offset", &self.offset)
            .field("len", &self.len)
            .field("host_offset", &"<redacted>")
            .finish()
    }
}

/// One host transfer result, including bytes completed before an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockAsyncTransferResult {
    bytes_transferred: usize,
    error: Option<io::ErrorKind>,
}

impl BlockAsyncTransferResult {
    pub const fn complete(bytes_transferred: usize) -> Self {
        Self {
            bytes_transferred,
            error: None,
        }
    }

    pub const fn failed(bytes_transferred: usize, error: io::ErrorKind) -> Self {
        Self {
            bytes_transferred,
            error: Some(error),
        }
    }

    pub const fn bytes_transferred(self) -> usize {
        self.bytes_transferred
    }

    pub const fn error(self) -> Option<io::ErrorKind> {
        self.error
    }
}

/// Injectable host boundary used by deterministic executor tests.
pub trait BlockAsyncHostIo: Send + Sync + 'static {
    fn read_at(
        &self,
        backing: &BlockFileBacking,
        offset: u64,
        destination: &mut [u8],
    ) -> BlockAsyncTransferResult;

    fn write_at(
        &self,
        backing: &BlockFileBacking,
        offset: u64,
        source: &[u8],
    ) -> BlockAsyncTransferResult;

    fn flush(&self, backing: &BlockFileBacking) -> Result<(), io::ErrorKind>;
}

#[derive(Debug)]
struct SystemBlockAsyncHostIo;

impl BlockAsyncHostIo for SystemBlockAsyncHostIo {
    fn read_at(
        &self,
        backing: &BlockFileBacking,
        offset: u64,
        destination: &mut [u8],
    ) -> BlockAsyncTransferResult {
        loop {
            match backing.file.read_at(destination, offset) {
                Ok(bytes) => return BlockAsyncTransferResult::complete(bytes),
                Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                Err(source) => {
                    return BlockAsyncTransferResult::failed(0, source.kind());
                }
            }
        }
    }

    fn write_at(
        &self,
        backing: &BlockFileBacking,
        offset: u64,
        source: &[u8],
    ) -> BlockAsyncTransferResult {
        if backing.is_read_only {
            return BlockAsyncTransferResult::failed(0, io::ErrorKind::PermissionDenied);
        }
        loop {
            match backing.file.write_at(source, offset) {
                Ok(bytes) => return BlockAsyncTransferResult::complete(bytes),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return BlockAsyncTransferResult::failed(0, error.kind()),
            }
        }
    }

    fn flush(&self, backing: &BlockFileBacking) -> Result<(), io::ErrorKind> {
        backing.file.sync_all().map_err(|source| source.kind())
    }
}

#[derive(Debug)]
struct ResourceBudget {
    task_limit: usize,
    byte_limit: usize,
    tasks: AtomicUsize,
    bytes: AtomicUsize,
}

impl ResourceBudget {
    fn new(task_limit: usize, byte_limit: usize) -> Self {
        Self {
            task_limit,
            byte_limit,
            tasks: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
        }
    }

    fn try_reserve_task(self: &Arc<Self>) -> Option<ResourceLease> {
        reserve_counter(&self.tasks, self.task_limit, 1).then(|| ResourceLease {
            budget: Arc::clone(self),
            kind: ResourceLeaseKind::Task,
            amount: 1,
        })
    }

    fn try_reserve_bytes(self: &Arc<Self>, bytes: usize) -> Option<ResourceLease> {
        if bytes == 0 {
            return Some(ResourceLease {
                budget: Arc::clone(self),
                kind: ResourceLeaseKind::Bytes,
                amount: 0,
            });
        }
        reserve_counter(&self.bytes, self.byte_limit, bytes).then(|| ResourceLease {
            budget: Arc::clone(self),
            kind: ResourceLeaseKind::Bytes,
            amount: bytes,
        })
    }
}

fn reserve_counter(counter: &AtomicUsize, limit: usize, amount: usize) -> bool {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(amount).filter(|next| *next <= limit)
        })
        .is_ok()
}

#[derive(Debug, Clone, Copy)]
enum ResourceLeaseKind {
    Task,
    Bytes,
}

struct ResourceLease {
    budget: Arc<ResourceBudget>,
    kind: ResourceLeaseKind,
    amount: usize,
}

impl fmt::Debug for ResourceLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ResourceLease")
            .field(&"<owned>")
            .finish()
    }
}

impl Drop for ResourceLease {
    fn drop(&mut self) {
        if self.amount == 0 {
            return;
        }
        let previous = match self.kind {
            ResourceLeaseKind::Task => self.budget.tasks.fetch_sub(self.amount, Ordering::AcqRel),
            ResourceLeaseKind::Bytes => self.budget.bytes.fetch_sub(self.amount, Ordering::AcqRel),
        };
        debug_assert!(previous >= self.amount, "resource lease must release once");
    }
}

struct StagedBuffer {
    bytes: Vec<u8>,
    _lease: ResourceLease,
}

impl fmt::Debug for StagedBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagedBuffer")
            .field("len", &self.bytes.len())
            .field("contents", &"<redacted>")
            .finish()
    }
}

enum HostTaskPayload {
    Read(StagedBuffer),
    Write(StagedBuffer),
    Flush,
}

impl fmt::Debug for HostTaskPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(buffer) => formatter.debug_tuple("Read").field(buffer).finish(),
            Self::Write(buffer) => formatter.debug_tuple("Write").field(buffer).finish(),
            Self::Flush => formatter.write_str("Flush"),
        }
    }
}

struct HostTask {
    key: BlockAsyncChunkKey,
    backing: Arc<BlockFileBacking>,
    payload: HostTaskPayload,
    queued_at: Instant,
    _task_lease: ResourceLease,
}

impl fmt::Debug for HostTask {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostTask")
            .field("key", &self.key)
            .field("backing", &"<owned>")
            .field("payload", &self.payload)
            .finish()
    }
}

enum WorkerMessage {
    Task(HostTask),
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostCompletionResult {
    Transfer(BlockAsyncTransferResult),
    Flush(Result<(), io::ErrorKind>),
    Panicked,
}

/// Host completion retained until exact owner application or discard.
pub struct BlockAsyncHostCompletion {
    key: BlockAsyncChunkKey,
    result: HostCompletionResult,
    payload: HostTaskPayload,
    queue_latency_us: u64,
    host_latency_us: u64,
    _task_lease: ResourceLease,
}

impl BlockAsyncHostCompletion {
    pub const fn operation_key(&self) -> BlockAsyncOperationKey {
        self.key.operation
    }

    pub const fn chunk_offset(&self) -> u32 {
        self.key.offset
    }

    pub const fn chunk_len(&self) -> u32 {
        self.key.len
    }
}

impl fmt::Debug for BlockAsyncHostCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockAsyncHostCompletion")
            .field("key", &self.key)
            .field("result", &self.result)
            .field("payload", &self.payload)
            .field("queue_latency_us", &self.queue_latency_us)
            .field("host_latency_us", &self.host_latency_us)
            .finish()
    }
}

#[derive(Debug, Default)]
struct WorkerHealth {
    completion_disconnected: AtomicBool,
    notifier_error: Mutex<Option<io::ErrorKind>>,
}

impl WorkerHealth {
    fn record_notifier_error(&self, error: io::ErrorKind) {
        if let Ok(mut current) = self.notifier_error.lock()
            && current.is_none()
        {
            *current = Some(error);
        }
    }

    fn notifier_error(&self) -> Option<io::ErrorKind> {
        self.notifier_error.lock().ok().and_then(|error| *error)
    }
}

#[derive(Debug)]
struct CompletionSignal {
    descriptor: Arc<OwnedFd>,
}

impl CompletionSignal {
    fn signal(&self) -> Result<BlockAsyncSignalOutcome, io::ErrorKind> {
        let bytes = [0_u8; NOTIFICATION_BYTES];
        loop {
            // SAFETY: The descriptor is a live nonblocking pipe writer and the
            // fixed byte array is readable for its complete length.
            let result = unsafe {
                libc::write(
                    self.descriptor.as_raw_fd(),
                    bytes.as_ptr().cast(),
                    bytes.len(),
                )
            };
            if result < 0 {
                let error = io::Error::last_os_error();
                match error.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => return Ok(BlockAsyncSignalOutcome::Coalesced),
                    kind => return Err(kind),
                }
            }
            return if usize::try_from(result).ok() == Some(NOTIFICATION_BYTES) {
                Ok(BlockAsyncSignalOutcome::Signaled)
            } else {
                Err(io::ErrorKind::InvalidData)
            };
        }
    }
}

/// Outcome of a nonblocking completion signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncSignalOutcome {
    Signaled,
    Coalesced,
}

#[derive(Debug)]
struct CompletionNotifier {
    descriptor: OwnedFd,
}

impl CompletionNotifier {
    fn drain(&self) -> Result<BlockAsyncNotificationDrain, io::ErrorKind> {
        let mut buffer = [0_u8; NOTIFICATION_BYTES * NOTIFICATION_DRAIN_UNITS];
        let mut notifications = 0usize;
        loop {
            // SAFETY: The descriptor is a live nonblocking pipe reader and the
            // buffer is writable for its complete declared length.
            let result = unsafe {
                libc::read(
                    self.descriptor.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if result == 0 {
                return Ok(BlockAsyncNotificationDrain::Closed { notifications });
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                match error.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock if notifications == 0 => {
                        return Ok(BlockAsyncNotificationDrain::WouldBlock);
                    }
                    io::ErrorKind::WouldBlock => {
                        return Ok(BlockAsyncNotificationDrain::Drained { notifications });
                    }
                    kind => return Err(kind),
                }
            }
            let bytes = usize::try_from(result).map_err(|_| io::ErrorKind::InvalidData)?;
            if bytes == 0 || bytes % NOTIFICATION_BYTES != 0 {
                return Err(io::ErrorKind::InvalidData);
            }
            notifications = notifications
                .checked_add(bytes / NOTIFICATION_BYTES)
                .ok_or(io::ErrorKind::InvalidData)?;
        }
    }
}

/// Result of draining the completion pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncNotificationDrain {
    WouldBlock,
    Drained { notifications: usize },
    Closed { notifications: usize },
}

fn create_completion_notifier()
-> Result<(CompletionNotifier, CompletionSignal), BlockAsyncExecutorBuildError> {
    let mut descriptors = [0_i32; 2];
    // SAFETY: `descriptors` contains the two writable entries required by
    // `pipe`; successful descriptors are adopted exactly once below.
    if unsafe { libc::pipe(descriptors.as_mut_ptr()) } != 0 {
        return Err(BlockAsyncExecutorBuildError::CreateNotifier(
            io::Error::last_os_error().kind(),
        ));
    }
    let reader_raw =
        descriptors
            .first()
            .copied()
            .ok_or(BlockAsyncExecutorBuildError::CreateNotifier(
                io::ErrorKind::InvalidData,
            ))?;
    let writer_raw =
        descriptors
            .get(1)
            .copied()
            .ok_or(BlockAsyncExecutorBuildError::CreateNotifier(
                io::ErrorKind::InvalidData,
            ))?;
    // SAFETY: A successful `pipe` returned each descriptor once and both are
    // transferred immediately into unique owners.
    let reader = unsafe { OwnedFd::from_raw_fd(reader_raw) };
    // SAFETY: See the successful `pipe` ownership argument above.
    let writer = unsafe { OwnedFd::from_raw_fd(writer_raw) };
    configure_pipe_descriptor(reader.as_raw_fd(), false)
        .map_err(BlockAsyncExecutorBuildError::CreateNotifier)?;
    configure_pipe_descriptor(writer.as_raw_fd(), true)
        .map_err(BlockAsyncExecutorBuildError::CreateNotifier)?;
    Ok((
        CompletionNotifier { descriptor: reader },
        CompletionSignal {
            descriptor: Arc::new(writer),
        },
    ))
}

fn configure_pipe_descriptor(descriptor: RawFd, writer: bool) -> Result<(), io::ErrorKind> {
    let status = retry_fcntl(descriptor, libc::F_GETFL, 0)?;
    retry_fcntl(descriptor, libc::F_SETFL, status | libc::O_NONBLOCK)?;
    let descriptor_flags = retry_fcntl(descriptor, libc::F_GETFD, 0)?;
    retry_fcntl(
        descriptor,
        libc::F_SETFD,
        descriptor_flags | libc::FD_CLOEXEC,
    )?;
    if writer {
        suppress_pipe_sigpipe(descriptor)?;
    }
    Ok(())
}

fn retry_fcntl(
    descriptor: RawFd,
    command: libc::c_int,
    argument: libc::c_int,
) -> Result<libc::c_int, io::ErrorKind> {
    loop {
        // SAFETY: Callers provide integer-only fcntl operations valid for a
        // live borrowed descriptor; no pointer argument is involved.
        let result = unsafe { libc::fcntl(descriptor, command, argument) };
        if result >= 0 {
            return Ok(result);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error.kind());
        }
    }
}

#[cfg(target_vendor = "apple")]
fn suppress_pipe_sigpipe(descriptor: RawFd) -> Result<(), io::ErrorKind> {
    const DARWIN_F_SETNOSIGPIPE: libc::c_int = 73;
    retry_fcntl(descriptor, DARWIN_F_SETNOSIGPIPE, 1).map(|_| ())
}

#[cfg(not(target_vendor = "apple"))]
fn suppress_pipe_sigpipe(_descriptor: RawFd) -> Result<(), io::ErrorKind> {
    Ok(())
}

#[derive(Debug)]
struct ExecutorShared {
    sender: mpsc::SyncSender<WorkerMessage>,
    signal: CompletionSignal,
    budget: Arc<ResourceBudget>,
    accepting: AtomicBool,
    submission_gate: Mutex<()>,
    config: BlockAsyncExecutorConfig,
}

/// Cloneable non-owning submission handle for one process executor.
#[derive(Clone)]
pub struct BlockAsyncExecutorHandle {
    shared: Weak<ExecutorShared>,
}

impl fmt::Debug for BlockAsyncExecutorHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("BlockAsyncExecutorHandle")
            .field(&"<non-owning>")
            .finish()
    }
}

impl BlockAsyncExecutorHandle {
    fn reserve(
        &self,
        buffer_bytes: usize,
    ) -> Result<BlockAsyncSubmissionPermit, BlockAsyncReserveError> {
        let shared = self
            .shared
            .upgrade()
            .filter(|shared| shared.accepting.load(Ordering::Acquire))
            .ok_or(BlockAsyncReserveError::Stopped)?;
        let task_lease =
            shared
                .budget
                .try_reserve_task()
                .ok_or(BlockAsyncReserveError::Pressure(
                    BlockAsyncPressure::GlobalTaskLimit,
                ))?;
        let buffer_lease = shared.budget.try_reserve_bytes(buffer_bytes).ok_or(
            BlockAsyncReserveError::Pressure(BlockAsyncPressure::BufferBudget),
        )?;
        Ok(BlockAsyncSubmissionPermit {
            shared,
            task_lease,
            buffer_lease,
        })
    }

    pub fn is_accepting(&self) -> bool {
        self.shared
            .upgrade()
            .is_some_and(|shared| shared.accepting.load(Ordering::Acquire))
    }

    pub fn outstanding_tasks(&self) -> usize {
        self.shared
            .upgrade()
            .map_or(0, |shared| shared.budget.tasks.load(Ordering::Acquire))
    }

    pub fn reserved_buffer_bytes(&self) -> usize {
        self.shared
            .upgrade()
            .map_or(0, |shared| shared.budget.bytes.load(Ordering::Acquire))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockAsyncReserveError {
    Pressure(BlockAsyncPressure),
    Stopped,
}

struct BlockAsyncSubmissionPermit {
    shared: Arc<ExecutorShared>,
    task_lease: ResourceLease,
    buffer_lease: ResourceLease,
}

impl BlockAsyncSubmissionPermit {
    fn submit(
        self,
        key: BlockAsyncChunkKey,
        backing: Arc<BlockFileBacking>,
        payload: PreparedPayload,
        queued_at: Instant,
    ) -> Result<(), BlockAsyncSubmitError> {
        let shared = Arc::clone(&self.shared);
        let _submission_guard = shared
            .submission_gate
            .lock()
            .map_err(|_| BlockAsyncSubmitError::Disconnected)?;
        if !shared.accepting.load(Ordering::Acquire) {
            return Err(BlockAsyncSubmitError::Stopped);
        }
        let payload = match payload {
            PreparedPayload::Read(bytes) => HostTaskPayload::Read(StagedBuffer {
                bytes,
                _lease: self.buffer_lease,
            }),
            PreparedPayload::Write(bytes) => HostTaskPayload::Write(StagedBuffer {
                bytes,
                _lease: self.buffer_lease,
            }),
            PreparedPayload::Flush => {
                drop(self.buffer_lease);
                HostTaskPayload::Flush
            }
        };
        let task = HostTask {
            key,
            backing,
            payload,
            queued_at,
            _task_lease: self.task_lease,
        };
        match shared.sender.try_send(WorkerMessage::Task(task)) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => Err(BlockAsyncSubmitError::Full),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(BlockAsyncSubmitError::Disconnected),
        }
    }
}

enum PreparedPayload {
    Read(Vec<u8>),
    Write(Vec<u8>),
    Flush,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockAsyncSubmitError {
    Stopped,
    Full,
    Disconnected,
}

/// Fixed process executor and its owner-side completion endpoint.
pub struct BlockAsyncExecutor {
    config: BlockAsyncExecutorConfig,
    shared: Option<Arc<ExecutorShared>>,
    completions: mpsc::Receiver<BlockAsyncHostCompletion>,
    notifier: Option<CompletionNotifier>,
    health: Arc<WorkerHealth>,
    workers: Vec<JoinHandle<()>>,
    shutdown: bool,
}

impl fmt::Debug for BlockAsyncExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockAsyncExecutor")
            .field("config", &self.config)
            .field("outstanding_tasks", &self.outstanding_tasks())
            .field("reserved_buffer_bytes", &self.reserved_buffer_bytes())
            .field("state", &if self.shutdown { "stopped" } else { "running" })
            .finish()
    }
}

impl BlockAsyncExecutor {
    pub fn new() -> Result<Self, BlockAsyncExecutorBuildError> {
        Self::with_config_and_host(
            BlockAsyncExecutorConfig::default(),
            Arc::new(SystemBlockAsyncHostIo),
        )
    }

    pub fn with_config_and_host(
        config: BlockAsyncExecutorConfig,
        host: Arc<dyn BlockAsyncHostIo>,
    ) -> Result<Self, BlockAsyncExecutorBuildError> {
        let config = config.validate()?;
        let (notifier, signal) = create_completion_notifier()?;
        let (sender, receiver) = mpsc::sync_channel(config.global_task_limit());
        let receiver = Arc::new(Mutex::new(receiver));
        let (completion_sender, completions) = mpsc::channel();
        let budget = Arc::new(ResourceBudget::new(
            config.global_task_limit(),
            config.buffer_budget(),
        ));
        let shared = Arc::new(ExecutorShared {
            sender,
            signal,
            budget,
            accepting: AtomicBool::new(true),
            submission_gate: Mutex::new(()),
            config,
        });
        let health = Arc::new(WorkerHealth::default());
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        workers
            .try_reserve_exact(config.worker_count())
            .map_err(|_| BlockAsyncExecutorBuildError::SpawnWorker(io::ErrorKind::OutOfMemory))?;

        for index in 0..config.worker_count() {
            let worker_receiver = Arc::clone(&receiver);
            let worker_completions = completion_sender.clone();
            let worker_signal = CompletionSignal {
                descriptor: Arc::clone(&shared.signal.descriptor),
            };
            let worker_host = Arc::clone(&host);
            let worker_health = Arc::clone(&health);
            let name = format!("bangbang-block-io-{index}");
            let spawn = thread::Builder::new().name(name).spawn(move || {
                run_worker(
                    &worker_receiver,
                    &worker_completions,
                    &worker_signal,
                    worker_host.as_ref(),
                    &worker_health,
                );
            });
            let worker = match spawn {
                Ok(worker) => worker,
                Err(source) => {
                    shared.accepting.store(false, Ordering::Release);
                    for _ in 0..workers.len() {
                        let _ = shared.sender.send(WorkerMessage::Stop);
                    }
                    let mut panicked = false;
                    for worker in workers {
                        panicked |= worker.join().is_err();
                    }
                    return Err(if panicked {
                        BlockAsyncExecutorBuildError::WorkerPanicked
                    } else {
                        BlockAsyncExecutorBuildError::SpawnWorker(source.kind())
                    });
                }
            };
            workers.push(worker);
        }
        drop(completion_sender);

        Ok(Self {
            config,
            shared: Some(shared),
            completions,
            notifier: Some(notifier),
            health,
            workers,
            shutdown: false,
        })
    }

    pub const fn config(&self) -> BlockAsyncExecutorConfig {
        self.config
    }

    pub fn handle(&self) -> BlockAsyncExecutorHandle {
        BlockAsyncExecutorHandle {
            shared: self.shared.as_ref().map_or_else(Weak::new, Arc::downgrade),
        }
    }

    pub fn completion_fd(&self) -> Option<RawFd> {
        self.notifier
            .as_ref()
            .map(|notifier| notifier.descriptor.as_raw_fd())
    }

    pub fn drain_notification(
        &self,
    ) -> Result<BlockAsyncNotificationDrain, BlockAsyncExecutorError> {
        self.notifier
            .as_ref()
            .ok_or(BlockAsyncExecutorError::Stopped)?
            .drain()
            .map_err(BlockAsyncExecutorError::Notification)
    }

    pub fn try_recv_completion(
        &self,
    ) -> Result<Option<BlockAsyncHostCompletion>, BlockAsyncExecutorError> {
        match self.completions.try_recv() {
            Ok(completion) => Ok(Some(completion)),
            Err(mpsc::TryRecvError::Empty) => {
                if self.health.completion_disconnected.load(Ordering::Acquire) {
                    Err(BlockAsyncExecutorError::CompletionQueueDisconnected)
                } else {
                    Ok(None)
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(BlockAsyncExecutorError::CompletionQueueDisconnected)
            }
        }
    }

    pub fn recv_completion(&self) -> Result<BlockAsyncHostCompletion, BlockAsyncExecutorError> {
        self.completions
            .recv()
            .map_err(|_| BlockAsyncExecutorError::CompletionQueueDisconnected)
    }

    pub fn notification_health(&self) -> Result<(), BlockAsyncExecutorError> {
        self.health.notifier_error().map_or(Ok(()), |error| {
            Err(BlockAsyncExecutorError::Notification(error))
        })
    }

    pub fn outstanding_tasks(&self) -> usize {
        self.shared
            .as_ref()
            .map_or(0, |shared| shared.budget.tasks.load(Ordering::Acquire))
    }

    pub fn reserved_buffer_bytes(&self) -> usize {
        self.shared
            .as_ref()
            .map_or(0, |shared| shared.budget.bytes.load(Ordering::Acquire))
    }

    pub fn stop_admission(&self) {
        if let Some(shared) = self.shared.as_ref() {
            shared.accepting.store(false, Ordering::Release);
        }
    }

    pub fn shutdown(&mut self) -> Result<(), BlockAsyncExecutorError> {
        if self.shutdown {
            return Ok(());
        }
        self.stop_admission();
        let mut primary_error = None;
        if let Some(shared) = self.shared.as_ref() {
            let _submission_guard = match shared.submission_gate.lock() {
                Ok(guard) => guard,
                Err(poisoned) => {
                    primary_error.get_or_insert(BlockAsyncExecutorError::TaskQueueDisconnected);
                    poisoned.into_inner()
                }
            };
            for _ in 0..self.workers.len() {
                if shared.sender.send(WorkerMessage::Stop).is_err() {
                    primary_error.get_or_insert(BlockAsyncExecutorError::TaskQueueDisconnected);
                    break;
                }
            }
        }
        let mut panicked = false;
        while let Some(worker) = self.workers.pop() {
            panicked |= worker.join().is_err();
        }
        while let Ok(completion) = self.completions.try_recv() {
            drop(completion);
        }
        if panicked {
            primary_error.get_or_insert(BlockAsyncExecutorError::WorkerPanicked);
        }
        if let Some(error) = self.health.notifier_error() {
            primary_error.get_or_insert(BlockAsyncExecutorError::Notification(error));
        }
        if let Some(notifier) = self.notifier.as_ref()
            && let Err(error) = notifier.drain()
        {
            primary_error.get_or_insert(BlockAsyncExecutorError::Notification(error));
        }
        drop(self.shared.take());
        drop(self.notifier.take());
        self.shutdown = true;
        primary_error.map_or(Ok(()), Err)
    }
}

impl Drop for BlockAsyncExecutor {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn run_worker(
    receiver: &Mutex<mpsc::Receiver<WorkerMessage>>,
    completions: &mpsc::Sender<BlockAsyncHostCompletion>,
    signal: &CompletionSignal,
    host: &dyn BlockAsyncHostIo,
    health: &WorkerHealth,
) {
    loop {
        let message = match receiver.lock() {
            Ok(receiver) => receiver.recv(),
            Err(_) => return,
        };
        let task = match message {
            Ok(WorkerMessage::Task(task)) => task,
            Ok(WorkerMessage::Stop) | Err(_) => return,
        };
        let completion = execute_host_task(task, host);
        if completions.send(completion).is_err() {
            health
                .completion_disconnected
                .store(true, Ordering::Release);
            continue;
        }
        if let Err(error) = signal.signal() {
            health.record_notifier_error(error);
        }
    }
}

fn execute_host_task(mut task: HostTask, host: &dyn BlockAsyncHostIo) -> BlockAsyncHostCompletion {
    let host_started_at = Instant::now();
    let result = catch_unwind(AssertUnwindSafe(|| match &mut task.payload {
        HostTaskPayload::Read(buffer) => HostCompletionResult::Transfer(host.read_at(
            &task.backing,
            task.key.host_offset,
            &mut buffer.bytes,
        )),
        HostTaskPayload::Write(buffer) => HostCompletionResult::Transfer(host.write_at(
            &task.backing,
            task.key.host_offset,
            &buffer.bytes,
        )),
        HostTaskPayload::Flush => HostCompletionResult::Flush(host.flush(&task.backing)),
    }))
    .unwrap_or(HostCompletionResult::Panicked);
    let finished_at = Instant::now();
    BlockAsyncHostCompletion {
        key: task.key,
        result,
        payload: task.payload,
        queue_latency_us: elapsed_us(task.queued_at, finished_at),
        host_latency_us: elapsed_us(host_started_at, finished_at),
        _task_lease: task._task_lease,
    }
}

fn elapsed_us(start: Instant, end: Instant) -> u64 {
    u64::try_from(end.saturating_duration_since(start).as_micros()).unwrap_or(u64::MAX)
}

/// Failure while binding a drive generation to the process executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncDriveBuildError {
    ExecutorStopped,
}

impl fmt::Display for BlockAsyncDriveBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExecutorStopped => {
                formatter.write_str("asynchronous block executor is not accepting drives")
            }
        }
    }
}

impl std::error::Error for BlockAsyncDriveBuildError {}

/// Admission failure before an operation owns a per-drive slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncAdmissionError {
    Stopped,
    DriveFull,
    BackingRangeOutOfBounds,
    OperationIdExhausted,
    SequenceExhausted,
    MetadataAllocation,
}

impl fmt::Display for BlockAsyncAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stopped => formatter.write_str("asynchronous block drive is stopped"),
            Self::DriveFull => formatter.write_str("asynchronous block drive is full"),
            Self::BackingRangeOutOfBounds => {
                formatter.write_str("asynchronous block operation exceeds its backing")
            }
            Self::OperationIdExhausted => {
                formatter.write_str("asynchronous block operation IDs are exhausted")
            }
            Self::SequenceExhausted => {
                formatter.write_str("asynchronous block operation sequences are exhausted")
            }
            Self::MetadataAllocation => {
                formatter.write_str("asynchronous block operation metadata allocation failed")
            }
        }
    }
}

impl std::error::Error for BlockAsyncAdmissionError {}

/// Terminal failure for one admitted operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncOperationFailure {
    BufferAllocation,
    GuestMemoryAccess,
    Host {
        operation: BlockAsyncOperationKind,
        bytes_transferred: usize,
        source: io::ErrorKind,
    },
    ShortIo {
        operation: BlockAsyncOperationKind,
        expected: usize,
        actual: usize,
    },
    WorkerPanicked,
    InvalidHostCompletion,
}

/// Final operation status returned to the VMM owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncOperationStatus {
    Success,
    Failed(BlockAsyncOperationFailure),
}

/// Final owner-side result for one exact request.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BlockAsyncOperationCompletion {
    key: BlockAsyncOperationKey,
    identity: BlockAsyncRequestIdentity,
    kind: BlockAsyncOperationKind,
    status: BlockAsyncOperationStatus,
    bytes_transferred: u64,
    dirty_range: Option<GuestMemoryRange>,
    queue_latency_us: u64,
    host_latency_us: u64,
    total_latency_us: u64,
    internal: bool,
}

impl BlockAsyncOperationCompletion {
    pub const fn key(self) -> BlockAsyncOperationKey {
        self.key
    }

    pub const fn identity(self) -> BlockAsyncRequestIdentity {
        self.identity
    }

    pub const fn kind(self) -> BlockAsyncOperationKind {
        self.kind
    }

    pub const fn status(self) -> BlockAsyncOperationStatus {
        self.status
    }

    pub const fn bytes_transferred(self) -> u64 {
        self.bytes_transferred
    }

    pub const fn dirty_range(self) -> Option<GuestMemoryRange> {
        self.dirty_range
    }

    pub const fn queue_latency_us(self) -> u64 {
        self.queue_latency_us
    }

    pub const fn host_latency_us(self) -> u64 {
        self.host_latency_us
    }

    pub const fn total_latency_us(self) -> u64 {
        self.total_latency_us
    }
}

impl fmt::Debug for BlockAsyncOperationCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockAsyncOperationCompletion")
            .field("key", &self.key)
            .field("identity", &"<redacted>")
            .field("kind", &self.kind)
            .field("status", &self.status)
            .field("bytes_transferred", &self.bytes_transferred)
            .field("dirty_range", &self.dirty_range.map(|_| "<redacted>"))
            .field("queue_latency_us", &self.queue_latency_us)
            .field("host_latency_us", &self.host_latency_us)
            .field("total_latency_us", &self.total_latency_us)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationProgress {
    Pending,
    InFlight(BlockAsyncChunkKey),
}

#[derive(Debug)]
struct DriveOperation {
    key: BlockAsyncOperationKey,
    operation: BlockAsyncOperation,
    admitted_at: Instant,
    next_offset: u32,
    completed_bytes: u64,
    queue_latency_us: u64,
    host_latency_us: u64,
    progress: OperationProgress,
    internal: bool,
}

/// One owner-side scheduling attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncScheduleOutcome {
    Idle,
    Pressure(BlockAsyncPressure),
    Submitted {
        operation: BlockAsyncOperationKey,
        chunk_offset: u32,
        chunk_len: u32,
    },
    Completed(BlockAsyncOperationCompletion),
}

/// Scheduler failure that cannot be retried merely by applying a completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncScheduleError {
    ExecutorStopped,
    TaskQueueDisconnected,
    InvariantViolation,
}

impl fmt::Display for BlockAsyncScheduleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExecutorStopped => formatter.write_str("asynchronous block executor is stopped"),
            Self::TaskQueueDisconnected => {
                formatter.write_str("asynchronous block task queue is disconnected")
            }
            Self::InvariantViolation => {
                formatter.write_str("asynchronous block scheduler invariant was violated")
            }
        }
    }
}

impl std::error::Error for BlockAsyncScheduleError {}

/// Whether an owner applies or safely discards a returned host result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncCompletionDisposition {
    Apply,
    Discard,
}

/// Result of matching one host completion against a drive generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncApplyOutcome {
    Advanced(BlockAsyncOperationKey),
    Completed(BlockAsyncOperationCompletion),
    Discarded(BlockAsyncOperationKey),
    Stale(BlockAsyncOperationKey),
}

/// Failure to match a completion to exact owner-side state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncApplyError {
    UnknownOperation,
    UnexpectedChunk,
}

impl fmt::Display for BlockAsyncApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOperation => {
                formatter.write_str("asynchronous block completion has no matching operation")
            }
            Self::UnexpectedChunk => {
                formatter.write_str("asynchronous block completion does not match the active chunk")
            }
        }
    }
}

impl std::error::Error for BlockAsyncApplyError {}

/// Generation-bound owner-side coordinator for one block backing.
pub struct BlockAsyncDrive {
    generation: BlockAsyncDriveGeneration,
    backing: Arc<BlockFileBacking>,
    cache_type: DriveCacheType,
    executor: BlockAsyncExecutorHandle,
    config: BlockAsyncExecutorConfig,
    operations: VecDeque<DriveOperation>,
    accepting: bool,
    next_operation_id: u64,
    next_sequence: u64,
}

impl fmt::Debug for BlockAsyncDrive {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockAsyncDrive")
            .field("generation", &self.generation)
            .field("backing", &"<owned>")
            .field("cache_type", &self.cache_type)
            .field("operations", &self.operations.len())
            .field("accepting", &self.accepting)
            .finish()
    }
}

impl BlockAsyncDrive {
    pub fn new(
        generation: BlockAsyncDriveGeneration,
        backing: Arc<BlockFileBacking>,
        cache_type: DriveCacheType,
        executor: BlockAsyncExecutorHandle,
    ) -> Result<Self, BlockAsyncDriveBuildError> {
        let config = executor
            .shared
            .upgrade()
            .filter(|shared| shared.accepting.load(Ordering::Acquire))
            .map(|shared| shared.config)
            .ok_or(BlockAsyncDriveBuildError::ExecutorStopped)?;
        Ok(Self {
            generation,
            backing,
            cache_type,
            executor,
            config,
            operations: VecDeque::new(),
            accepting: true,
            next_operation_id: 1,
            next_sequence: 1,
        })
    }

    pub const fn generation(&self) -> BlockAsyncDriveGeneration {
        self.generation
    }

    pub const fn cache_type(&self) -> DriveCacheType {
        self.cache_type
    }

    pub fn operation_count(&self) -> usize {
        self.operations.len()
    }

    pub fn inflight_count(&self) -> usize {
        self.operations
            .iter()
            .filter(|operation| matches!(operation.progress, OperationProgress::InFlight(_)))
            .count()
    }

    pub fn is_drained(&self) -> bool {
        self.operations.is_empty()
    }

    pub const fn is_accepting(&self) -> bool {
        self.accepting
    }

    pub fn admit(
        &mut self,
        operation: BlockAsyncOperation,
    ) -> Result<BlockAsyncOperationKey, BlockAsyncAdmissionError> {
        if !self.accepting || !self.executor.is_accepting() {
            return Err(BlockAsyncAdmissionError::Stopped);
        }
        if self.operations.len() >= self.config.per_drive_operation_limit() {
            return Err(BlockAsyncAdmissionError::DriveFull);
        }
        if operation.kind() != BlockAsyncOperationKind::Flush
            && operation
                .host_offset()
                .checked_add(u64::from(operation.len()))
                .is_none_or(|end| end > self.backing.len())
        {
            return Err(BlockAsyncAdmissionError::BackingRangeOutOfBounds);
        }
        let next_operation_id = self
            .next_operation_id
            .checked_add(1)
            .ok_or(BlockAsyncAdmissionError::OperationIdExhausted)?;
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(BlockAsyncAdmissionError::SequenceExhausted)?;
        self.operations
            .try_reserve(1)
            .map_err(|_| BlockAsyncAdmissionError::MetadataAllocation)?;
        let key = BlockAsyncOperationKey {
            generation: self.generation,
            operation_id: self.next_operation_id,
            sequence: self.next_sequence,
        };
        self.next_operation_id = next_operation_id;
        self.next_sequence = next_sequence;
        self.operations.push_back(DriveOperation {
            key,
            operation,
            admitted_at: Instant::now(),
            next_offset: 0,
            completed_bytes: 0,
            queue_latency_us: 0,
            host_latency_us: 0,
            progress: OperationProgress::Pending,
            internal: false,
        });
        Ok(key)
    }

    pub fn stop_admission(&mut self) {
        self.accepting = false;
    }

    pub fn discard_unsubmitted(&mut self) -> usize {
        let before = self.operations.len();
        self.operations
            .retain(|operation| matches!(operation.progress, OperationProgress::InFlight(_)));
        before.saturating_sub(self.operations.len())
    }

    pub fn schedule_one(
        &mut self,
        memory: &GuestMemory,
    ) -> Result<BlockAsyncScheduleOutcome, BlockAsyncScheduleError> {
        let Some(index) = self.ready_operation_index() else {
            return Ok(BlockAsyncScheduleOutcome::Idle);
        };
        let Some(operation) = self.operations.get(index) else {
            return Err(BlockAsyncScheduleError::InvariantViolation);
        };
        let key = operation.key;
        let kind = operation.operation.kind();
        let chunk_offset = operation.next_offset;
        let remaining = operation.operation.len().saturating_sub(chunk_offset);
        let chunk_len = if kind == BlockAsyncOperationKind::Flush {
            0
        } else {
            remaining.min(
                u32::try_from(self.config.chunk_size())
                    .map_err(|_| BlockAsyncScheduleError::InvariantViolation)?,
            )
        };
        let host_offset = operation
            .operation
            .host_offset()
            .checked_add(u64::from(chunk_offset))
            .ok_or(BlockAsyncScheduleError::InvariantViolation)?;
        let guest_address = operation.operation.guest_address();
        let permit = match self.executor.reserve(chunk_len as usize) {
            Ok(permit) => permit,
            Err(BlockAsyncReserveError::Pressure(pressure)) => {
                return Ok(BlockAsyncScheduleOutcome::Pressure(pressure));
            }
            Err(BlockAsyncReserveError::Stopped) => {
                return Err(BlockAsyncScheduleError::ExecutorStopped);
            }
        };

        let payload = match kind {
            BlockAsyncOperationKind::Read | BlockAsyncOperationKind::Write => {
                let mut bytes = Vec::new();
                if bytes.try_reserve_exact(chunk_len as usize).is_err() {
                    drop(permit);
                    return self
                        .finish_without_host(index, BlockAsyncOperationFailure::BufferAllocation);
                }
                bytes.resize(chunk_len as usize, 0);
                if kind == BlockAsyncOperationKind::Write {
                    let Some(address) = guest_address
                        .and_then(|address| address.checked_add(u64::from(chunk_offset)))
                    else {
                        drop(permit);
                        return Err(BlockAsyncScheduleError::InvariantViolation);
                    };
                    if memory.read_slice(&mut bytes, address).is_err() {
                        drop(permit);
                        return self.finish_without_host(
                            index,
                            BlockAsyncOperationFailure::GuestMemoryAccess,
                        );
                    }
                    PreparedPayload::Write(bytes)
                } else {
                    PreparedPayload::Read(bytes)
                }
            }
            BlockAsyncOperationKind::Flush => PreparedPayload::Flush,
        };
        let chunk_key = BlockAsyncChunkKey {
            operation: key,
            offset: chunk_offset,
            len: chunk_len,
            host_offset,
        };
        match permit.submit(
            chunk_key,
            Arc::clone(&self.backing),
            payload,
            Instant::now(),
        ) {
            Ok(()) => {
                let Some(operation) = self.operations.get_mut(index) else {
                    return Err(BlockAsyncScheduleError::InvariantViolation);
                };
                operation.progress = OperationProgress::InFlight(chunk_key);
                Ok(BlockAsyncScheduleOutcome::Submitted {
                    operation: key,
                    chunk_offset,
                    chunk_len,
                })
            }
            Err(BlockAsyncSubmitError::Full) => Ok(BlockAsyncScheduleOutcome::Pressure(
                BlockAsyncPressure::TaskQueueFull,
            )),
            Err(BlockAsyncSubmitError::Stopped) => Err(BlockAsyncScheduleError::ExecutorStopped),
            Err(BlockAsyncSubmitError::Disconnected) => {
                Err(BlockAsyncScheduleError::TaskQueueDisconnected)
            }
        }
    }

    fn ready_operation_index(&self) -> Option<usize> {
        self.operations
            .iter()
            .enumerate()
            .find_map(|(index, operation)| {
                if operation.progress != OperationProgress::Pending {
                    return None;
                }
                let blocked = self
                    .operations
                    .iter()
                    .take(index)
                    .any(|earlier| operations_conflict(earlier.operation, operation.operation));
                (!blocked).then_some(index)
            })
    }

    fn finish_without_host(
        &mut self,
        index: usize,
        failure: BlockAsyncOperationFailure,
    ) -> Result<BlockAsyncScheduleOutcome, BlockAsyncScheduleError> {
        let Some(operation) = self.operations.remove(index) else {
            return Err(BlockAsyncScheduleError::InvariantViolation);
        };
        let bytes_transferred = operation.completed_bytes;
        let dirty_bytes = dirty_bytes_for(operation.operation.kind(), bytes_transferred);
        Ok(BlockAsyncScheduleOutcome::Completed(
            final_operation_completion(
                operation,
                BlockAsyncOperationStatus::Failed(failure),
                bytes_transferred,
                dirty_bytes,
                Instant::now(),
            ),
        ))
    }

    fn enqueue_internal_flush(&mut self) -> Result<(), BlockAsyncAdmissionError> {
        if !self.operations.is_empty() {
            return Err(BlockAsyncAdmissionError::DriveFull);
        }
        let next_operation_id = self
            .next_operation_id
            .checked_add(1)
            .ok_or(BlockAsyncAdmissionError::OperationIdExhausted)?;
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(BlockAsyncAdmissionError::SequenceExhausted)?;
        self.operations
            .try_reserve(1)
            .map_err(|_| BlockAsyncAdmissionError::MetadataAllocation)?;
        let key = BlockAsyncOperationKey {
            generation: self.generation,
            operation_id: self.next_operation_id,
            sequence: self.next_sequence,
        };
        self.next_operation_id = next_operation_id;
        self.next_sequence = next_sequence;
        self.operations.push_back(DriveOperation {
            key,
            operation: BlockAsyncOperation::internal_flush(),
            admitted_at: Instant::now(),
            next_offset: 0,
            completed_bytes: 0,
            queue_latency_us: 0,
            host_latency_us: 0,
            progress: OperationProgress::Pending,
            internal: true,
        });
        Ok(())
    }
}

fn operations_conflict(earlier: BlockAsyncOperation, later: BlockAsyncOperation) -> bool {
    if earlier.kind() == BlockAsyncOperationKind::Flush
        || later.kind() == BlockAsyncOperationKind::Flush
    {
        return true;
    }
    if earlier.kind() == BlockAsyncOperationKind::Read
        && later.kind() == BlockAsyncOperationKind::Read
    {
        return false;
    }
    let earlier_end = earlier.host_offset() + u64::from(earlier.len());
    let later_end = later.host_offset() + u64::from(later.len());
    earlier.host_offset() < later_end && later.host_offset() < earlier_end
}

fn final_operation_completion(
    operation: DriveOperation,
    status: BlockAsyncOperationStatus,
    bytes_transferred: u64,
    dirty_bytes: u64,
    completed_at: Instant,
) -> BlockAsyncOperationCompletion {
    let dirty_range = operation
        .operation
        .guest_address()
        .filter(|_| operation.operation.kind() == BlockAsyncOperationKind::Read)
        .and_then(|address| GuestMemoryRange::new(address, dirty_bytes).ok());
    BlockAsyncOperationCompletion {
        key: operation.key,
        identity: operation.operation.identity(),
        kind: operation.operation.kind(),
        status,
        bytes_transferred,
        dirty_range,
        queue_latency_us: operation.queue_latency_us,
        host_latency_us: operation.host_latency_us,
        total_latency_us: elapsed_us(operation.admitted_at, completed_at),
        internal: operation.internal,
    }
}

enum OwnerCompletionDecision {
    Advance {
        completed_bytes: u64,
    },
    Finish {
        status: BlockAsyncOperationStatus,
        bytes_transferred: u64,
        dirty_bytes: u64,
    },
}

impl BlockAsyncDrive {
    pub fn apply_completion(
        &mut self,
        memory: &mut GuestMemory,
        completion: BlockAsyncHostCompletion,
        disposition: BlockAsyncCompletionDisposition,
    ) -> Result<BlockAsyncApplyOutcome, BlockAsyncApplyError> {
        let BlockAsyncHostCompletion {
            key: chunk_key,
            result,
            payload,
            queue_latency_us,
            host_latency_us,
            _task_lease,
        } = completion;
        let operation_key = chunk_key.operation;
        if operation_key.generation() != self.generation {
            return Ok(BlockAsyncApplyOutcome::Stale(operation_key));
        }
        let Some(index) = self
            .operations
            .iter()
            .position(|operation| operation.key == operation_key)
        else {
            return Err(BlockAsyncApplyError::UnknownOperation);
        };
        let Some(operation) = self.operations.get(index) else {
            return Err(BlockAsyncApplyError::UnknownOperation);
        };
        if operation.progress != OperationProgress::InFlight(chunk_key) {
            return Err(BlockAsyncApplyError::UnexpectedChunk);
        }
        if disposition == BlockAsyncCompletionDisposition::Discard {
            let Some(operation) = self.operations.remove(index) else {
                return Err(BlockAsyncApplyError::UnknownOperation);
            };
            return Ok(BlockAsyncApplyOutcome::Discarded(operation.key));
        }

        let request = operation.operation;
        let prior_bytes = operation.completed_bytes;
        let expected =
            usize::try_from(chunk_key.len).map_err(|_| BlockAsyncApplyError::UnexpectedChunk)?;
        let decision = match (request.kind(), result, payload) {
            (_, HostCompletionResult::Panicked, _) => OwnerCompletionDecision::Finish {
                status: BlockAsyncOperationStatus::Failed(
                    BlockAsyncOperationFailure::WorkerPanicked,
                ),
                bytes_transferred: prior_bytes,
                dirty_bytes: dirty_bytes_for(request.kind(), prior_bytes),
            },
            (
                BlockAsyncOperationKind::Read,
                HostCompletionResult::Transfer(transfer),
                HostTaskPayload::Read(buffer),
            ) => evaluate_read_completion(
                request,
                chunk_key,
                prior_bytes,
                expected,
                transfer,
                &buffer.bytes,
                memory,
            ),
            (
                BlockAsyncOperationKind::Write,
                HostCompletionResult::Transfer(transfer),
                HostTaskPayload::Write(buffer),
            ) => evaluate_write_completion(
                request,
                chunk_key,
                prior_bytes,
                expected,
                transfer,
                buffer.bytes.len(),
            ),
            (
                BlockAsyncOperationKind::Flush,
                HostCompletionResult::Flush(result),
                HostTaskPayload::Flush,
            ) => match result {
                Ok(()) => OwnerCompletionDecision::Finish {
                    status: BlockAsyncOperationStatus::Success,
                    bytes_transferred: 0,
                    dirty_bytes: 0,
                },
                Err(source) => OwnerCompletionDecision::Finish {
                    status: BlockAsyncOperationStatus::Failed(BlockAsyncOperationFailure::Host {
                        operation: BlockAsyncOperationKind::Flush,
                        bytes_transferred: 0,
                        source,
                    }),
                    bytes_transferred: 0,
                    dirty_bytes: 0,
                },
            },
            _ => OwnerCompletionDecision::Finish {
                status: BlockAsyncOperationStatus::Failed(
                    BlockAsyncOperationFailure::InvalidHostCompletion,
                ),
                bytes_transferred: prior_bytes,
                dirty_bytes: dirty_bytes_for(request.kind(), prior_bytes),
            },
        };

        match decision {
            OwnerCompletionDecision::Advance { completed_bytes } => {
                let Some(operation) = self.operations.get_mut(index) else {
                    return Err(BlockAsyncApplyError::UnknownOperation);
                };
                let Some(next_offset) = chunk_key.offset.checked_add(chunk_key.len) else {
                    return Err(BlockAsyncApplyError::UnexpectedChunk);
                };
                operation.completed_bytes = completed_bytes;
                operation.next_offset = next_offset;
                operation.queue_latency_us =
                    operation.queue_latency_us.saturating_add(queue_latency_us);
                operation.host_latency_us =
                    operation.host_latency_us.saturating_add(host_latency_us);
                operation.progress = OperationProgress::Pending;
                Ok(BlockAsyncApplyOutcome::Advanced(operation_key))
            }
            OwnerCompletionDecision::Finish {
                status,
                bytes_transferred,
                dirty_bytes,
            } => {
                let Some(mut operation) = self.operations.remove(index) else {
                    return Err(BlockAsyncApplyError::UnknownOperation);
                };
                operation.queue_latency_us =
                    operation.queue_latency_us.saturating_add(queue_latency_us);
                operation.host_latency_us =
                    operation.host_latency_us.saturating_add(host_latency_us);
                Ok(BlockAsyncApplyOutcome::Completed(
                    final_operation_completion(
                        operation,
                        status,
                        bytes_transferred,
                        dirty_bytes,
                        Instant::now(),
                    ),
                ))
            }
        }
    }

    fn completion_is_internal(&self, completion: &BlockAsyncHostCompletion) -> bool {
        self.operations
            .iter()
            .any(|operation| operation.key == completion.operation_key() && operation.internal)
    }
}

fn evaluate_read_completion(
    operation: BlockAsyncOperation,
    chunk: BlockAsyncChunkKey,
    prior_bytes: u64,
    expected: usize,
    transfer: BlockAsyncTransferResult,
    buffer: &[u8],
    memory: &mut GuestMemory,
) -> OwnerCompletionDecision {
    let actual = transfer.bytes_transferred();
    if actual > expected || buffer.len() != expected {
        return invalid_host_completion(operation.kind(), prior_bytes);
    }
    let Ok(actual_u64) = u64::try_from(actual) else {
        return invalid_host_completion(operation.kind(), prior_bytes);
    };
    let total = prior_bytes.saturating_add(actual_u64);
    if actual != 0 {
        let Some(source) = buffer.get(..actual) else {
            return invalid_host_completion(operation.kind(), prior_bytes);
        };
        let Some(address) = operation
            .guest_address()
            .and_then(|address| address.checked_add(u64::from(chunk.offset)))
        else {
            return invalid_host_completion(operation.kind(), prior_bytes);
        };
        if memory.write_slice(source, address).is_err() {
            return OwnerCompletionDecision::Finish {
                status: BlockAsyncOperationStatus::Failed(
                    BlockAsyncOperationFailure::GuestMemoryAccess,
                ),
                bytes_transferred: total,
                dirty_bytes: prior_bytes,
            };
        }
    }
    transfer_decision(
        operation,
        chunk,
        prior_bytes,
        expected,
        transfer,
        total,
        total,
    )
}

fn evaluate_write_completion(
    operation: BlockAsyncOperation,
    chunk: BlockAsyncChunkKey,
    prior_bytes: u64,
    expected: usize,
    transfer: BlockAsyncTransferResult,
    buffer_len: usize,
) -> OwnerCompletionDecision {
    let actual = transfer.bytes_transferred();
    if actual > expected || buffer_len != expected {
        return invalid_host_completion(operation.kind(), prior_bytes);
    }
    let Ok(actual_u64) = u64::try_from(actual) else {
        return invalid_host_completion(operation.kind(), prior_bytes);
    };
    let total = prior_bytes.saturating_add(actual_u64);
    transfer_decision(operation, chunk, prior_bytes, expected, transfer, total, 0)
}

fn transfer_decision(
    operation: BlockAsyncOperation,
    chunk: BlockAsyncChunkKey,
    prior_bytes: u64,
    expected: usize,
    transfer: BlockAsyncTransferResult,
    total: u64,
    dirty_bytes: u64,
) -> OwnerCompletionDecision {
    let actual = transfer.bytes_transferred();
    if let Some(source) = transfer.error() {
        return OwnerCompletionDecision::Finish {
            status: BlockAsyncOperationStatus::Failed(BlockAsyncOperationFailure::Host {
                operation: operation.kind(),
                bytes_transferred: actual,
                source,
            }),
            bytes_transferred: total,
            dirty_bytes,
        };
    }
    if actual != expected {
        return OwnerCompletionDecision::Finish {
            status: BlockAsyncOperationStatus::Failed(BlockAsyncOperationFailure::ShortIo {
                operation: operation.kind(),
                expected,
                actual,
            }),
            bytes_transferred: total,
            dirty_bytes,
        };
    }
    let Some(next_offset) = chunk.offset.checked_add(chunk.len) else {
        return invalid_host_completion(operation.kind(), prior_bytes);
    };
    if next_offset < operation.len() {
        OwnerCompletionDecision::Advance {
            completed_bytes: total,
        }
    } else if next_offset == operation.len() {
        OwnerCompletionDecision::Finish {
            status: BlockAsyncOperationStatus::Success,
            bytes_transferred: total,
            dirty_bytes,
        }
    } else {
        invalid_host_completion(operation.kind(), prior_bytes)
    }
}

fn invalid_host_completion(
    operation: BlockAsyncOperationKind,
    prior_bytes: u64,
) -> OwnerCompletionDecision {
    OwnerCompletionDecision::Finish {
        status: BlockAsyncOperationStatus::Failed(
            BlockAsyncOperationFailure::InvalidHostCompletion,
        ),
        bytes_transferred: prior_bytes,
        dirty_bytes: dirty_bytes_for(operation, prior_bytes),
    }
}

const fn dirty_bytes_for(operation: BlockAsyncOperationKind, bytes: u64) -> u64 {
    if matches!(operation, BlockAsyncOperationKind::Read) {
        bytes
    } else {
        0
    }
}

/// Failed final persistence barrier for one writeback drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockAsyncPersistenceFailure {
    generation: BlockAsyncDriveGeneration,
    source: BlockAsyncOperationFailure,
}

impl BlockAsyncPersistenceFailure {
    pub const fn generation(self) -> BlockAsyncDriveGeneration {
        self.generation
    }

    pub const fn source(self) -> BlockAsyncOperationFailure {
        self.source
    }
}

/// Deterministic result of draining a set of process-owned drives.
#[derive(Debug, PartialEq, Eq)]
pub struct BlockAsyncDrainOutcome {
    completions: Vec<BlockAsyncOperationCompletion>,
    persistence_failures: Vec<BlockAsyncPersistenceFailure>,
}

impl BlockAsyncDrainOutcome {
    pub fn completions(&self) -> &[BlockAsyncOperationCompletion] {
        &self.completions
    }

    pub fn persistence_failures(&self) -> &[BlockAsyncPersistenceFailure] {
        &self.persistence_failures
    }

    pub fn into_parts(
        self,
    ) -> (
        Vec<BlockAsyncOperationCompletion>,
        Vec<BlockAsyncPersistenceFailure>,
    ) {
        (self.completions, self.persistence_failures)
    }
}

/// Structural failure while stopping and draining drive generations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAsyncDrainError {
    DuplicateGeneration,
    MetadataAllocation,
    FinalFlushAdmission(BlockAsyncAdmissionError),
    Schedule(BlockAsyncScheduleError),
    Apply(BlockAsyncApplyError),
    Executor(BlockAsyncExecutorError),
}

impl fmt::Display for BlockAsyncDrainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateGeneration => {
                formatter.write_str("asynchronous block drain has duplicate generations")
            }
            Self::MetadataAllocation => {
                formatter.write_str("asynchronous block drain metadata allocation failed")
            }
            Self::FinalFlushAdmission(_) => {
                formatter.write_str("asynchronous block final flush admission failed")
            }
            Self::Schedule(_) => formatter.write_str("asynchronous block drain scheduling failed"),
            Self::Apply(_) => formatter.write_str("asynchronous block completion apply failed"),
            Self::Executor(_) => formatter.write_str("asynchronous block executor drain failed"),
        }
    }
}

impl std::error::Error for BlockAsyncDrainError {}

/// Stop, finish, and either apply or discard all work owned by the drives.
pub fn drain_block_async_drives(
    executor: &BlockAsyncExecutor,
    drives: &mut [BlockAsyncDrive],
    memory: &mut GuestMemory,
    disposition: BlockAsyncCompletionDisposition,
) -> Result<BlockAsyncDrainOutcome, BlockAsyncDrainError> {
    for (index, drive) in drives.iter().enumerate() {
        if drives
            .iter()
            .take(index)
            .any(|earlier| earlier.generation() == drive.generation())
        {
            return Err(BlockAsyncDrainError::DuplicateGeneration);
        }
    }

    let mut final_flushes = Vec::new();
    final_flushes
        .try_reserve_exact(drives.len())
        .map_err(|_| BlockAsyncDrainError::MetadataAllocation)?;
    let operation_count = drives.iter().try_fold(0usize, |total, drive| {
        total.checked_add(drive.operation_count())
    });
    let Some(operation_count) = operation_count else {
        return Err(BlockAsyncDrainError::MetadataAllocation);
    };
    let persistence_count = drives
        .iter()
        .filter(|drive| drive.cache_type() == DriveCacheType::Writeback)
        .count();
    let mut completions = Vec::new();
    completions
        .try_reserve_exact(operation_count)
        .map_err(|_| BlockAsyncDrainError::MetadataAllocation)?;
    let mut persistence_failures = Vec::new();
    persistence_failures
        .try_reserve_exact(persistence_count)
        .map_err(|_| BlockAsyncDrainError::MetadataAllocation)?;
    for drive in drives.iter_mut() {
        drive.stop_admission();
        if disposition == BlockAsyncCompletionDisposition::Discard {
            drive.discard_unsubmitted();
        }
        final_flushes.push(drive.cache_type() == DriveCacheType::Writeback);
    }

    loop {
        let mut made_progress = false;
        for (drive, needs_final_flush) in drives.iter_mut().zip(final_flushes.iter_mut()) {
            if *needs_final_flush && drive.is_drained() {
                drive
                    .enqueue_internal_flush()
                    .map_err(BlockAsyncDrainError::FinalFlushAdmission)?;
                *needs_final_flush = false;
                made_progress = true;
            }
            match drive
                .schedule_one(memory)
                .map_err(BlockAsyncDrainError::Schedule)?
            {
                BlockAsyncScheduleOutcome::Idle | BlockAsyncScheduleOutcome::Pressure(_) => {}
                BlockAsyncScheduleOutcome::Submitted { .. } => made_progress = true,
                BlockAsyncScheduleOutcome::Completed(completion) => {
                    record_drain_completion(
                        completion,
                        &mut completions,
                        &mut persistence_failures,
                    );
                    made_progress = true;
                }
            }
        }

        while let Some(completion) = executor
            .try_recv_completion()
            .map_err(BlockAsyncDrainError::Executor)?
        {
            route_drain_completion(
                drives,
                memory,
                completion,
                disposition,
                &mut completions,
                &mut persistence_failures,
            )?;
            made_progress = true;
        }

        let drained = final_flushes.iter().all(|pending| !pending)
            && drives.iter().all(BlockAsyncDrive::is_drained);
        if drained {
            break;
        }
        if !made_progress {
            let completion = executor
                .recv_completion()
                .map_err(BlockAsyncDrainError::Executor)?;
            route_drain_completion(
                drives,
                memory,
                completion,
                disposition,
                &mut completions,
                &mut persistence_failures,
            )?;
        }
    }

    executor
        .drain_notification()
        .map_err(BlockAsyncDrainError::Executor)?;
    completions.sort_unstable_by_key(|completion| {
        (completion.key().generation(), completion.key().sequence())
    });
    persistence_failures.sort_unstable_by_key(|failure| failure.generation());
    Ok(BlockAsyncDrainOutcome {
        completions,
        persistence_failures,
    })
}

fn route_drain_completion(
    drives: &mut [BlockAsyncDrive],
    memory: &mut GuestMemory,
    completion: BlockAsyncHostCompletion,
    disposition: BlockAsyncCompletionDisposition,
    completions: &mut Vec<BlockAsyncOperationCompletion>,
    persistence_failures: &mut Vec<BlockAsyncPersistenceFailure>,
) -> Result<(), BlockAsyncDrainError> {
    let generation = completion.operation_key().generation();
    let Some(drive) = drives
        .iter_mut()
        .find(|drive| drive.generation() == generation)
    else {
        return Ok(());
    };
    let effective_disposition = if drive.completion_is_internal(&completion) {
        BlockAsyncCompletionDisposition::Apply
    } else {
        disposition
    };
    match drive
        .apply_completion(memory, completion, effective_disposition)
        .map_err(BlockAsyncDrainError::Apply)?
    {
        BlockAsyncApplyOutcome::Completed(completion) => {
            record_drain_completion(completion, completions, persistence_failures);
        }
        BlockAsyncApplyOutcome::Advanced(_)
        | BlockAsyncApplyOutcome::Discarded(_)
        | BlockAsyncApplyOutcome::Stale(_) => {}
    }
    Ok(())
}

fn record_drain_completion(
    completion: BlockAsyncOperationCompletion,
    completions: &mut Vec<BlockAsyncOperationCompletion>,
    persistence_failures: &mut Vec<BlockAsyncPersistenceFailure>,
) {
    if completion.internal {
        if let BlockAsyncOperationStatus::Failed(source) = completion.status() {
            persistence_failures.push(BlockAsyncPersistenceFailure {
                generation: completion.key().generation(),
                source,
            });
        }
    } else {
        completions.push(completion);
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::mpsc::{Receiver, Sender};
    use std::time::Duration;

    use super::*;
    use crate::memory::{GuestMemoryLayout, GuestMemoryRange};

    static NEXT_TEMPORARY_FILE: AtomicUsize = AtomicUsize::new(0);

    #[derive(Debug)]
    struct TemporaryFile {
        path: PathBuf,
    }

    impl Drop for TemporaryFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn temporary_backing(initial: &[u8]) -> (TemporaryFile, Arc<BlockFileBacking>) {
        temporary_backing_with_len(initial, 64 * 1024)
    }

    fn temporary_backing_with_len(
        initial: &[u8],
        len: u64,
    ) -> (TemporaryFile, Arc<BlockFileBacking>) {
        let suffix = NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bangbang-async-block-{}-{suffix}",
            std::process::id()
        ));
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .expect("temporary backing should open");
        file.set_len(len).expect("temporary backing should size");
        if !initial.is_empty() {
            file.write_all_at(initial, 0)
                .expect("temporary backing should initialize");
        }
        let backing = BlockFileBacking::from_file(file, false)
            .map(Arc::new)
            .expect("temporary backing should validate");
        (TemporaryFile { path }, backing)
    }

    fn guest_memory() -> GuestMemory {
        guest_memory_with_size(64 * 1024)
    }

    fn guest_memory_with_size(size: u64) -> GuestMemory {
        let range =
            GuestMemoryRange::new(GuestAddress::new(0), size).expect("test range should validate");
        let layout = GuestMemoryLayout::new(vec![range]).expect("test layout should validate");
        GuestMemory::allocate(&layout).expect("test guest memory should allocate")
    }

    const fn identity(head: u16) -> BlockAsyncRequestIdentity {
        BlockAsyncRequestIdentity::new(0, head, GuestAddress::new(0x8000))
    }

    const fn test_config(
        workers: usize,
        tasks: usize,
        per_drive: usize,
        chunk: usize,
        bytes: usize,
    ) -> BlockAsyncExecutorConfig {
        BlockAsyncExecutorConfig::new(workers, tasks, per_drive, chunk, bytes)
    }

    fn wait_for_completion(executor: &BlockAsyncExecutor) -> BlockAsyncHostCompletion {
        executor
            .completions
            .recv_timeout(Duration::from_secs(2))
            .expect("host completion should arrive before the bounded deadline")
    }

    #[derive(Debug, Default)]
    struct ImmediateHost;

    impl BlockAsyncHostIo for ImmediateHost {
        fn read_at(
            &self,
            _backing: &BlockFileBacking,
            offset: u64,
            destination: &mut [u8],
        ) -> BlockAsyncTransferResult {
            destination.fill(u8::try_from(offset & 0xff).expect("masked offset should fit"));
            BlockAsyncTransferResult::complete(destination.len())
        }

        fn write_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            source: &[u8],
        ) -> BlockAsyncTransferResult {
            BlockAsyncTransferResult::complete(source.len())
        }

        fn flush(&self, _backing: &BlockFileBacking) -> Result<(), io::ErrorKind> {
            Ok(())
        }
    }

    #[test]
    fn production_limits_and_injected_bounds_are_fixed() {
        let production = BlockAsyncExecutorConfig::default();
        assert_eq!(production.worker_count(), BLOCK_ASYNC_WORKER_COUNT);
        assert_eq!(
            production.global_task_limit(),
            BLOCK_ASYNC_GLOBAL_TASK_LIMIT
        );
        assert_eq!(
            production.per_drive_operation_limit(),
            BLOCK_ASYNC_PER_DRIVE_OPERATION_LIMIT
        );
        assert_eq!(production.chunk_size(), BLOCK_ASYNC_CHUNK_SIZE);
        assert_eq!(production.buffer_budget(), BLOCK_ASYNC_BUFFER_BUDGET);

        let host: Arc<dyn BlockAsyncHostIo> = Arc::new(ImmediateHost);
        for (config, expected) in [
            (
                test_config(0, 1, 1, 1, 1),
                BlockAsyncExecutorBuildError::InvalidWorkerCount,
            ),
            (
                test_config(BLOCK_ASYNC_WORKER_COUNT + 1, 1, 1, 1, 1),
                BlockAsyncExecutorBuildError::InvalidWorkerCount,
            ),
            (
                test_config(1, 0, 1, 1, 1),
                BlockAsyncExecutorBuildError::InvalidTaskLimit,
            ),
            (
                test_config(1, BLOCK_ASYNC_GLOBAL_TASK_LIMIT + 1, 1, 1, 1),
                BlockAsyncExecutorBuildError::InvalidTaskLimit,
            ),
            (
                test_config(1, 1, 0, 1, 1),
                BlockAsyncExecutorBuildError::InvalidPerDriveLimit,
            ),
            (
                test_config(1, 1, 1, 2, 1),
                BlockAsyncExecutorBuildError::InvalidChunkSize,
            ),
            (
                test_config(
                    1,
                    1,
                    1,
                    BLOCK_ASYNC_CHUNK_SIZE + 1,
                    BLOCK_ASYNC_BUFFER_BUDGET,
                ),
                BlockAsyncExecutorBuildError::InvalidChunkSize,
            ),
            (
                test_config(1, 1, 1, 1, 0),
                BlockAsyncExecutorBuildError::InvalidBufferBudget,
            ),
            (
                test_config(1, 1, 1, 1, BLOCK_ASYNC_BUFFER_BUDGET + 1),
                BlockAsyncExecutorBuildError::InvalidBufferBudget,
            ),
        ] {
            let error = BlockAsyncExecutor::with_config_and_host(config, Arc::clone(&host))
                .expect_err("invalid executor configuration should fail");
            assert_eq!(
                std::mem::discriminant(&error),
                std::mem::discriminant(&expected)
            );
        }
    }

    #[test]
    fn multi_chunk_write_exceeding_budget_completes_with_exact_bytes() {
        let (temporary, backing) = temporary_backing(&[]);
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(2, 2, 4, 4, 4),
            Arc::new(SystemBlockAsyncHostIo),
        )
        .expect("executor should start");
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(1),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        let bytes = [1_u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        memory
            .write_slice(&bytes, GuestAddress::new(0x100))
            .expect("guest write should succeed");
        drive
            .admit(
                BlockAsyncOperation::write(identity(1), GuestAddress::new(0x100), 7, 12)
                    .expect("operation should validate"),
            )
            .expect("operation should admit");

        let outcome = drain_block_async_drives(
            &executor,
            std::slice::from_mut(&mut drive),
            &mut memory,
            BlockAsyncCompletionDisposition::Apply,
        )
        .expect("drive should drain");
        assert_eq!(outcome.completions().len(), 1);
        assert_eq!(outcome.completions()[0].bytes_transferred(), 12);
        assert_eq!(
            outcome.completions()[0].status(),
            BlockAsyncOperationStatus::Success
        );
        let contents = fs::read(&temporary.path).expect("backing should read");
        assert_eq!(&contents[7..19], &bytes);
        assert_eq!(executor.outstanding_tasks(), 0);
        assert_eq!(executor.reserved_buffer_bytes(), 0);
        executor.shutdown().expect("executor should stop");
    }

    #[derive(Debug, Default)]
    struct ChunkCountingHost {
        writes: AtomicUsize,
        largest_write: AtomicUsize,
    }

    impl BlockAsyncHostIo for ChunkCountingHost {
        fn read_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            _destination: &mut [u8],
        ) -> BlockAsyncTransferResult {
            BlockAsyncTransferResult::failed(0, io::ErrorKind::Unsupported)
        }

        fn write_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            source: &[u8],
        ) -> BlockAsyncTransferResult {
            self.writes.fetch_add(1, Ordering::AcqRel);
            self.largest_write.fetch_max(source.len(), Ordering::AcqRel);
            BlockAsyncTransferResult::complete(source.len())
        }

        fn flush(&self, _backing: &BlockFileBacking) -> Result<(), io::ErrorKind> {
            Ok(())
        }
    }

    #[test]
    fn request_larger_than_production_buffer_budget_uses_one_mib_chunks() {
        const REQUEST_SIZE: usize = BLOCK_ASYNC_BUFFER_BUDGET + BLOCK_ASYNC_CHUNK_SIZE;
        let (_temporary, backing) = temporary_backing_with_len(
            &[],
            u64::try_from(REQUEST_SIZE).expect("request should fit u64"),
        );
        let host = Arc::new(ChunkCountingHost::default());
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            BlockAsyncExecutorConfig::default(),
            Arc::clone(&host) as Arc<dyn BlockAsyncHostIo>,
        )
        .expect("executor should start");
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(20),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory_with_size(
            u64::try_from(REQUEST_SIZE).expect("request should fit guest memory"),
        );
        drive
            .admit(
                BlockAsyncOperation::write(
                    identity(21),
                    GuestAddress::new(0),
                    0,
                    u32::try_from(REQUEST_SIZE).expect("request should fit operation"),
                )
                .expect("write should validate"),
            )
            .expect("write should admit");
        let outcome = drain_block_async_drives(
            &executor,
            std::slice::from_mut(&mut drive),
            &mut memory,
            BlockAsyncCompletionDisposition::Apply,
        )
        .expect("large write should drain");
        assert_eq!(outcome.completions().len(), 1);
        assert_eq!(
            outcome.completions()[0].bytes_transferred(),
            u64::try_from(REQUEST_SIZE).expect("request should fit u64")
        );
        assert_eq!(host.writes.load(Ordering::Acquire), 17);
        assert_eq!(
            host.largest_write.load(Ordering::Acquire),
            BLOCK_ASYNC_CHUNK_SIZE
        );
        assert_eq!(executor.reserved_buffer_bytes(), 0);
        executor.shutdown().expect("executor should stop");
    }

    #[test]
    fn write_is_snapshotted_and_read_is_staged_until_owner_apply() {
        let initial = [9_u8, 8, 7, 6];
        let (temporary, backing) = temporary_backing(&initial);
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 2, 4, 4, 8),
            Arc::new(SystemBlockAsyncHostIo),
        )
        .expect("executor should start");
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(2),
            Arc::clone(&backing),
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        memory
            .write_slice(&[1, 2, 3, 4], GuestAddress::new(0x100))
            .expect("guest write should succeed");
        drive
            .admit(
                BlockAsyncOperation::write(identity(2), GuestAddress::new(0x100), 16, 4)
                    .expect("write should validate"),
            )
            .expect("write should admit");
        assert!(matches!(
            drive.schedule_one(&memory).expect("write should schedule"),
            BlockAsyncScheduleOutcome::Submitted { .. }
        ));
        memory
            .write_slice(&[5, 5, 5, 5], GuestAddress::new(0x100))
            .expect("guest mutation should succeed");
        let write_completion = wait_for_completion(&executor);
        assert!(matches!(
            drive
                .apply_completion(
                    &mut memory,
                    write_completion,
                    BlockAsyncCompletionDisposition::Apply,
                )
                .expect("write completion should apply"),
            BlockAsyncApplyOutcome::Completed(_)
        ));
        let contents = fs::read(&temporary.path).expect("backing should read");
        assert_eq!(&contents[16..20], &[1, 2, 3, 4]);

        drive
            .admit(
                BlockAsyncOperation::read(identity(3), GuestAddress::new(0x200), 0, 4)
                    .expect("read should validate"),
            )
            .expect("read should admit");
        assert!(matches!(
            drive.schedule_one(&memory).expect("read should schedule"),
            BlockAsyncScheduleOutcome::Submitted { .. }
        ));
        let read_completion = wait_for_completion(&executor);
        let mut before_apply = [0_u8; 4];
        memory
            .read_slice(&mut before_apply, GuestAddress::new(0x200))
            .expect("guest read should succeed");
        assert_eq!(before_apply, [0; 4]);
        let applied = drive
            .apply_completion(
                &mut memory,
                read_completion,
                BlockAsyncCompletionDisposition::Apply,
            )
            .expect("read completion should apply");
        let BlockAsyncApplyOutcome::Completed(applied) = applied else {
            panic!("read should finish");
        };
        let mut after_apply = [0_u8; 4];
        memory
            .read_slice(&mut after_apply, GuestAddress::new(0x200))
            .expect("guest read should succeed");
        assert_eq!(after_apply, initial);
        assert_eq!(
            applied.dirty_range(),
            GuestMemoryRange::new(GuestAddress::new(0x200), 4).ok()
        );
        executor.shutdown().expect("executor should stop");
    }

    #[test]
    fn task_and_buffer_leases_cover_completed_but_unapplied_work() {
        let (_temporary, backing) = temporary_backing(&[]);
        let mut memory = guest_memory();
        memory
            .write_slice(&[1; 8], GuestAddress::new(0x100))
            .expect("guest write should succeed");
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 2, 4, 4),
            Arc::new(ImmediateHost),
        )
        .expect("executor should start");
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(3),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        for (head, host_offset, guest_offset) in [(4, 0, 0x100), (5, 8, 0x104)] {
            drive
                .admit(
                    BlockAsyncOperation::write(
                        identity(head),
                        GuestAddress::new(guest_offset),
                        host_offset,
                        4,
                    )
                    .expect("write should validate"),
                )
                .expect("write should admit");
        }
        assert_eq!(
            drive.admit(BlockAsyncOperation::flush(identity(6))),
            Err(BlockAsyncAdmissionError::DriveFull)
        );
        drive.schedule_one(&memory).expect("first should schedule");
        let first = wait_for_completion(&executor);
        assert_eq!(executor.outstanding_tasks(), 1);
        assert_eq!(executor.reserved_buffer_bytes(), 4);
        assert_eq!(
            drive.schedule_one(&memory).expect("pressure should report"),
            BlockAsyncScheduleOutcome::Pressure(BlockAsyncPressure::GlobalTaskLimit)
        );
        drive
            .apply_completion(&mut memory, first, BlockAsyncCompletionDisposition::Apply)
            .expect("first should apply");
        assert_eq!(executor.outstanding_tasks(), 0);
        assert_eq!(executor.reserved_buffer_bytes(), 0);
        drive.schedule_one(&memory).expect("second should schedule");
        let second = wait_for_completion(&executor);
        drive
            .apply_completion(&mut memory, second, BlockAsyncCompletionDisposition::Apply)
            .expect("second should apply");
        executor.shutdown().expect("executor should stop");

        let (_temporary, backing) = temporary_backing(&[]);
        let mut byte_executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 2, 2, 4, 4),
            Arc::new(ImmediateHost),
        )
        .expect("executor should start");
        let mut byte_drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(4),
            backing,
            DriveCacheType::Unsafe,
            byte_executor.handle(),
        )
        .expect("drive should bind");
        for (head, host_offset, guest_offset) in [(7, 0, 0x100), (8, 8, 0x104)] {
            byte_drive
                .admit(
                    BlockAsyncOperation::write(
                        identity(head),
                        GuestAddress::new(guest_offset),
                        host_offset,
                        4,
                    )
                    .expect("write should validate"),
                )
                .expect("write should admit");
        }
        byte_drive
            .schedule_one(&memory)
            .expect("first should schedule");
        assert_eq!(
            byte_drive
                .schedule_one(&memory)
                .expect("pressure should report"),
            BlockAsyncScheduleOutcome::Pressure(BlockAsyncPressure::BufferBudget)
        );
        let first = wait_for_completion(&byte_executor);
        byte_drive
            .apply_completion(&mut memory, first, BlockAsyncCompletionDisposition::Apply)
            .expect("first should apply");
        let outcome = drain_block_async_drives(
            &byte_executor,
            std::slice::from_mut(&mut byte_drive),
            &mut memory,
            BlockAsyncCompletionDisposition::Apply,
        )
        .expect("remaining work should drain");
        assert_eq!(outcome.completions().len(), 1);
        byte_executor.shutdown().expect("executor should stop");
    }

    #[derive(Debug)]
    struct GateHost {
        blocked_offset: u64,
        entered: Sender<u64>,
        release: Mutex<Receiver<()>>,
    }

    impl BlockAsyncHostIo for GateHost {
        fn read_at(
            &self,
            _backing: &BlockFileBacking,
            offset: u64,
            destination: &mut [u8],
        ) -> BlockAsyncTransferResult {
            self.entered
                .send(offset)
                .expect("observer should remain live");
            destination.fill(7);
            BlockAsyncTransferResult::complete(destination.len())
        }

        fn write_at(
            &self,
            _backing: &BlockFileBacking,
            offset: u64,
            source: &[u8],
        ) -> BlockAsyncTransferResult {
            self.entered
                .send(offset)
                .expect("observer should remain live");
            if offset == self.blocked_offset
                && self
                    .release
                    .lock()
                    .expect("release gate should lock")
                    .recv_timeout(Duration::from_secs(2))
                    .is_err()
            {
                return BlockAsyncTransferResult::failed(0, io::ErrorKind::TimedOut);
            }
            BlockAsyncTransferResult::complete(source.len())
        }

        fn flush(&self, _backing: &BlockFileBacking) -> Result<(), io::ErrorKind> {
            Ok(())
        }
    }

    #[test]
    fn non_overlapping_work_overtakes_but_overlap_remains_serialized() {
        let (_temporary, backing) = temporary_backing(&[]);
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let host = GateHost {
            blocked_offset: 0,
            entered: entered_sender,
            release: Mutex::new(release_receiver),
        };
        let mut executor =
            BlockAsyncExecutor::with_config_and_host(test_config(2, 3, 4, 4, 12), Arc::new(host))
                .expect("executor should start");
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(5),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        memory
            .write_slice(&[1; 4], GuestAddress::new(0x100))
            .expect("guest write should succeed");
        let first = drive
            .admit(
                BlockAsyncOperation::write(identity(9), GuestAddress::new(0x100), 0, 4)
                    .expect("write should validate"),
            )
            .expect("first should admit");
        let overlapping = drive
            .admit(
                BlockAsyncOperation::read(identity(10), GuestAddress::new(0x200), 2, 4)
                    .expect("read should validate"),
            )
            .expect("overlap should admit");
        let independent = drive
            .admit(
                BlockAsyncOperation::read(identity(11), GuestAddress::new(0x300), 16, 4)
                    .expect("read should validate"),
            )
            .expect("independent read should admit");

        drive.schedule_one(&memory).expect("first should schedule");
        assert_eq!(
            entered_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("blocked call should enter"),
            0
        );
        let submitted = drive
            .schedule_one(&memory)
            .expect("independent work should schedule");
        assert!(matches!(
            submitted,
            BlockAsyncScheduleOutcome::Submitted { operation, .. } if operation == independent
        ));
        assert_eq!(
            entered_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("independent call should enter"),
            16
        );
        let independent_completion = wait_for_completion(&executor);
        assert_eq!(independent_completion.operation_key(), independent);
        drive
            .apply_completion(
                &mut memory,
                independent_completion,
                BlockAsyncCompletionDisposition::Apply,
            )
            .expect("independent read should apply");
        assert_eq!(
            drive.schedule_one(&memory).expect("overlap stays blocked"),
            BlockAsyncScheduleOutcome::Idle
        );

        release_sender
            .send(())
            .expect("blocked write should release");
        let first_completion = wait_for_completion(&executor);
        assert_eq!(first_completion.operation_key(), first);
        drive
            .apply_completion(
                &mut memory,
                first_completion,
                BlockAsyncCompletionDisposition::Apply,
            )
            .expect("first write should apply");
        assert!(matches!(
            drive.schedule_one(&memory).expect("overlap should schedule"),
            BlockAsyncScheduleOutcome::Submitted { operation, .. } if operation == overlapping
        ));
        let overlap_completion = wait_for_completion(&executor);
        drive
            .apply_completion(
                &mut memory,
                overlap_completion,
                BlockAsyncCompletionDisposition::Apply,
            )
            .expect("overlap should apply");
        executor.shutdown().expect("executor should stop");
    }

    #[test]
    fn flush_is_an_exclusive_before_and_after_barrier() {
        let (_temporary, backing) = temporary_backing(&[]);
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(2, 3, 4, 4, 12),
            Arc::new(ImmediateHost),
        )
        .expect("executor should start");
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(6),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        memory
            .write_slice(&[1; 4], GuestAddress::new(0x100))
            .expect("guest write should succeed");
        let write = drive
            .admit(
                BlockAsyncOperation::write(identity(12), GuestAddress::new(0x100), 0, 4)
                    .expect("write should validate"),
            )
            .expect("write should admit");
        let flush = drive
            .admit(BlockAsyncOperation::flush(identity(13)))
            .expect("flush should admit");
        let read = drive
            .admit(
                BlockAsyncOperation::read(identity(14), GuestAddress::new(0x200), 16, 4)
                    .expect("read should validate"),
            )
            .expect("read should admit");
        assert!(matches!(
            drive.schedule_one(&memory).expect("write should schedule"),
            BlockAsyncScheduleOutcome::Submitted { operation, .. } if operation == write
        ));
        assert_eq!(
            drive.schedule_one(&memory).expect("flush should wait"),
            BlockAsyncScheduleOutcome::Idle
        );
        let completion = wait_for_completion(&executor);
        drive
            .apply_completion(
                &mut memory,
                completion,
                BlockAsyncCompletionDisposition::Apply,
            )
            .expect("write should apply");
        assert!(matches!(
            drive.schedule_one(&memory).expect("flush should schedule"),
            BlockAsyncScheduleOutcome::Submitted { operation, .. } if operation == flush
        ));
        assert_eq!(
            drive.schedule_one(&memory).expect("read should wait"),
            BlockAsyncScheduleOutcome::Idle
        );
        let completion = wait_for_completion(&executor);
        drive
            .apply_completion(
                &mut memory,
                completion,
                BlockAsyncCompletionDisposition::Apply,
            )
            .expect("flush should apply");
        assert!(matches!(
            drive.schedule_one(&memory).expect("read should schedule"),
            BlockAsyncScheduleOutcome::Submitted { operation, .. } if operation == read
        ));
        let completion = wait_for_completion(&executor);
        drive
            .apply_completion(
                &mut memory,
                completion,
                BlockAsyncCompletionDisposition::Apply,
            )
            .expect("read should apply");
        executor.shutdown().expect("executor should stop");
    }

    #[test]
    fn separate_drives_share_workers_without_cross_drive_serialization() {
        let (_temporary_a, backing_a) = temporary_backing(&[]);
        let (_temporary_b, backing_b) = temporary_backing(&[]);
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(2, 2, 2, 4, 8),
            Arc::new(GateHost {
                blocked_offset: 0,
                entered: entered_sender,
                release: Mutex::new(release_receiver),
            }),
        )
        .expect("executor should start");
        let mut drive_a = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(60),
            backing_a,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("first drive should bind");
        let mut drive_b = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(61),
            backing_b,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("second drive should bind");
        let mut memory = guest_memory();
        memory
            .write_slice(&[1; 4], GuestAddress::new(0x100))
            .expect("guest write should succeed");
        let blocked = drive_a
            .admit(
                BlockAsyncOperation::write(identity(40), GuestAddress::new(0x100), 0, 4)
                    .expect("write should validate"),
            )
            .expect("write should admit");
        let independent = drive_b
            .admit(
                BlockAsyncOperation::read(identity(41), GuestAddress::new(0x200), 8, 4)
                    .expect("read should validate"),
            )
            .expect("read should admit");
        drive_a
            .schedule_one(&memory)
            .expect("blocked write should schedule");
        assert_eq!(
            entered_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("blocked write should enter"),
            0
        );
        drive_b
            .schedule_one(&memory)
            .expect("other drive should schedule");
        assert_eq!(
            entered_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("other drive should enter"),
            8
        );
        let completion = wait_for_completion(&executor);
        assert_eq!(completion.operation_key(), independent);
        drive_b
            .apply_completion(
                &mut memory,
                completion,
                BlockAsyncCompletionDisposition::Apply,
            )
            .expect("other drive should apply");
        release_sender.send(()).expect("write should release");
        let completion = wait_for_completion(&executor);
        assert_eq!(completion.operation_key(), blocked);
        drive_a
            .apply_completion(
                &mut memory,
                completion,
                BlockAsyncCompletionDisposition::Apply,
            )
            .expect("write should apply");
        executor.shutdown().expect("executor should stop");
    }

    #[test]
    fn admission_checks_backing_stop_and_monotonic_identity_exhaustion() {
        let (_temporary, backing) = temporary_backing(&[]);
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 2, 4, 4),
            Arc::new(ImmediateHost),
        )
        .expect("executor should start");
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(62),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        assert_eq!(
            drive.admit(
                BlockAsyncOperation::read(
                    identity(42),
                    GuestAddress::new(0x100),
                    64 * 1024 - 2,
                    4,
                )
                .expect("request geometry should validate"),
            ),
            Err(BlockAsyncAdmissionError::BackingRangeOutOfBounds)
        );
        drive.next_operation_id = u64::MAX;
        assert_eq!(
            drive.admit(BlockAsyncOperation::flush(identity(43))),
            Err(BlockAsyncAdmissionError::OperationIdExhausted)
        );
        drive.next_operation_id = 1;
        drive.next_sequence = u64::MAX;
        assert_eq!(
            drive.admit(BlockAsyncOperation::flush(identity(44))),
            Err(BlockAsyncAdmissionError::SequenceExhausted)
        );
        drive.next_sequence = 1;
        drive.stop_admission();
        assert_eq!(
            drive.admit(BlockAsyncOperation::flush(identity(45))),
            Err(BlockAsyncAdmissionError::Stopped)
        );
        executor.shutdown().expect("executor should stop");
    }

    #[derive(Debug)]
    struct PartialReadHost {
        short_success: bool,
    }

    impl BlockAsyncHostIo for PartialReadHost {
        fn read_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            destination: &mut [u8],
        ) -> BlockAsyncTransferResult {
            destination[..3].copy_from_slice(&[4, 5, 6]);
            if self.short_success {
                BlockAsyncTransferResult::complete(3)
            } else {
                BlockAsyncTransferResult::failed(3, io::ErrorKind::UnexpectedEof)
            }
        }

        fn write_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            _source: &[u8],
        ) -> BlockAsyncTransferResult {
            BlockAsyncTransferResult::failed(0, io::ErrorKind::Unsupported)
        }

        fn flush(&self, _backing: &BlockFileBacking) -> Result<(), io::ErrorKind> {
            Err(io::ErrorKind::Unsupported)
        }
    }

    #[test]
    fn partial_read_applies_only_exact_prefix_and_reports_failure() {
        for (generation, short_success) in [(7, false), (8, true)] {
            let (_temporary, backing) = temporary_backing(&[]);
            let mut executor = BlockAsyncExecutor::with_config_and_host(
                test_config(1, 1, 1, 4, 4),
                Arc::new(PartialReadHost { short_success }),
            )
            .expect("executor should start");
            let mut drive = BlockAsyncDrive::new(
                BlockAsyncDriveGeneration::new(generation),
                backing,
                DriveCacheType::Unsafe,
                executor.handle(),
            )
            .expect("drive should bind");
            let mut memory = guest_memory();
            drive
                .admit(
                    BlockAsyncOperation::read(identity(15), GuestAddress::new(0x100), 0, 4)
                        .expect("read should validate"),
                )
                .expect("read should admit");
            drive.schedule_one(&memory).expect("read should schedule");
            let completion = wait_for_completion(&executor);
            let applied = drive
                .apply_completion(
                    &mut memory,
                    completion,
                    BlockAsyncCompletionDisposition::Apply,
                )
                .expect("read should apply");
            let BlockAsyncApplyOutcome::Completed(applied) = applied else {
                panic!("partial read should finish");
            };
            assert_eq!(applied.bytes_transferred(), 3);
            let expected = if short_success {
                BlockAsyncOperationFailure::ShortIo {
                    operation: BlockAsyncOperationKind::Read,
                    expected: 4,
                    actual: 3,
                }
            } else {
                BlockAsyncOperationFailure::Host {
                    operation: BlockAsyncOperationKind::Read,
                    bytes_transferred: 3,
                    source: io::ErrorKind::UnexpectedEof,
                }
            };
            assert_eq!(
                applied.status(),
                BlockAsyncOperationStatus::Failed(expected)
            );
            let mut guest = [0_u8; 4];
            memory
                .read_slice(&mut guest, GuestAddress::new(0x100))
                .expect("guest should read");
            assert_eq!(guest, [4, 5, 6, 0]);
            assert_eq!(
                applied.dirty_range(),
                GuestMemoryRange::new(GuestAddress::new(0x100), 3).ok()
            );
            executor.shutdown().expect("executor should stop");
        }
    }

    #[derive(Debug, Default)]
    struct PartialWriteAndFlushHost;

    impl BlockAsyncHostIo for PartialWriteAndFlushHost {
        fn read_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            _destination: &mut [u8],
        ) -> BlockAsyncTransferResult {
            BlockAsyncTransferResult::failed(0, io::ErrorKind::Unsupported)
        }

        fn write_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            _source: &[u8],
        ) -> BlockAsyncTransferResult {
            BlockAsyncTransferResult::failed(2, io::ErrorKind::WriteZero)
        }

        fn flush(&self, _backing: &BlockFileBacking) -> Result<(), io::ErrorKind> {
            Err(io::ErrorKind::Other)
        }
    }

    #[test]
    fn partial_write_and_external_flush_return_independent_exact_failures() {
        let (_temporary, backing) = temporary_backing(&[]);
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 2, 4, 4),
            Arc::new(PartialWriteAndFlushHost),
        )
        .expect("executor should start");
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(21),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        memory
            .write_slice(&[1, 2, 3, 4], GuestAddress::new(0x100))
            .expect("guest write should succeed");
        drive
            .admit(
                BlockAsyncOperation::write(identity(22), GuestAddress::new(0x100), 0, 4)
                    .expect("write should validate"),
            )
            .expect("write should admit");
        drive
            .admit(BlockAsyncOperation::flush(identity(23)))
            .expect("flush should admit");
        let outcome = drain_block_async_drives(
            &executor,
            std::slice::from_mut(&mut drive),
            &mut memory,
            BlockAsyncCompletionDisposition::Apply,
        )
        .expect("operations should drain");
        assert_eq!(outcome.completions().len(), 2);
        assert_eq!(outcome.completions()[0].bytes_transferred(), 2);
        assert_eq!(
            outcome.completions()[0].status(),
            BlockAsyncOperationStatus::Failed(BlockAsyncOperationFailure::Host {
                operation: BlockAsyncOperationKind::Write,
                bytes_transferred: 2,
                source: io::ErrorKind::WriteZero,
            })
        );
        assert_eq!(
            outcome.completions()[1].status(),
            BlockAsyncOperationStatus::Failed(BlockAsyncOperationFailure::Host {
                operation: BlockAsyncOperationKind::Flush,
                bytes_transferred: 0,
                source: io::ErrorKind::Other,
            })
        );
        executor.shutdown().expect("executor should stop");
    }

    #[test]
    fn system_host_rejects_read_only_write_with_exact_zero_count() {
        let (temporary, writable_backing) = temporary_backing(&[]);
        drop(writable_backing);
        let read_only_file = fs::File::open(&temporary.path).expect("backing should reopen");
        let backing = Arc::new(
            BlockFileBacking::from_file(read_only_file, true)
                .expect("read-only backing should validate"),
        );
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 1, 4, 4),
            Arc::new(SystemBlockAsyncHostIo),
        )
        .expect("executor should start");
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(24),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        memory
            .write_slice(&[1, 2, 3, 4], GuestAddress::new(0x100))
            .expect("guest write should succeed");
        drive
            .admit(
                BlockAsyncOperation::write(identity(26), GuestAddress::new(0x100), 0, 4)
                    .expect("write should validate"),
            )
            .expect("write should admit");
        let outcome = drain_block_async_drives(
            &executor,
            std::slice::from_mut(&mut drive),
            &mut memory,
            BlockAsyncCompletionDisposition::Apply,
        )
        .expect("write should drain");
        assert_eq!(outcome.completions().len(), 1);
        assert_eq!(outcome.completions()[0].bytes_transferred(), 0);
        assert_eq!(
            outcome.completions()[0].status(),
            BlockAsyncOperationStatus::Failed(BlockAsyncOperationFailure::Host {
                operation: BlockAsyncOperationKind::Write,
                bytes_transferred: 0,
                source: io::ErrorKind::PermissionDenied,
            })
        );
        executor.shutdown().expect("executor should stop");
    }

    #[derive(Debug)]
    struct PanicOnceHost {
        first: AtomicBool,
    }

    impl BlockAsyncHostIo for PanicOnceHost {
        fn read_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            destination: &mut [u8],
        ) -> BlockAsyncTransferResult {
            if self.first.swap(false, Ordering::AcqRel) {
                panic!("injected host panic");
            }
            destination.fill(3);
            BlockAsyncTransferResult::complete(destination.len())
        }

        fn write_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            _source: &[u8],
        ) -> BlockAsyncTransferResult {
            BlockAsyncTransferResult::failed(0, io::ErrorKind::Unsupported)
        }

        fn flush(&self, _backing: &BlockFileBacking) -> Result<(), io::ErrorKind> {
            Ok(())
        }
    }

    #[test]
    fn caught_host_panic_is_terminal_and_worker_is_reused() {
        let (_temporary, backing) = temporary_backing(&[]);
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 2, 4, 4),
            Arc::new(PanicOnceHost {
                first: AtomicBool::new(true),
            }),
        )
        .expect("executor should start");
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(9),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        for (head, address, offset) in [(16, 0x100, 0), (17, 0x200, 8)] {
            drive
                .admit(
                    BlockAsyncOperation::read(
                        identity(head),
                        GuestAddress::new(address),
                        offset,
                        4,
                    )
                    .expect("read should validate"),
                )
                .expect("read should admit");
        }
        drive.schedule_one(&memory).expect("first should schedule");
        let first = wait_for_completion(&executor);
        let first = drive
            .apply_completion(&mut memory, first, BlockAsyncCompletionDisposition::Apply)
            .expect("panic should apply");
        assert!(matches!(
            first,
            BlockAsyncApplyOutcome::Completed(BlockAsyncOperationCompletion {
                status: BlockAsyncOperationStatus::Failed(
                    BlockAsyncOperationFailure::WorkerPanicked
                ),
                ..
            })
        ));
        drive.schedule_one(&memory).expect("second should schedule");
        let second = wait_for_completion(&executor);
        let second = drive
            .apply_completion(&mut memory, second, BlockAsyncCompletionDisposition::Apply)
            .expect("second should apply");
        assert!(matches!(
            second,
            BlockAsyncApplyOutcome::Completed(BlockAsyncOperationCompletion {
                status: BlockAsyncOperationStatus::Success,
                ..
            })
        ));
        executor.shutdown().expect("executor should stop");
    }

    #[derive(Debug)]
    struct FlushHost {
        flushes: AtomicUsize,
        fail: bool,
    }

    impl BlockAsyncHostIo for FlushHost {
        fn read_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            destination: &mut [u8],
        ) -> BlockAsyncTransferResult {
            destination.fill(1);
            BlockAsyncTransferResult::complete(destination.len())
        }

        fn write_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            source: &[u8],
        ) -> BlockAsyncTransferResult {
            BlockAsyncTransferResult::complete(source.len())
        }

        fn flush(&self, _backing: &BlockFileBacking) -> Result<(), io::ErrorKind> {
            self.flushes.fetch_add(1, Ordering::AcqRel);
            if self.fail {
                Err(io::ErrorKind::Other)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn drain_discards_guest_results_and_flushes_only_writeback_cache() {
        let host = Arc::new(FlushHost {
            flushes: AtomicUsize::new(0),
            fail: false,
        });
        let (_temporary_a, backing_a) = temporary_backing(&[]);
        let (_temporary_b, backing_b) = temporary_backing(&[]);
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(2, 2, 2, 4, 8),
            Arc::clone(&host) as Arc<dyn BlockAsyncHostIo>,
        )
        .expect("executor should start");
        let unsafe_drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(10),
            backing_a,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("unsafe drive should bind");
        let mut writeback_drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(11),
            backing_b,
            DriveCacheType::Writeback,
            executor.handle(),
        )
        .expect("writeback drive should bind");
        let mut memory = guest_memory();
        writeback_drive
            .admit(
                BlockAsyncOperation::read(identity(18), GuestAddress::new(0x100), 0, 4)
                    .expect("read should validate"),
            )
            .expect("read should admit");
        writeback_drive
            .schedule_one(&memory)
            .expect("read should schedule");
        let mut drives = [unsafe_drive, writeback_drive];
        let outcome = drain_block_async_drives(
            &executor,
            &mut drives,
            &mut memory,
            BlockAsyncCompletionDisposition::Discard,
        )
        .expect("drives should discard");
        assert!(outcome.completions().is_empty());
        assert!(outcome.persistence_failures().is_empty());
        assert_eq!(host.flushes.load(Ordering::Acquire), 1);
        let mut guest = [0_u8; 4];
        memory
            .read_slice(&mut guest, GuestAddress::new(0x100))
            .expect("guest should read");
        assert_eq!(guest, [0; 4]);
        executor.shutdown().expect("executor should stop");
    }

    #[test]
    fn failed_final_writeback_flush_is_typed() {
        let host = Arc::new(FlushHost {
            flushes: AtomicUsize::new(0),
            fail: true,
        });
        let (_temporary, backing) = temporary_backing(&[]);
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 1, 4, 4),
            Arc::clone(&host) as Arc<dyn BlockAsyncHostIo>,
        )
        .expect("executor should start");
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(12),
            backing,
            DriveCacheType::Writeback,
            executor.handle(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        let outcome = drain_block_async_drives(
            &executor,
            std::slice::from_mut(&mut drive),
            &mut memory,
            BlockAsyncCompletionDisposition::Apply,
        )
        .expect("drive should drain structurally");
        assert_eq!(outcome.persistence_failures().len(), 1);
        assert_eq!(
            outcome.persistence_failures()[0].source(),
            BlockAsyncOperationFailure::Host {
                operation: BlockAsyncOperationKind::Flush,
                bytes_transferred: 0,
                source: io::ErrorKind::Other,
            }
        );
        executor.shutdown().expect("executor should stop");
    }

    #[test]
    fn replacement_generation_discards_stale_completion_without_guest_mutation() {
        let (_temporary, backing) = temporary_backing(&[]);
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 1, 4, 4),
            Arc::new(ImmediateHost),
        )
        .expect("executor should start");
        let mut old_drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(13),
            Arc::clone(&backing),
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("old drive should bind");
        let mut replacement = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(14),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("replacement should bind");
        let mut memory = guest_memory();
        let key = old_drive
            .admit(
                BlockAsyncOperation::read(identity(19), GuestAddress::new(0x100), 7, 4)
                    .expect("read should validate"),
            )
            .expect("read should admit");
        old_drive
            .schedule_one(&memory)
            .expect("old read should schedule");
        let completion = wait_for_completion(&executor);
        assert_eq!(
            replacement
                .apply_completion(
                    &mut memory,
                    completion,
                    BlockAsyncCompletionDisposition::Apply,
                )
                .expect("stale completion should classify"),
            BlockAsyncApplyOutcome::Stale(key)
        );
        let mut guest = [0_u8; 4];
        memory
            .read_slice(&mut guest, GuestAddress::new(0x100))
            .expect("guest should read");
        assert_eq!(guest, [0; 4]);
        assert_eq!(executor.outstanding_tasks(), 0);
        executor.shutdown().expect("executor should stop");
    }

    #[test]
    fn notifier_is_nonblocking_coalescing_and_completion_precedes_signal() {
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 1, 4, 4),
            Arc::new(ImmediateHost),
        )
        .expect("executor should start");
        let descriptor = executor
            .completion_fd()
            .expect("running executor should expose completion fd");
        let flags = retry_fcntl(descriptor, libc::F_GETFL, 0).expect("status flags should read");
        let descriptor_flags =
            retry_fcntl(descriptor, libc::F_GETFD, 0).expect("descriptor flags should read");
        assert_ne!(flags & libc::O_NONBLOCK, 0);
        assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
        let debug = format!("{executor:?}");
        assert!(debug.contains("outstanding_tasks"));
        assert!(!debug.contains("completion_fd"));
        assert!(!debug.contains("descriptor"));

        let mut coalesced = false;
        for _ in 0..1_000_000 {
            if executor
                .shared
                .as_ref()
                .expect("running executor should retain shared state")
                .signal
                .signal()
                .expect("pipe signal should remain valid")
                == BlockAsyncSignalOutcome::Coalesced
            {
                coalesced = true;
                break;
            }
        }
        assert!(coalesced);
        assert!(matches!(
            executor
                .drain_notification()
                .expect("notification should drain"),
            BlockAsyncNotificationDrain::Drained { notifications } if notifications > 0
        ));
        assert_eq!(
            executor
                .drain_notification()
                .expect("empty pipe should report"),
            BlockAsyncNotificationDrain::WouldBlock
        );

        let (_temporary, backing) = temporary_backing(&[]);
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(15),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        drive
            .admit(
                BlockAsyncOperation::read(identity(20), GuestAddress::new(0x100), 0, 4)
                    .expect("read should validate"),
            )
            .expect("read should admit");
        drive.schedule_one(&memory).expect("read should schedule");
        let mut readiness = libc::pollfd {
            fd: descriptor,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: One initialized pollfd is writable for the bounded wait.
        let ready = unsafe { libc::poll(&raw mut readiness, 1, 1_000) };
        assert_eq!(ready, 1);
        assert_ne!(readiness.revents & libc::POLLIN, 0);
        let completion = executor
            .try_recv_completion()
            .expect("completion queue should remain connected")
            .expect("typed completion must precede its readable signal");
        assert!(matches!(
            executor
                .drain_notification()
                .expect("completion signal should drain"),
            BlockAsyncNotificationDrain::Drained { notifications: 1 }
        ));
        drive
            .apply_completion(
                &mut memory,
                completion,
                BlockAsyncCompletionDisposition::Apply,
            )
            .expect("completion should apply");
        executor.shutdown().expect("executor should stop");
    }

    #[test]
    fn full_or_stopped_task_send_releases_every_reserved_resource() {
        let (_temporary, backing) = temporary_backing(&[]);
        let (_notifier, signal) = create_completion_notifier().expect("notifier should create");
        let (sender, _receiver) = mpsc::sync_channel(0);
        let config = test_config(1, 1, 1, 4, 4);
        let budget = Arc::new(ResourceBudget::new(1, 4));
        let shared = Arc::new(ExecutorShared {
            sender,
            signal,
            budget: Arc::clone(&budget),
            accepting: AtomicBool::new(true),
            submission_gate: Mutex::new(()),
            config,
        });
        let handle = BlockAsyncExecutorHandle {
            shared: Arc::downgrade(&shared),
        };
        let key = BlockAsyncChunkKey {
            operation: BlockAsyncOperationKey {
                generation: BlockAsyncDriveGeneration::new(1),
                operation_id: 1,
                sequence: 1,
            },
            offset: 0,
            len: 4,
            host_offset: 0,
        };
        let permit = handle.reserve(4).expect("resources should reserve");
        assert_eq!(budget.tasks.load(Ordering::Acquire), 1);
        assert_eq!(budget.bytes.load(Ordering::Acquire), 4);
        assert_eq!(
            permit.submit(
                key,
                Arc::clone(&backing),
                PreparedPayload::Write(vec![1; 4]),
                Instant::now(),
            ),
            Err(BlockAsyncSubmitError::Full)
        );
        assert_eq!(budget.tasks.load(Ordering::Acquire), 0);
        assert_eq!(budget.bytes.load(Ordering::Acquire), 0);

        let permit = handle.reserve(4).expect("resources should reserve again");
        shared.accepting.store(false, Ordering::Release);
        assert_eq!(
            permit.submit(
                key,
                backing,
                PreparedPayload::Read(vec![0; 4]),
                Instant::now(),
            ),
            Err(BlockAsyncSubmitError::Stopped)
        );
        assert_eq!(budget.tasks.load(Ordering::Acquire), 0);
        assert_eq!(budget.bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn explicit_shutdown_does_not_wait_for_owner_held_completion_and_closes_fds() {
        let (_temporary, backing) = temporary_backing(&[]);
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 1, 4, 4),
            Arc::new(ImmediateHost),
        )
        .expect("executor should start");
        let handle = executor.handle();
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(22),
            backing,
            DriveCacheType::Unsafe,
            handle.clone(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        drive
            .admit(
                BlockAsyncOperation::read(identity(24), GuestAddress::new(0x100), 0, 4)
                    .expect("read should validate"),
            )
            .expect("read should admit");
        drive.schedule_one(&memory).expect("read should schedule");
        let descriptor = executor
            .completion_fd()
            .expect("running executor should expose completion fd");
        let mut readiness = libc::pollfd {
            fd: descriptor,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: One initialized pollfd is writable for the bounded wait.
        assert_eq!(unsafe { libc::poll(&raw mut readiness, 1, 1_000) }, 1);
        let completion = executor
            .try_recv_completion()
            .expect("completion queue should remain connected")
            .expect("owner should take completed work");
        assert_eq!(executor.outstanding_tasks(), 1);

        executor
            .shutdown()
            .expect("shutdown should not wait for owner-held completion");
        assert_eq!(executor.outstanding_tasks(), 0);
        assert_eq!(executor.reserved_buffer_bytes(), 0);
        assert_eq!(executor.completion_fd(), None);
        assert!(!handle.is_accepting());
        // SAFETY: F_GETFD only probes whether the formerly owned number is live.
        assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) }, -1);
        assert!(matches!(
            drive
                .apply_completion(
                    &mut memory,
                    completion,
                    BlockAsyncCompletionDisposition::Apply,
                )
                .expect("owner-held completion should remain applicable"),
            BlockAsyncApplyOutcome::Completed(BlockAsyncOperationCompletion {
                status: BlockAsyncOperationStatus::Success,
                ..
            })
        ));
        executor.shutdown().expect("shutdown should be idempotent");
    }

    #[test]
    fn shutdown_finishes_running_and_queued_tasks_before_joining_worker() {
        let (_temporary, backing) = temporary_backing(&[]);
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 3, 3, 4, 12),
            Arc::new(GateHost {
                blocked_offset: 0,
                entered: entered_sender,
                release: Mutex::new(release_receiver),
            }),
        )
        .expect("executor should start");
        let handle = executor.handle();
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(25),
            backing,
            DriveCacheType::Unsafe,
            handle.clone(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        memory
            .write_slice(&[1; 12], GuestAddress::new(0x100))
            .expect("guest write should succeed");
        for (head, guest, host) in [(27, 0x100, 0), (28, 0x104, 8), (29, 0x108, 16)] {
            drive
                .admit(
                    BlockAsyncOperation::write(identity(head), GuestAddress::new(guest), host, 4)
                        .expect("write should validate"),
                )
                .expect("write should admit");
        }
        drive.schedule_one(&memory).expect("first should schedule");
        assert_eq!(
            entered_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("running task should enter"),
            0
        );
        drive.schedule_one(&memory).expect("second should queue");
        drive.schedule_one(&memory).expect("third should queue");
        assert_eq!(executor.outstanding_tasks(), 3);

        std::thread::scope(|scope| {
            let (shutdown_started_sender, shutdown_started_receiver) = mpsc::channel();
            let executor_for_shutdown = &mut executor;
            let shutdown = scope.spawn(move || {
                shutdown_started_sender
                    .send(())
                    .expect("shutdown start should be observed");
                executor_for_shutdown.shutdown()
            });
            shutdown_started_receiver
                .recv()
                .expect("shutdown should start");
            release_sender
                .send(())
                .expect("running task should release");
            shutdown
                .join()
                .expect("shutdown thread should not panic")
                .expect("shutdown should finish queued work");
        });
        assert_eq!(
            entered_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("second task should run"),
            8
        );
        assert_eq!(
            entered_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("third task should run"),
            16
        );
        assert!(!handle.is_accepting());
        assert_eq!(executor.outstanding_tasks(), 0);
        assert_eq!(executor.reserved_buffer_bytes(), 0);
        assert_eq!(executor.completion_fd(), None);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn broken_notifier_does_not_hide_typed_completion() {
        let (_temporary, backing) = temporary_backing(&[]);
        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 1, 4, 4),
            Arc::new(ImmediateHost),
        )
        .expect("executor should start");
        let notifier = executor
            .notifier
            .take()
            .expect("running executor should own notifier");
        drop(notifier);
        let mut drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(23),
            backing,
            DriveCacheType::Unsafe,
            executor.handle(),
        )
        .expect("drive should bind");
        let mut memory = guest_memory();
        drive
            .admit(
                BlockAsyncOperation::read(identity(25), GuestAddress::new(0x100), 0, 4)
                    .expect("read should validate"),
            )
            .expect("read should admit");
        drive.schedule_one(&memory).expect("read should schedule");
        let completion = wait_for_completion(&executor);
        assert!(matches!(
            drive
                .apply_completion(
                    &mut memory,
                    completion,
                    BlockAsyncCompletionDisposition::Apply,
                )
                .expect("typed completion should remain applicable"),
            BlockAsyncApplyOutcome::Completed(BlockAsyncOperationCompletion {
                status: BlockAsyncOperationStatus::Success,
                ..
            })
        ));
        assert_eq!(
            executor.shutdown(),
            Err(BlockAsyncExecutorError::Notification(
                io::ErrorKind::BrokenPipe,
            ))
        );
        assert_eq!(
            executor.notification_health(),
            Err(BlockAsyncExecutorError::Notification(
                io::ErrorKind::BrokenPipe,
            ))
        );
        assert!(!executor.handle().is_accepting());
    }

    #[test]
    fn handles_do_not_keep_executor_or_backing_alive() {
        let (_temporary, backing) = temporary_backing(&[]);
        let weak_backing = Arc::downgrade(&backing);
        let executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 1, 4, 4),
            Arc::new(ImmediateHost),
        )
        .expect("executor should start");
        let handle = executor.handle();
        let drive = BlockAsyncDrive::new(
            BlockAsyncDriveGeneration::new(16),
            backing,
            DriveCacheType::Unsafe,
            handle.clone(),
        )
        .expect("drive should bind");
        let descriptor = executor
            .completion_fd()
            .expect("running executor should expose completion fd");
        drop(drive);
        assert!(weak_backing.upgrade().is_none());
        drop(executor);
        assert!(!handle.is_accepting());
        // SAFETY: F_GETFD only probes whether the formerly owned number is live.
        assert_eq!(unsafe { libc::fcntl(descriptor, libc::F_GETFD) }, -1);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn completion_pipe_is_kqueue_readable_and_clears_after_drain() {
        use std::mem::MaybeUninit;

        let mut executor = BlockAsyncExecutor::with_config_and_host(
            test_config(1, 1, 1, 4, 4),
            Arc::new(ImmediateHost),
        )
        .expect("executor should start");
        // SAFETY: kqueue creates one new descriptor with no pointer arguments.
        let queue = unsafe { libc::kqueue() };
        assert!(queue >= 0);
        // SAFETY: Successful kqueue returned one new descriptor adopted once.
        let queue = unsafe { OwnedFd::from_raw_fd(queue) };
        let change = libc::kevent {
            ident: usize::try_from(
                executor
                    .completion_fd()
                    .expect("running executor should expose completion fd"),
            )
            .expect("fd should be nonnegative"),
            filter: libc::EVFILT_READ,
            flags: libc::EV_ADD | libc::EV_ENABLE,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: One initialized change is readable; no output list is used.
        let registered = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                &raw const change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(registered, 0);
        assert_eq!(
            executor
                .shared
                .as_ref()
                .expect("running executor should retain shared state")
                .signal
                .signal()
                .expect("signal should succeed"),
            BlockAsyncSignalOutcome::Signaled
        );
        // SAFETY: The kernel initializes the event before the successful read.
        let mut event: libc::kevent = unsafe { MaybeUninit::zeroed().assume_init() };
        let timeout = libc::timespec {
            tv_sec: 1,
            tv_nsec: 0,
        };
        // SAFETY: One live output event and timeout are provided for this wait.
        let ready = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                std::ptr::null(),
                0,
                &raw mut event,
                1,
                &raw const timeout,
            )
        };
        assert_eq!(ready, 1);
        assert_eq!(event.filter, libc::EVFILT_READ);
        assert!(matches!(
            executor
                .drain_notification()
                .expect("notification should drain"),
            BlockAsyncNotificationDrain::Drained { notifications: 1 }
        ));
        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: The same live event is reused for a nonblocking readiness poll.
        let residual = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                std::ptr::null(),
                0,
                &raw mut event,
                1,
                &raw const zero,
            )
        };
        assert_eq!(residual, 0);
        executor.shutdown().expect("executor should stop");
    }
}
