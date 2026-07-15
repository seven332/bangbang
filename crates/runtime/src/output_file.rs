//! Descriptor-native output-file preparation shared by runtime sinks.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, RawFd};

/// Adopts an authority-provided regular file as an append-only nonblocking sink.
///
/// The descriptor must already carry exact write-only authority. Status changes
/// never upgrade that access mode.
pub(crate) fn adopt_write_only_file(file: File) -> Result<File, io::ErrorKind> {
    let metadata = file.metadata().map_err(|error| error.kind())?;
    if !metadata.file_type().is_file() {
        return Err(io::ErrorKind::InvalidInput);
    }

    let descriptor = file.as_raw_fd();
    let flags = descriptor_flags(descriptor)?;
    if flags & libc::O_ACCMODE != libc::O_WRONLY {
        return Err(io::ErrorKind::PermissionDenied);
    }

    let required = libc::O_APPEND | libc::O_NONBLOCK;
    // SAFETY: `descriptor` is borrowed from the live owned `File`; `F_SETFL`
    // changes only status flags on that open file description.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | required) } < 0 {
        return Err(io::Error::last_os_error().kind());
    }

    let normalized = descriptor_flags(descriptor)?;
    if normalized & libc::O_ACCMODE != libc::O_WRONLY || normalized & required != required {
        return Err(io::ErrorKind::Other);
    }

    Ok(file)
}

fn descriptor_flags(descriptor: RawFd) -> Result<libc::c_int, io::ErrorKind> {
    // SAFETY: `descriptor` is borrowed from a live owned `File`; `F_GETFL`
    // performs no pointer access or ownership transfer.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        Err(io::Error::last_os_error().kind())
    } else {
        Ok(flags)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::adopt_write_only_file;

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should follow epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "bangbang-output-file-{name}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should create");
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn adopts_write_only_regular_file_with_append_and_nonblocking_status() {
        let root = TestDir::new("flags");
        let path = root.path.join("output");
        fs::write(&path, b"before").expect("fixture should write");
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("write-only file should open");

        let mut file = adopt_write_only_file(file).expect("file should adopt");
        // SAFETY: the descriptor is borrowed from the live adopted file.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(flags & libc::O_ACCMODE, libc::O_WRONLY);
        assert_ne!(flags & libc::O_APPEND, 0);
        assert_ne!(flags & libc::O_NONBLOCK, 0);
        file.write_all(b"-after")
            .expect("adopted file should write");
        drop(file);

        assert_eq!(
            fs::read(&path).expect("output should read"),
            b"before-after"
        );
    }

    #[test]
    fn rejects_read_write_and_read_only_files_without_upgrading_access() {
        let root = TestDir::new("access");
        let path = root.path.join("output");
        fs::write(&path, b"value").expect("fixture should write");

        let read_write = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("read-write file should open");
        assert_eq!(
            adopt_write_only_file(read_write).expect_err("read-write access should reject"),
            std::io::ErrorKind::PermissionDenied
        );

        let mut read_only = File::open(&path).expect("read-only file should open");
        assert_eq!(
            adopt_write_only_file(read_only.try_clone().expect("file should clone"))
                .expect_err("read-only access should reject"),
            std::io::ErrorKind::PermissionDenied
        );
        let mut value = String::new();
        read_only
            .read_to_string(&mut value)
            .expect("original access should remain readable");
        assert_eq!(value, "value");
    }

    #[test]
    fn rejects_non_regular_descriptor() {
        let root = TestDir::new("type");
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(&root.path)
            .expect("directory should open");
        assert_eq!(
            adopt_write_only_file(directory).expect_err("directory should reject"),
            std::io::ErrorKind::InvalidInput
        );
    }
}
