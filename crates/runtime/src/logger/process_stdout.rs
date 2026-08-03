use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;
use std::time::Duration;

/// A bounded process-stdout adapter for the logger and status writers.
///
/// Writers use one nonblocking internal pipe. A single process-owned forwarder
/// may block on the real stdout descriptor without changing its shared status
/// flags or blocking a logger producer. A successful write confirms pipe
/// admission; forwarding is a later non-durable process transport.
#[derive(Clone)]
pub struct ProcessStdoutLogger {
    output: Arc<File>,
    progress: Arc<ProcessStdoutProgress>,
}

impl fmt::Debug for ProcessStdoutLogger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessStdoutLogger")
            .finish_non_exhaustive()
    }
}

impl ProcessStdoutLogger {
    /// Prepares a bounded adapter for writable process stdout.
    pub fn prepare() -> Result<Self, ProcessStdoutLoggerError> {
        Self::prepare_from_descriptor(libc::STDOUT_FILENO)
    }

    /// Prepares one caller-owned descriptor for focused descriptor verification.
    #[doc(hidden)]
    pub fn prepare_from_descriptor(descriptor: RawFd) -> Result<Self, ProcessStdoutLoggerError> {
        let target = duplicate_descriptor(descriptor)
            .map_err(ProcessStdoutLoggerError::DuplicateDescriptor)?;
        let target_flags = descriptor_status_flags(target.as_raw_fd())
            .map_err(ProcessStdoutLoggerError::InspectDescriptor)?;
        if target_flags & libc::O_ACCMODE == libc::O_RDONLY {
            return Err(ProcessStdoutLoggerError::NotWritable);
        }

        let (input, output) = create_pipe().map_err(ProcessStdoutLoggerError::CreatePipe)?;
        set_descriptor_close_on_exec(input.as_raw_fd())
            .map_err(ProcessStdoutLoggerError::ConfigurePipe)?;
        set_descriptor_close_on_exec(output.as_raw_fd())
            .map_err(ProcessStdoutLoggerError::ConfigurePipe)?;
        let output_flags = descriptor_status_flags(output.as_raw_fd())
            .map_err(ProcessStdoutLoggerError::ConfigurePipe)?;
        set_descriptor_status_flags(output.as_raw_fd(), output_flags | libc::O_NONBLOCK)
            .map_err(ProcessStdoutLoggerError::ConfigurePipe)?;

        let progress = Arc::new(ProcessStdoutProgress::default());
        let forwarder_progress = Arc::clone(&progress);
        thread::Builder::new()
            .name("bangbang-stdout-forwarder".to_owned())
            .spawn(move || forward_stdout(input, target, &forwarder_progress))
            .map_err(|error| ProcessStdoutLoggerError::SpawnForwarder(error.kind()))?;

        Ok(Self {
            output: Arc::new(output),
            progress,
        })
    }

    /// Waits a bounded interval for bytes accepted before this call to reach stdout.
    ///
    /// Returns `false` after a downstream failure or timeout. It does not join
    /// the sole forwarder, which may remain blocked until process exit.
    pub fn flush_forwarded(&self, timeout: Duration) -> bool {
        self.progress.wait_for_forwarded(timeout)
    }
}

impl Write for ProcessStdoutLogger {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut output = self.output.as_ref();
        let written = output.write(bytes)?;
        self.progress.record_enqueued(written);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut output = self.output.as_ref();
        output.flush()
    }
}

#[derive(Default)]
struct ProcessStdoutProgress {
    enqueued: AtomicU64,
    forwarded: Mutex<ForwardedState>,
    changed: Condvar,
}

#[derive(Default)]
struct ForwardedState {
    bytes: u64,
    failed: bool,
}

impl ProcessStdoutProgress {
    fn record_enqueued(&self, bytes: usize) {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        let _ = self
            .enqueued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_add(bytes))
            });
    }

    fn record_forwarded(&self, bytes: usize) {
        let Ok(mut state) = self.forwarded.lock() else {
            return;
        };
        state.bytes = state
            .bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        self.changed.notify_all();
    }

    fn record_failure(&self) {
        let Ok(mut state) = self.forwarded.lock() else {
            return;
        };
        state.failed = true;
        self.changed.notify_all();
    }

    fn wait_for_forwarded(&self, timeout: Duration) -> bool {
        let expected = self.enqueued.load(Ordering::Acquire);
        let Ok(state) = self.forwarded.lock() else {
            return false;
        };
        let Ok((state, _)) = self.changed.wait_timeout_while(state, timeout, |state| {
            !state.failed && state.bytes < expected
        }) else {
            return false;
        };
        !state.failed && state.bytes >= expected
    }
}

