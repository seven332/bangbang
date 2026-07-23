use std::ffi::c_void;
use std::ptr::{self, NonNull};

use bangbang_runtime::memory::GuestMemoryRegion;

const MACH_OK: i32 = 0;
const MACH_INVALID: i32 = 1;
const MACH_NO_MEMORY: i32 = 2;
const MACH_OPERATION_FAILED: i32 = 3;
const MACH_OWNER_BUSY: i32 = 4;
const MACH_RESTORE_FAILED: i32 = 5;
const MACH_THREAD_FAILED: i32 = 6;

pub(crate) const MACH_FAULT_FORWARD: u32 = 0;
pub(crate) const MACH_FAULT_HANDLED: u32 = 1;
pub(crate) const MACH_FAULT_TERMINAL: u32 = 2;
pub(crate) const MACH_ACCESS_READ: u32 = 1;
pub(crate) const MACH_ACCESS_WRITE: u32 = 2;
pub(crate) const MACH_TERMINAL_EXIT_CODE: i32 = 70;

pub(crate) type MachFaultCallback =
    unsafe extern "C" fn(context: *mut c_void, address: u64, access: u32) -> u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MachLazyError {
    Invalid,
    Allocation,
    Operation,
    OwnerBusy,
    Restore,
    Thread,
}

#[repr(C)]
struct MachRegionInput {
    primary: *mut c_void,
    size: usize,
}

pub(crate) struct MachLazyMapping {
    raw: NonNull<c_void>,
}

impl MachLazyMapping {
    pub(crate) fn new(regions: &[GuestMemoryRegion]) -> Result<Self, MachLazyError> {
        imp::mapping_new(regions).map(|raw| Self { raw })
    }

    pub(crate) fn protect_all_none(&self) -> Result<(), MachLazyError> {
        imp::mapping_protect_all_none(self.raw)
    }

    pub(crate) fn restore_all_rw(&self) -> Result<(), MachLazyError> {
        imp::mapping_restore_all_rw(self.raw)
    }

    pub(crate) fn publish(
        &self,
        region_index: usize,
        offset: usize,
        contents: MachLazyContents<'_>,
        writable: bool,
    ) -> Result<(), MachLazyError> {
        imp::mapping_publish(self.raw, region_index, offset, contents, writable)
    }

    pub(crate) fn allow(
        &self,
        region_index: usize,
        offset: usize,
        length: usize,
        writable: bool,
    ) -> Result<(), MachLazyError> {
        imp::mapping_allow(self.raw, region_index, offset, length, writable)
    }

    pub(crate) fn hide(
        &self,
        region_index: usize,
        offset: usize,
        length: usize,
    ) -> Result<(), MachLazyError> {
        imp::mapping_hide(self.raw, region_index, offset, length)
    }
}

impl std::fmt::Debug for MachLazyMapping {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MachLazyMapping(<redacted>)")
    }
}

// SAFETY: the native owner retains immutable region metadata and independent
// aliases. Page mutation is serialized by LazyGuestMemory publication guards;
// permission calls are process-wide Mach operations and accept concurrent
// calls for disjoint pages.
unsafe impl Send for MachLazyMapping {}

// SAFETY: see the Send proof. Shared methods never mutate the native region
// table and the higher-level coordinator serializes same-page transitions.
unsafe impl Sync for MachLazyMapping {}

impl Drop for MachLazyMapping {
    fn drop(&mut self) {
        imp::mapping_destroy(self.raw);
    }
}

pub(crate) enum MachLazyContents<'a> {
    Data(&'a [u8]),
    Zero { length: usize },
}

pub(crate) struct MachExceptionOwner {
    raw: Option<NonNull<c_void>>,
}

impl MachExceptionOwner {
    pub(crate) fn install(
        context: NonNull<c_void>,
        callback: MachFaultCallback,
    ) -> Result<Self, MachLazyError> {
        imp::exception_install(context, callback).map(|raw| Self { raw: Some(raw) })
    }

    pub(crate) fn shutdown(&mut self) -> Result<bool, MachLazyError> {
        let raw = self.raw.ok_or(MachLazyError::Invalid)?;
        let restored = imp::exception_shutdown(raw)?;
        self.raw = None;
        Ok(restored)
    }
}

impl std::fmt::Debug for MachExceptionOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MachExceptionOwner(<redacted>)")
    }
}

// SAFETY: the native owner is process-global and internally serializes
// install/shutdown. Moving its unique Rust owner does not move callback state,
// whose address is separately pinned by the high-level Box.
unsafe impl Send for MachExceptionOwner {}

impl Drop for MachExceptionOwner {
    fn drop(&mut self) {
        let Some(raw) = self.raw else {
            return;
        };
        if imp::exception_shutdown(raw).is_err() {
            terminal_exit();
        }
        self.raw = None;
    }
}

