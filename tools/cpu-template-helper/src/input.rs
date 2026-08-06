//! Bounded path-redacted CPU-template helper input.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use rustix::fs::{FileType, Mode, OFlags, fstat, open};

use crate::CPU_TEMPLATE_DOCUMENT_MAX_BYTES;

/// Failure while reading one helper input document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    Open,
    Inspect,
    NotRegular,
    TooLarge,
    Read,
    InvalidUtf8,
}

impl fmt::Display for InputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Open => "helper input could not be opened safely",
            Self::Inspect => "helper input could not be inspected safely",
            Self::NotRegular => "helper input must be a regular file",
            Self::TooLarge => "helper input exceeds the size limit",
            Self::Read => "helper input could not be read",
            Self::InvalidUtf8 => "helper input is not UTF-8",
        })
    }
}

impl std::error::Error for InputError {}

/// Read at most one MiB from a no-follow regular file and validate UTF-8.
pub fn read_regular_utf8(path: &Path) -> Result<String, InputError> {
    read_regular_utf8_with_post_inspection(path, || {})
}

fn read_regular_utf8_with_post_inspection(
    path: &Path,
    post_inspection: impl FnOnce(),
) -> Result<String, InputError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| InputError::Open)?;
    let metadata = fstat(&descriptor).map_err(|_| InputError::Inspect)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
        return Err(InputError::NotRegular);
    }
    let size = usize::try_from(metadata.st_size).map_err(|_| InputError::Inspect)?;
    if size > CPU_TEMPLATE_DOCUMENT_MAX_BYTES {
        return Err(InputError::TooLarge);
    }
    post_inspection();

    let file = File::from(descriptor);
    let mut contents = Vec::with_capacity(size);
    file.take(CPU_TEMPLATE_DOCUMENT_MAX_BYTES as u64 + 1)
        .read_to_end(&mut contents)
        .map_err(|_| InputError::Read)?;
    if contents.len() > CPU_TEMPLATE_DOCUMENT_MAX_BYTES {
        return Err(InputError::TooLarge);
    }
    String::from_utf8(contents).map_err(|_| InputError::InvalidUtf8)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should follow epoch")
                .as_nanos();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-cpu-template-input-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should be created");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_boundaries_and_rejects_oversize_or_invalid_utf8() {
        let directory = TestDirectory::new();
        let empty = directory.0.join("empty");
        fs::write(&empty, []).expect("empty fixture should be written");
        assert_eq!(read_regular_utf8(&empty).as_deref(), Ok(""));

        let exact = directory.0.join("exact");
        fs::write(&exact, vec![b'a'; CPU_TEMPLATE_DOCUMENT_MAX_BYTES])
            .expect("fixture should be written");
        assert_eq!(
            read_regular_utf8(&exact)
                .expect("exact limit should read")
                .len(),
            CPU_TEMPLATE_DOCUMENT_MAX_BYTES
        );

        let oversized = directory.0.join("oversized");
        fs::write(&oversized, vec![b'a'; CPU_TEMPLATE_DOCUMENT_MAX_BYTES + 1])
            .expect("fixture should be written");
        assert_eq!(read_regular_utf8(&oversized), Err(InputError::TooLarge));

        let invalid = directory.0.join("invalid");
        fs::write(&invalid, [0xff]).expect("fixture should be written");
        assert_eq!(read_regular_utf8(&invalid), Err(InputError::InvalidUtf8));

        let growing = directory.0.join("growing");
        fs::write(&growing, vec![b'a'; CPU_TEMPLATE_DOCUMENT_MAX_BYTES])
            .expect("growing fixture should be written");
        assert_eq!(
            read_regular_utf8_with_post_inspection(&growing, || {
                let mut file = fs::OpenOptions::new()
                    .append(true)
                    .open(&growing)
                    .expect("growing fixture should reopen");
                file.write_all(b"x").expect("growing fixture should extend");
            }),
            Err(InputError::TooLarge)
        );
    }

    #[test]
    fn rejects_symlink_and_non_regular_paths_without_echoing_paths() {
        let directory = TestDirectory::new();
        let target = directory.0.join("target");
        fs::write(&target, b"{}").expect("fixture should be written");
        let link = directory.0.join("private-link-name");
        symlink(&target, &link).expect("symlink fixture should be created");
        let error = read_regular_utf8(&link).expect_err("symlink should fail closed");
        assert_eq!(error, InputError::Open);
        assert!(!error.to_string().contains("private-link-name"));

        assert_eq!(read_regular_utf8(&directory.0), Err(InputError::NotRegular));

        let fifo = directory.0.join("private-fifo-name");
        let status = Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo should be available on supported hosts");
        assert!(status.success(), "FIFO fixture should be created");
        let error = read_regular_utf8(&fifo).expect_err("FIFO should fail without blocking");
        assert_eq!(error, InputError::NotRegular);
        assert!(!error.to_string().contains("private-fifo-name"));
    }
}