fn forward_stdout(mut input: File, mut target: File, progress: &ProcessStdoutProgress) {
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = loop {
            match input.read(&mut buffer) {
                Ok(read) => break read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    progress.record_failure();
                    return;
                }
            }
        };
        if read == 0 {
            return;
        }
        let Some(bytes) = buffer.get(..read) else {
            progress.record_failure();
            return;
        };
        if write_all_to_target(&mut target, bytes).is_err() {
            progress.record_failure();
            return;
        }
        progress.record_forwarded(read);
    }
}

fn write_all_to_target(target: &mut File, mut bytes: &[u8]) -> Result<(), io::ErrorKind> {
    while !bytes.is_empty() {
        match target.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero),
            Ok(written) => {
                let Some(remaining) = bytes.get(written..) else {
                    return Err(io::ErrorKind::InvalidData);
                };
                bytes = remaining;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_for_descriptor_write(target.as_raw_fd())?;
            }
            Err(error) => return Err(error.kind()),
        }
    }
    Ok(())
}

fn wait_for_descriptor_write(descriptor: RawFd) -> Result<(), io::ErrorKind> {
    let mut event = libc::pollfd {
        fd: descriptor,
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        // SAFETY: `event` is one initialized writable poll entry. The sole
        // forwarder may wait indefinitely without blocking a logger producer.
        let ready = unsafe { libc::poll(&raw mut event, 1, -1) };
        if ready > 0 {
            return Ok(());
        }
        if ready == 0 {
            continue;
        }
        let kind = io::Error::last_os_error().kind();
        if kind != io::ErrorKind::Interrupted {
            return Err(kind);
        }
    }
}

fn create_pipe() -> Result<(File, File), io::ErrorKind> {
    let mut descriptors = [-1; 2];
    // SAFETY: `descriptors` points to storage for exactly two descriptors and
    // successful results are immediately transferred into owned `File`s.
    if unsafe { libc::pipe(descriptors.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error().kind());
    }
    // SAFETY: a successful `pipe` returns two fresh descriptors.
    let input = unsafe { File::from_raw_fd(descriptors[0]) };
    // SAFETY: ownership of the second fresh descriptor transfers once here.
    let output = unsafe { File::from_raw_fd(descriptors[1]) };
    Ok((input, output))
}

fn duplicate_descriptor(descriptor: RawFd) -> Result<File, io::ErrorKind> {
    // SAFETY: `F_DUPFD_CLOEXEC` borrows the caller-owned live descriptor and
    // returns a fresh descriptor on success.
    let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        Err(io::Error::last_os_error().kind())
    } else {
        // SAFETY: `duplicate` is a fresh successful descriptor owned here.
        Ok(unsafe { File::from_raw_fd(duplicate) })
    }
}

fn descriptor_status_flags(descriptor: RawFd) -> Result<libc::c_int, io::ErrorKind> {
    // SAFETY: `F_GETFL` only inspects the borrowed live descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        Err(io::Error::last_os_error().kind())
    } else {
        Ok(flags)
    }
}

fn set_descriptor_status_flags(descriptor: RawFd, flags: libc::c_int) -> Result<(), io::ErrorKind> {
    // SAFETY: `F_SETFL` changes only mutable status flags on the borrowed live
    // internal pipe descriptor.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags) } < 0 {
        Err(io::Error::last_os_error().kind())
    } else {
        Ok(())
    }
}

fn set_descriptor_close_on_exec(descriptor: RawFd) -> Result<(), io::ErrorKind> {
    // SAFETY: `F_GETFD` only inspects the borrowed live descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error().kind());
    }
    // SAFETY: `F_SETFD` changes only descriptor-local flags on the borrowed
    // internal pipe descriptor.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        Err(io::Error::last_os_error().kind())
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStdoutLoggerError {
    DuplicateDescriptor(io::ErrorKind),
    InspectDescriptor(io::ErrorKind),
    NotWritable,
    CreatePipe(io::ErrorKind),
    ConfigurePipe(io::ErrorKind),
    SpawnForwarder(io::ErrorKind),
}

