//! Minimal Core Foundation bridge for one-session implicit file access.

use std::ffi::{CStr, c_void};
use std::fmt;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};

use crate::MAX_BOOKMARK_BYTES;

type CFAllocatorRef = *const c_void;
type CFArrayRef = *const c_void;
type CFDataRef = *const c_void;
type CFErrorRef = *const c_void;
type CFTypeRef = *const c_void;
type CFURLRef = *const c_void;
type CFIndex = isize;
type CFOptionFlags = usize;
type Boolean = u8;

const RESOLVE_WITHOUT_UI: CFOptionFlags = 1 << 8;
const RESOLVE_WITHOUT_MOUNTING: CFOptionFlags = 1 << 9;
const RESOLVE_WITHOUT_IMPLICIT_START: CFOptionFlags = 1 << 15;
const MAX_FILESYSTEM_PATH_BYTES: usize = 4096;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFAllocatorRef,
        bytes: *const u8,
        length: CFIndex,
        is_directory: Boolean,
    ) -> CFURLRef;
    fn CFURLCreateBookmarkData(
        allocator: CFAllocatorRef,
        url: CFURLRef,
        options: CFOptionFlags,
        properties: CFArrayRef,
        relative_url: CFURLRef,
        error: *mut CFErrorRef,
    ) -> CFDataRef;
    fn CFURLCreateByResolvingBookmarkData(
        allocator: CFAllocatorRef,
        bookmark: CFDataRef,
        options: CFOptionFlags,
        relative_url: CFURLRef,
        properties: CFArrayRef,
        is_stale: *mut Boolean,
        error: *mut CFErrorRef,
    ) -> CFURLRef;
    fn CFURLGetFileSystemRepresentation(
        url: CFURLRef,
        resolve_against_base: Boolean,
        buffer: *mut u8,
        max_buffer_length: CFIndex,
    ) -> Boolean;
    fn CFURLStartAccessingSecurityScopedResource(url: CFURLRef) -> Boolean;
    fn CFURLStopAccessingSecurityScopedResource(url: CFURLRef);
    fn CFDataCreate(allocator: CFAllocatorRef, bytes: *const u8, length: CFIndex) -> CFDataRef;
    fn CFDataGetLength(data: CFDataRef) -> CFIndex;
    fn CFDataGetBytePtr(data: CFDataRef) -> *const u8;
    fn CFRelease(value: CFTypeRef);
}

/// Redacted bookmark bridge failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookmarkError;

impl fmt::Display for BookmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private resource bookmark failure")
    }
}

impl std::error::Error for BookmarkError {}

/// Creates bounded ordinary bookmark data for one existing filesystem object.
pub fn create_implicit_bookmark(path: &Path, is_directory: bool) -> Result<Vec<u8>, BookmarkError> {
    let path = path.as_os_str().as_bytes();
    if path.is_empty() || path.len() > MAX_FILESYSTEM_PATH_BYTES || path.contains(&0) {
        return Err(BookmarkError);
    }
    let path_len = CFIndex::try_from(path.len()).map_err(|_| BookmarkError)?;
    // SAFETY: The path bytes remain live for this synchronous Core Foundation
    // call and are not retained except through the returned owned URL.
    let url = unsafe {
        CFURLCreateFromFileSystemRepresentation(
            ptr::null(),
            path.as_ptr(),
            path_len,
            Boolean::from(is_directory),
        )
    };
    let url = CfOwned::new(url).ok_or(BookmarkError)?;
    let mut error = ptr::null();
    // Options zero deliberately requests an ordinary implicit one-session
    // bookmark, not persistent security scope.
    // SAFETY: All object pointers are valid or null by the Core Foundation API.
    let data = unsafe {
        CFURLCreateBookmarkData(
            ptr::null(),
            url.as_ptr(),
            0,
            ptr::null(),
            ptr::null(),
            &raw mut error,
        )
    };
    let error = CfOwned::new(error);
    let data = CfOwned::new(data);
    if error.is_some() {
        return Err(BookmarkError);
    }
    let data = data.ok_or(BookmarkError)?;
    // SAFETY: data is a live CFData object.
    let length = unsafe { CFDataGetLength(data.as_ptr()) };
    let length = usize::try_from(length).map_err(|_| BookmarkError)?;
    if length == 0 || length > usize::try_from(MAX_BOOKMARK_BYTES).map_err(|_| BookmarkError)? {
        return Err(BookmarkError);
    }
    // SAFETY: A nonempty CFData exposes exactly length readable bytes while data
    // remains owned by this function.
    let bytes = unsafe { CFDataGetBytePtr(data.as_ptr()) };
    if bytes.is_null() {
        return Err(BookmarkError);
    }
    // SAFETY: Core Foundation guarantees the pointer covers the checked length.
    Ok(unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec())
}

/// Active one-session implicit bookmark scope.
pub(crate) struct ScopedBookmark {
    url: CfOwned,
    path: PathBuf,
    stale: bool,
}

