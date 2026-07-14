use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub(crate) fn publish_exclusive(source: &Path, destination: &Path) -> Result<(), io::ErrorKind> {
    let source =
        CString::new(source.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| io::ErrorKind::InvalidInput)?;

    // SAFETY: Both C strings are NUL-terminated, remain alive for the call,
    // and name the private staging bundle and absent final path respectively.
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().kind())
    }
}