impl fmt::Display for ProcessStdoutLoggerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateDescriptor(kind) => {
                write!(
                    formatter,
                    "process stdout could not be duplicated: {kind:?}"
                )
            }
            Self::InspectDescriptor(kind) => {
                write!(formatter, "process stdout could not be inspected: {kind:?}")
            }
            Self::NotWritable => formatter.write_str("process stdout is not writable"),
            Self::CreatePipe(kind) => {
                write!(
                    formatter,
                    "process stdout adapter could not be created: {kind:?}"
                )
            }
            Self::ConfigurePipe(kind) => {
                write!(
                    formatter,
                    "process stdout adapter could not be configured: {kind:?}"
                )
            }
            Self::SpawnForwarder(kind) => {
                write!(
                    formatter,
                    "process stdout forwarder could not be started: {kind:?}"
                )
            }
        }
    }
}

impl std::error::Error for ProcessStdoutLoggerError {}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    use super::{
        ProcessStdoutLogger, ProcessStdoutLoggerError, create_pipe, descriptor_status_flags,
    };

    fn mutable_status_flags(flags: libc::c_int) -> libc::c_int {
        let mask =
            libc::O_ACCMODE | libc::O_APPEND | libc::O_NONBLOCK | libc::O_ASYNC | libc::O_SYNC;
        flags & mask
    }

    fn read_exact_with_timeout(reader: &mut (impl Read + AsRawFd), bytes: &mut [u8]) {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut offset = 0;
        while offset < bytes.len() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let timeout = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
            let mut event = libc::pollfd {
                fd: reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `event` is one initialized writable poll entry.
            let ready = unsafe { libc::poll(&raw mut event, 1, timeout) };
            assert!(
                ready > 0,
                "stdout adapter bytes should arrive before timeout"
            );
            match reader.read(&mut bytes[offset..]) {
                Ok(0) => panic!("stdout adapter closed before forwarding every byte"),
                Ok(read) => offset += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => panic!("stdout adapter read failed: {error}"),
            }
        }
    }

    fn fill_nonblocking_socket(writer: &mut UnixStream) -> usize {
        let bytes = [b'x'; 4096];
        let mut written = 0;
        loop {
            match writer.write(&bytes) {
                Ok(0) => panic!("stdout fixture made no progress"),
                Ok(count) => written += count,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return written,
                Err(error) => panic!("stdout fixture should fill: {error}"),
            }
        }
    }

    #[test]
    fn process_stdout_adapter_is_nonblocking_close_on_exec_and_target_is_unchanged() {
        let (mut reader, target) = create_pipe().expect("stdout fixture should create");
        let descriptor = target.as_raw_fd();
        let original = descriptor_status_flags(descriptor).expect("stdout flags should inspect");

        let mut output = ProcessStdoutLogger::prepare_from_descriptor(descriptor)
            .expect("writable stdout fixture should prepare");
        assert_eq!(
            mutable_status_flags(
                descriptor_status_flags(descriptor).expect("target flags should inspect")
            ),
            mutable_status_flags(original)
        );
        assert_ne!(
            descriptor_status_flags(output.output.as_raw_fd())
                .expect("adapter flags should inspect")
                & libc::O_NONBLOCK,
            0
        );
        // SAFETY: `F_GETFD` only inspects the live owned adapter descriptor.
        let descriptor_flags = unsafe { libc::fcntl(output.output.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);

        output
            .write_all(b"logger\n")
            .expect("adapter write should succeed");
        assert!(
            output.flush_forwarded(Duration::from_secs(1)),
            "accepted logger bytes should reach writable stdout"
        );
        let mut bytes = [0_u8; 7];
        read_exact_with_timeout(&mut reader, &mut bytes);
        assert_eq!(&bytes, b"logger\n");
        assert_eq!(
            mutable_status_flags(
                descriptor_status_flags(descriptor).expect("final target flags should inspect")
            ),
            mutable_status_flags(original)
        );
    }

    #[test]
    fn process_stdout_clones_share_ordered_forwarding_without_target_flag_changes() {
        let (mut reader, target) = create_pipe().expect("stdout fixture should create");
        let descriptor = target.as_raw_fd();
        let original = descriptor_status_flags(descriptor).expect("stdout flags should inspect");
        let mut logger = ProcessStdoutLogger::prepare_from_descriptor(descriptor)
            .expect("writable stdout fixture should prepare");
        let mut status = logger.clone();

        logger.write_all(b"first\n").expect("logger should write");
        drop(logger);
        status.write_all(b"second\n").expect("status should write");
        assert!(
            status.flush_forwarded(Duration::from_secs(1)),
            "accepted logger and status bytes should reach stdout"
        );
        let mut bytes = [0_u8; 13];
        read_exact_with_timeout(&mut reader, &mut bytes);
        assert_eq!(&bytes, b"first\nsecond\n");
        assert_eq!(
            mutable_status_flags(
                descriptor_status_flags(descriptor)
                    .expect("target flags should remain inspectable")
            ),
            mutable_status_flags(original)
        );
    }

    #[test]
    fn process_stdout_forwarder_resumes_after_temporary_nonblocking_backpressure() {
        let (mut reader, mut target) =
            UnixStream::pair().expect("stdout socket fixture should create");
        target
            .set_nonblocking(true)
            .expect("stdout target should become nonblocking");
        let descriptor = target.as_raw_fd();
        let original = descriptor_status_flags(descriptor).expect("stdout flags should inspect");
        let filled = fill_nonblocking_socket(&mut target);
        let mut output = ProcessStdoutLogger::prepare_from_descriptor(descriptor)
            .expect("full writable stdout fixture should prepare");
        let marker = b"forwarded-after-backpressure\n";

        output
            .write_all(marker)
            .expect("adapter should accept the marker while stdout is full");
        assert!(
            !output.flush_forwarded(Duration::from_millis(10)),
            "full stdout must not report the marker as forwarded"
        );

        let mut received = vec![0_u8; filled + marker.len()];
        read_exact_with_timeout(&mut reader, &mut received);
        assert_eq!(&received[..filled], vec![b'x'; filled]);
        assert_eq!(&received[filled..], marker);
        assert!(
            output.flush_forwarded(Duration::from_secs(1)),
            "the same forwarder should resume after stdout becomes writable"
        );
        assert_eq!(
            mutable_status_flags(
                descriptor_status_flags(descriptor)
                    .expect("target flags should remain inspectable")
            ),
            mutable_status_flags(original)
        );
    }

    #[test]
    fn process_stdout_target_failure_bounds_progress_and_closes_future_admission() {
        let (reader, target) = UnixStream::pair().expect("stdout socket fixture should create");
        let mut output = ProcessStdoutLogger::prepare_from_descriptor(target.as_raw_fd())
            .expect("writable stdout fixture should prepare");
        drop(reader);

        output
            .write_all(b"unforwardable\n")
            .expect("adapter can admit bytes before the target failure is observed");
        assert!(
            !output.flush_forwarded(Duration::from_secs(1)),
            "target failure must make bounded progress fail"
        );

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match output.write(b"later\n") {
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => break,
                Err(error) => panic!("closed adapter returned an unexpected error: {error}"),
                Ok(_) if Instant::now() < deadline => std::thread::yield_now(),
                Ok(_) => panic!("closed adapter should reject future admission"),
            }
        }
    }

    #[test]
    fn process_stdout_rejects_read_only_descriptor_without_mutation() {
        let file = File::open("/dev/null").expect("read-only null device should open");
        let descriptor = file.as_raw_fd();
        let original = descriptor_status_flags(descriptor).expect("null flags should inspect");

        assert_eq!(
            ProcessStdoutLogger::prepare_from_descriptor(descriptor)
                .expect_err("read-only stdout must reject"),
            ProcessStdoutLoggerError::NotWritable
        );
        assert_eq!(
            mutable_status_flags(
                descriptor_status_flags(descriptor).expect("rejected flags should inspect")
            ),
            mutable_status_flags(original)
        );
    }
}