impl ScopedBookmark {
    /// Resolves bounded data, defers implicit start, then explicitly starts scope.
    pub(crate) fn resolve(bytes: &[u8]) -> Result<Self, BookmarkError> {
        if bytes.is_empty()
            || bytes.len() > usize::try_from(MAX_BOOKMARK_BYTES).map_err(|_| BookmarkError)?
        {
            return Err(BookmarkError);
        }
        let length = CFIndex::try_from(bytes.len()).map_err(|_| BookmarkError)?;
        // SAFETY: The byte slice remains live for the synchronous copy into the
        // returned Core Foundation data object.
        let data = unsafe { CFDataCreate(ptr::null(), bytes.as_ptr(), length) };
        let data = CfOwned::new(data).ok_or(BookmarkError)?;
        let mut stale = 0;
        let mut error = ptr::null();
        // SAFETY: All pointers are live or null and output storage is writable.
        let url = unsafe {
            CFURLCreateByResolvingBookmarkData(
                ptr::null(),
                data.as_ptr(),
                RESOLVE_WITHOUT_UI | RESOLVE_WITHOUT_MOUNTING | RESOLVE_WITHOUT_IMPLICIT_START,
                ptr::null(),
                ptr::null(),
                &raw mut stale,
                &raw mut error,
            )
        };
        let error = CfOwned::new(error);
        let url = CfOwned::new(url);
        if error.is_some() {
            return Err(BookmarkError);
        }
        let url = url.ok_or(BookmarkError)?;
        // SAFETY: url is a live resolved URL. A true result owns one scope
        // reference that ScopedBookmark balances in Drop.
        if unsafe { CFURLStartAccessingSecurityScopedResource(url.as_ptr()) } == 0 {
            return Err(BookmarkError);
        }
        let path = match filesystem_path(&url) {
            Ok(path) => path,
            Err(error) => {
                // SAFETY: The successful start above owns exactly one reference.
                unsafe { CFURLStopAccessingSecurityScopedResource(url.as_ptr()) };
                return Err(error);
            }
        };
        Ok(Self {
            url,
            path,
            stale: stale != 0,
        })
    }

    /// Returns the resolved path while this scope remains active.
    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the platform stale indication without treating it as invalidity.
    #[must_use]
    pub(crate) const fn is_stale(&self) -> bool {
        self.stale
    }
}

impl fmt::Debug for ScopedBookmark {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScopedBookmark")
            .field("path", &"<redacted>")
            .field("stale", &"<redacted>")
            .finish()
    }
}

impl Drop for ScopedBookmark {
    fn drop(&mut self) {
        // SAFETY: Construction succeeded only after exactly one successful
        // start call; this Drop balances it once before the URL is released.
        unsafe { CFURLStopAccessingSecurityScopedResource(self.url.as_ptr()) };
    }
}

fn filesystem_path(url: &CfOwned) -> Result<PathBuf, BookmarkError> {
    let mut bytes = vec![0_u8; MAX_FILESYSTEM_PATH_BYTES + 1];
    let max = CFIndex::try_from(bytes.len()).map_err(|_| BookmarkError)?;
    // SAFETY: bytes is writable for max bytes and url is live.
    if unsafe { CFURLGetFileSystemRepresentation(url.as_ptr(), 1, bytes.as_mut_ptr(), max) } == 0 {
        return Err(BookmarkError);
    }
    let path = CStr::from_bytes_until_nul(&bytes).map_err(|_| BookmarkError)?;
    if path.to_bytes().is_empty() || path.to_bytes().len() > MAX_FILESYSTEM_PATH_BYTES {
        return Err(BookmarkError);
    }
    Ok(PathBuf::from(std::ffi::OsString::from_vec(
        path.to_bytes().to_vec(),
    )))
}

struct CfOwned(NonNull<c_void>);

impl CfOwned {
    fn new(value: *const c_void) -> Option<Self> {
        NonNull::new(value.cast_mut()).map(Self)
    }

    fn as_ptr(&self) -> *const c_void {
        self.0.as_ptr()
    }
}

impl Drop for CfOwned {
    fn drop(&mut self) {
        // SAFETY: CfOwned is constructed only from a create/copy result and owns
        // exactly one Core Foundation reference.
        unsafe { CFRelease(self.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[test]
    fn ordinary_directory_bookmark_round_trips_without_path_debug() {
        let directory =
            std::env::temp_dir().join(format!("bangbang-bookmark-test-{}", std::process::id()));
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("test directory should create: {error}"),
        }
        let bytes = create_implicit_bookmark(&directory, true).expect("bookmark should be created");
        let scoped = ScopedBookmark::resolve(&bytes).expect("bookmark should resolve");
        let expected = fs::metadata(&directory).expect("source metadata should read");
        let resolved = fs::metadata(scoped.path()).expect("resolved metadata should read");
        assert_eq!(
            (resolved.dev(), resolved.ino()),
            (expected.dev(), expected.ino())
        );
        assert!(!format!("{scoped:?}").contains(directory.to_string_lossy().as_ref()));
        drop(scoped);
        fs::remove_dir(&directory).expect("test directory should clean up");
    }
}
