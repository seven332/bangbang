//! One process-wide boundary for Darwin Unix-socket operations by directory anchor.

use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Mutex;

use bangbang_session::ObjectIdentity;

// Darwin has no descriptor-relative Unix-socket bind or connect. Every process
// cwd switch in the launcher must share this one lock for the whole operation
// and its verified restoration.
static CWD_OPERATION_LOCK: Mutex<()> = Mutex::new(());

/// Redacted failure at the shared process-cwd boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScopedCwdError;

/// Distinguishes the caller's operation error from the shared cwd boundary.
pub(crate) enum ScopedCwdOperationError<E> {
    /// Entering, validating, restoring, or locking the cwd boundary failed.
    Boundary(ScopedCwdError),
    /// The bounded operation returned its own error after entering the anchor.
    Operation(E),
}

/// Runs one closure relative to an exact live directory anchor and restores cwd.
///
/// The one process-wide lock remains held until restoration has completed and
/// been independently revalidated. A panic is resumed only after the same
/// explicit restoration path succeeds; the process aborts if restoration also
/// fails because unwinding into an unknown process-wide cwd is unsafe.
pub(crate) fn with_scoped_cwd<T, E>(
    anchor_descriptor: RawFd,
    anchor_identity: ObjectIdentity,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, ScopedCwdOperationError<E>> {
    let lock = CWD_OPERATION_LOCK
        .lock()
        .map_err(|_| ScopedCwdOperationError::Boundary(ScopedCwdError))?;
    let mut guard = CwdGuard::enter(anchor_descriptor, anchor_identity)
        .map_err(ScopedCwdOperationError::Boundary)?;
    let outcome = catch_unwind(AssertUnwindSafe(operation));
    let restored = guard.restore();
    if restored.is_err() && guard.restore().is_err() {
        // Continuing with an unknown process-wide cwd could redirect every
        // later relative operation outside its validated anchor.
        std::process::abort();
    }
    // Keep the global boundary locked through the independently validated
    // recovery attempt after an explicit restoration failure.
    drop(guard);
    drop(lock);
    match (outcome, restored) {
        (Ok(Ok(value)), Ok(())) => Ok(value),
        (Ok(Err(error)), Ok(())) => Err(ScopedCwdOperationError::Operation(error)),
        (Ok(_), Err(error)) => Err(ScopedCwdOperationError::Boundary(error)),
        (Err(payload), Ok(())) => resume_unwind(payload),
        (Err(payload), Err(_)) => resume_unwind(payload),
    }
}

struct CwdGuard {
    saved: Option<OwnedFd>,
    identity: ObjectIdentity,
}

impl CwdGuard {
    fn enter(
        anchor_descriptor: RawFd,
        anchor_identity: ObjectIdentity,
    ) -> Result<Self, ScopedCwdError> {
        // SAFETY: The fixed relative directory path is NUL-terminated; success
        // returns a fresh close-on-exec directory descriptor.
        let saved = unsafe {
            libc::open(
                c".".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if saved < 0 {
            return Err(ScopedCwdError);
        }
        // SAFETY: `saved` is the fresh descriptor returned above.
        let saved = unsafe { OwnedFd::from_raw_fd(saved) };
        let identity = directory_descriptor_identity(saved.as_raw_fd())?;
        if current_directory_identity()? != identity
            || directory_descriptor_identity(anchor_descriptor)? != anchor_identity
        {
            return Err(ScopedCwdError);
        }
        let guard = Self {
            saved: Some(saved),
            identity,
        };
        // SAFETY: The independently validated retained descriptor is a live directory.
        if unsafe { libc::fchdir(anchor_descriptor) } != 0
            || current_directory_identity()? != anchor_identity
        {
            return Err(ScopedCwdError);
        }
        Ok(guard)
    }

    fn restore(&mut self) -> Result<(), ScopedCwdError> {
        let saved = self.saved.as_ref().ok_or(ScopedCwdError)?;
        if directory_descriptor_identity(saved.as_raw_fd())? != self.identity {
            return Err(ScopedCwdError);
        }
        // SAFETY: `saved` remains a live descriptor for the original cwd.
        if unsafe { libc::fchdir(saved.as_raw_fd()) } != 0
            || current_directory_identity()? != self.identity
        {
            return Err(ScopedCwdError);
        }
        self.saved.take();
        Ok(())
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.as_ref() {
            // SAFETY: Best-effort restoration uses the still-owned original cwd.
            let _ = unsafe { libc::fchdir(saved.as_raw_fd()) };
        }
    }
}

pub(crate) fn current_directory_identity() -> Result<ObjectIdentity, ScopedCwdError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The fixed relative name is live and output storage is writable.
    if unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            c".".as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(ScopedCwdError);
    }
    // SAFETY: Successful fstatat initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(ScopedCwdError);
    }
    Ok(stat_identity(&stat))
}