pub(crate) fn is_supported_target() -> bool {
    imp::is_supported_target()
}

pub(crate) fn terminal_exit() -> ! {
    // SAFETY: `_exit` terminates the current process immediately and accepts
    // every i32 value. It does not return into Rust or run destructors.
    unsafe { libc::_exit(MACH_TERMINAL_EXIT_CODE) }
}

fn status_result(status: i32) -> Result<(), MachLazyError> {
    match status {
        MACH_OK => Ok(()),
        MACH_INVALID => Err(MachLazyError::Invalid),
        MACH_NO_MEMORY => Err(MachLazyError::Allocation),
        MACH_OPERATION_FAILED => Err(MachLazyError::Operation),
        MACH_OWNER_BUSY => Err(MachLazyError::OwnerBusy),
        MACH_RESTORE_FAILED => Err(MachLazyError::Restore),
        MACH_THREAD_FAILED => Err(MachLazyError::Thread),
        _ => Err(MachLazyError::Operation),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod imp {
    use super::{
        MachFaultCallback, MachLazyContents, MachLazyError, MachRegionInput, NonNull, c_void, ptr,
        status_result,
    };
    use bangbang_runtime::memory::GuestMemoryRegion;

    unsafe extern "C" {
        fn bangbang_mach_lazy_mapping_create(
            inputs: *const MachRegionInput,
            count: usize,
            output: *mut *mut c_void,
        ) -> i32;
        fn bangbang_mach_lazy_mapping_protect_all_none(mapping: *mut c_void) -> i32;
        fn bangbang_mach_lazy_mapping_restore_all_rw(mapping: *mut c_void) -> i32;
        fn bangbang_mach_lazy_mapping_allow(
            mapping: *mut c_void,
            region_index: usize,
            offset: usize,
            length: usize,
            writable: bool,
        ) -> i32;
        fn bangbang_mach_lazy_mapping_hide(
            mapping: *mut c_void,
            region_index: usize,
            offset: usize,
            length: usize,
        ) -> i32;
        fn bangbang_mach_lazy_mapping_publish(
            mapping: *mut c_void,
            region_index: usize,
            offset: usize,
            data: *const u8,
            length: usize,
            zero: bool,
            writable: bool,
        ) -> i32;
        fn bangbang_mach_lazy_mapping_destroy(mapping: *mut c_void);
        fn bangbang_mach_exception_owner_install(
            context: *mut c_void,
            callback: MachFaultCallback,
            output: *mut *mut c_void,
        ) -> i32;
        fn bangbang_mach_exception_owner_shutdown(owner: *mut c_void, restored: *mut bool) -> i32;
    }

    pub(super) fn mapping_new(
        regions: &[GuestMemoryRegion],
    ) -> Result<NonNull<c_void>, MachLazyError> {
        let mut inputs = Vec::new();
        inputs
            .try_reserve_exact(regions.len())
            .map_err(|_| MachLazyError::Allocation)?;
        inputs.extend(regions.iter().map(|region| MachRegionInput {
            primary: region.host_address().as_ptr(),
            size: region.host_size(),
        }));
        let mut output = ptr::null_mut();
        // SAFETY: `inputs` is retained for the call, the count matches its
        // initialized length, and `output` is writable. The native constructor
        // copies metadata and returns one owned opaque allocation on success.
        let status = unsafe {
            bangbang_mach_lazy_mapping_create(inputs.as_ptr(), inputs.len(), &mut output)
        };
        status_result(status)?;
        NonNull::new(output).ok_or(MachLazyError::Operation)
    }

    pub(super) fn mapping_protect_all_none(mapping: NonNull<c_void>) -> Result<(), MachLazyError> {
        // SAFETY: `mapping` is a live unique native owner retained by the
        // MachLazyMapping wrapper for this call.
        status_result(unsafe { bangbang_mach_lazy_mapping_protect_all_none(mapping.as_ptr()) })
    }

    pub(super) fn mapping_restore_all_rw(mapping: NonNull<c_void>) -> Result<(), MachLazyError> {
        // SAFETY: `mapping` is a live native owner and the call changes only
        // permissions of its retained original regions.
        status_result(unsafe { bangbang_mach_lazy_mapping_restore_all_rw(mapping.as_ptr()) })
    }

    pub(super) fn mapping_allow(
        mapping: NonNull<c_void>,
        region_index: usize,
        offset: usize,
        length: usize,
        writable: bool,
    ) -> Result<(), MachLazyError> {
        // SAFETY: the native owner validates the region, offset, length, and
        // host-page alignment before changing permissions.
        status_result(unsafe {
            bangbang_mach_lazy_mapping_allow(
                mapping.as_ptr(),
                region_index,
                offset,
                length,
                writable,
            )
        })
    }

    pub(super) fn mapping_hide(
        mapping: NonNull<c_void>,
        region_index: usize,
        offset: usize,
        length: usize,
    ) -> Result<(), MachLazyError> {
        // SAFETY: the native owner validates the exact retained range before
        // revoking its original mapping permissions.
        status_result(unsafe {
            bangbang_mach_lazy_mapping_hide(mapping.as_ptr(), region_index, offset, length)
        })
    }

    pub(super) fn mapping_publish(
        mapping: NonNull<c_void>,
        region_index: usize,
        offset: usize,
        contents: MachLazyContents<'_>,
        writable: bool,
    ) -> Result<(), MachLazyError> {
        let (data, length, zero) = match contents {
            MachLazyContents::Data(data) => (data.as_ptr(), data.len(), false),
            MachLazyContents::Zero { length } => (ptr::null(), length, true),
        };
        // SAFETY: data is retained for the call when non-null. The native
        // owner validates all indices and lengths, writes the retained alias,
        // fences, and opens only the matching original range.
        status_result(unsafe {
            bangbang_mach_lazy_mapping_publish(
                mapping.as_ptr(),
                region_index,
                offset,
                data,
                length,
                zero,
                writable,
            )
        })
    }

    pub(super) fn mapping_destroy(mapping: NonNull<c_void>) {
        // SAFETY: Drop calls this exactly once for the opaque native owner.
        unsafe { bangbang_mach_lazy_mapping_destroy(mapping.as_ptr()) }
    }

    pub(super) fn exception_install(
        context: NonNull<c_void>,
        callback: MachFaultCallback,
    ) -> Result<NonNull<c_void>, MachLazyError> {
        let mut output = ptr::null_mut();
        // SAFETY: the high-level bridge retains `context` until successful
        // shutdown joins the native server. `callback` has the declared ABI
        // and catches unwind before returning.
        let status = unsafe {
            bangbang_mach_exception_owner_install(context.as_ptr(), callback, &mut output)
        };
        status_result(status)?;
        NonNull::new(output).ok_or(MachLazyError::Operation)
    }

    pub(super) fn exception_shutdown(owner: NonNull<c_void>) -> Result<bool, MachLazyError> {
        let mut restored = false;
        // SAFETY: `owner` is live and uniquely shut down here; `restored` is a
        // writable out-parameter. Success joins the callback server before
        // freeing the native owner.
        status_result(unsafe {
            bangbang_mach_exception_owner_shutdown(owner.as_ptr(), &mut restored)
        })?;
        Ok(restored)
    }

    pub(super) const fn is_supported_target() -> bool {
        true
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod imp {
    use super::{MachFaultCallback, MachLazyContents, MachLazyError, NonNull, c_void};
    use bangbang_runtime::memory::GuestMemoryRegion;

    pub(super) fn mapping_new(
        _regions: &[GuestMemoryRegion],
    ) -> Result<NonNull<c_void>, MachLazyError> {
        Err(MachLazyError::Operation)
    }

    pub(super) fn mapping_protect_all_none(_mapping: NonNull<c_void>) -> Result<(), MachLazyError> {
        Err(MachLazyError::Operation)
    }

    pub(super) fn mapping_restore_all_rw(_mapping: NonNull<c_void>) -> Result<(), MachLazyError> {
        Err(MachLazyError::Operation)
    }

    pub(super) fn mapping_allow(
        _mapping: NonNull<c_void>,
        _region_index: usize,
        _offset: usize,
        _length: usize,
        _writable: bool,
    ) -> Result<(), MachLazyError> {
        Err(MachLazyError::Operation)
    }

    pub(super) fn mapping_hide(
        _mapping: NonNull<c_void>,
        _region_index: usize,
        _offset: usize,
        _length: usize,
    ) -> Result<(), MachLazyError> {
        Err(MachLazyError::Operation)
    }

    pub(super) fn mapping_publish(
        _mapping: NonNull<c_void>,
        _region_index: usize,
        _offset: usize,
        _contents: MachLazyContents<'_>,
        _writable: bool,
    ) -> Result<(), MachLazyError> {
        Err(MachLazyError::Operation)
    }

    pub(super) fn mapping_destroy(_mapping: NonNull<c_void>) {}

    pub(super) fn exception_install(
        _context: NonNull<c_void>,
        _callback: MachFaultCallback,
    ) -> Result<NonNull<c_void>, MachLazyError> {
        Err(MachLazyError::Operation)
    }

    pub(super) fn exception_shutdown(_owner: NonNull<c_void>) -> Result<bool, MachLazyError> {
        Err(MachLazyError::Operation)
    }

    pub(super) const fn is_supported_target() -> bool {
        false
    }
}