pub(crate) fn directory_descriptor_identity(
    descriptor: RawFd,
) -> Result<ObjectIdentity, ScopedCwdError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The descriptor remains live and output storage is writable.
    if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
        return Err(ScopedCwdError);
    }
    // SAFETY: Successful fstat initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(ScopedCwdError);
    }
    Ok(stat_identity(&stat))
}

fn stat_identity(stat: &libc::stat) -> ObjectIdentity {
    ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::os::fd::AsRawFd;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use super::*;

    const CHILD_ENV: &str = "BANGBANG_TEST_SCOPED_CWD_CHILD";
    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn create(label: &str) -> Self {
            let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-scoped-cwd-{label}-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should create");
            Self(path)
        }

        fn open(&self) -> File {
            File::open(&self.0).expect("test directory should open")
        }

        fn identity(&self) -> ObjectIdentity {
            let descriptor = self.open();
            directory_descriptor_identity(descriptor.as_raw_fd()).expect("identity should inspect")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir(&self.0);
        }
    }

    #[test]
    fn scoped_cwd_restores_success_error_panic_and_serializes_operations() {
        if std::env::var_os(CHILD_ENV).is_none() {
            let status =
                Command::new(std::env::current_exe().expect("test executable should resolve"))
                    .arg("scoped_cwd_restores_success_error_panic_and_serializes_operations")
                    .current_dir("/")
                    .env(CHILD_ENV, "1")
                    .env("RUST_TEST_THREADS", "1")
                    .status()
                    .expect("isolated cwd test should launch");
            assert!(status.success(), "isolated cwd test should pass: {status}");
            return;
        }

        let original = std::env::current_dir().expect("original cwd should read");
        let unrelated = TestDir::create("unrelated");
        let first = TestDir::create("first");
        let second = TestDir::create("second");
        std::env::set_current_dir(&unrelated.0).expect("test cwd should change");
        let unrelated_identity = current_directory_identity().expect("cwd should inspect");

        let first_descriptor = first.open();
        let value = with_scoped_cwd(
            first_descriptor.as_raw_fd(),
            first.identity(),
            || -> Result<_, ()> {
                assert_eq!(
                    current_directory_identity().expect("entered cwd should inspect"),
                    first.identity()
                );
                Ok(7)
            },
        )
        .unwrap_or_else(|_| panic!("success operation should complete"));
        assert_eq!(value, 7);
        assert_eq!(
            current_directory_identity().expect("restored cwd should inspect"),
            unrelated_identity
        );

        assert!(matches!(
            with_scoped_cwd(
                first_descriptor.as_raw_fd(),
                first.identity(),
                || -> Result<(), u8> { Err(9) },
            ),
            Err(ScopedCwdOperationError::Operation(9))
        ));
        assert_eq!(
            current_directory_identity().expect("error cwd should inspect"),
            unrelated_identity
        );

        let panic_result = std::panic::catch_unwind(|| {
            let _: Result<(), ScopedCwdOperationError<()>> = with_scoped_cwd(
                first_descriptor.as_raw_fd(),
                first.identity(),
                || -> Result<(), ()> { panic!("deliberate scoped panic") },
            );
        });
        assert!(panic_result.is_err());
        assert_eq!(
            current_directory_identity().expect("panic cwd should inspect"),
            unrelated_identity
        );

        assert!(matches!(
            with_scoped_cwd(
                first_descriptor.as_raw_fd(),
                second.identity(),
                || -> Result<(), ()> { Ok(()) },
            ),
            Err(ScopedCwdOperationError::Boundary(_))
        ));

        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for directory in [&first, &second] {
            let descriptor = directory.open();
            let identity = directory.identity();
            let active = Arc::clone(&active);
            let maximum = Arc::clone(&maximum);
            threads.push(thread::spawn(move || {
                with_scoped_cwd(descriptor.as_raw_fd(), identity, || -> Result<(), ()> {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(count, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(10));
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .unwrap_or_else(|_| panic!("concurrent operation should complete"));
            }));
        }
        for thread in threads {
            thread.join().expect("scoped thread should join");
        }
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
        assert_eq!(
            current_directory_identity().expect("concurrent cwd should inspect"),
            unrelated_identity
        );

        std::env::set_current_dir(original).expect("original cwd should restore");
    }
}
