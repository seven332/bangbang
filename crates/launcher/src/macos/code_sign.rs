use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{self, NonNull};

use crate::{BundleLayout, LauncherError};

type CfIndex = isize;
type CfNumberType = CfIndex;
type CfStringEncoding = u32;
type CfTypeId = usize;
type OsStatus = i32;
type SecCsFlags = u32;

const CF_STRING_ENCODING_UTF8: CfStringEncoding = 0x0800_0100;
const CF_NUMBER_SINT32_TYPE: CfNumberType = 3;
const CODE_SIGNATURE_RUNTIME: u32 = 0x0001_0000;
const SEC_CS_DEFAULT_FLAGS: SecCsFlags = 0;
const SEC_CS_CHECK_ALL_ARCHITECTURES: SecCsFlags = 1 << 0;
const SEC_CS_CHECK_NESTED_CODE: SecCsFlags = 1 << 3;
const SEC_CS_STRICT_VALIDATE: SecCsFlags = 1 << 4;
const SEC_CS_RESTRICT_SYMLINKS: SecCsFlags = 1 << 7;

const OUTER_REQUIREMENT: &str = "identifier \"dev.bangbang\" and entitlement[\"com.apple.security.app-sandbox\"] absent and entitlement[\"com.apple.security.hypervisor\"] absent";
const WORKER_REQUIREMENT: &str = "identifier \"dev.bangbang.worker\" and entitlement[\"com.apple.security.app-sandbox\"] exists and entitlement[\"com.apple.security.hypervisor\"] exists";

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: *const c_void,
        buffer: *const u8,
        buffer_length: CfIndex,
        is_directory: u8,
    ) -> *const c_void;
    fn CFStringCreateWithBytes(
        allocator: *const c_void,
        bytes: *const u8,
        byte_count: CfIndex,
        encoding: CfStringEncoding,
        is_external_representation: u8,
    ) -> *const c_void;
    fn CFGetTypeID(value: *const c_void) -> CfTypeId;
    fn CFDictionaryGetCount(dictionary: *const c_void) -> CfIndex;
    fn CFDictionaryGetTypeID() -> CfTypeId;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetTypeID() -> CfTypeId;
    fn CFNumberGetValue(number: *const c_void, number_type: CfNumberType, value: *mut c_void)
    -> u8;
    fn CFBooleanGetTypeID() -> CfTypeId;
    fn CFBooleanGetValue(boolean: *const c_void) -> u8;
    fn CFRelease(value: *const c_void);
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecStaticCodeCreateWithPath(
        path: *const c_void,
        flags: SecCsFlags,
        code: *mut *const c_void,
    ) -> OsStatus;
    fn SecStaticCodeCheckValidity(
        code: *const c_void,
        flags: SecCsFlags,
        requirement: *const c_void,
    ) -> OsStatus;
    fn SecRequirementCreateWithString(
        text: *const c_void,
        flags: SecCsFlags,
        requirement: *mut *const c_void,
    ) -> OsStatus;
    fn SecCodeCopySigningInformation(
        code: *const c_void,
        flags: SecCsFlags,
        information: *mut *const c_void,
    ) -> OsStatus;
    static kSecCodeInfoEntitlementsDict: *const c_void;
    static kSecCodeInfoFlags: *const c_void;
}

#[derive(Debug)]
struct CfOwned(NonNull<c_void>);

impl CfOwned {
    fn as_ptr(&self) -> *const c_void {
        self.0.as_ptr()
    }
}

impl Drop for CfOwned {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a retained Core Foundation or Security object
        // returned by a Create function and is released exactly once here.
        unsafe { CFRelease(self.as_ptr()) };
    }
}

pub(crate) fn validate_bundle(layout: &BundleLayout) -> Result<(), LauncherError> {
    let outer_requirement = requirement(OUTER_REQUIREMENT)?;
    let outer = static_code(layout.outer_bundle(), true)?;
    check(
        &outer,
        SEC_CS_CHECK_ALL_ARCHITECTURES
            | SEC_CS_CHECK_NESTED_CODE
            | SEC_CS_STRICT_VALIDATE
            | SEC_CS_RESTRICT_SYMLINKS,
        &outer_requirement,
    )?;
    validate_entitlements(&outer, EntitlementProfile::Outer)?;

    let worker_requirement = requirement(WORKER_REQUIREMENT)?;
    let worker = static_code(layout.worker_bundle(), true)?;
    check(
        &worker,
        SEC_CS_CHECK_ALL_ARCHITECTURES | SEC_CS_STRICT_VALIDATE | SEC_CS_RESTRICT_SYMLINKS,
        &worker_requirement,
    )?;
    validate_entitlements(&worker, EntitlementProfile::Worker)
}

fn static_code(path: &Path, is_directory: bool) -> Result<CfOwned, LauncherError> {
    let path_bytes = path.as_os_str().as_bytes();
    let path_length =
        CfIndex::try_from(path_bytes.len()).map_err(|_| LauncherError::InvalidBundleSignature)?;
    // SAFETY: `path_bytes` remains valid for the call, its exact byte length is
    // supplied, and a null allocator selects the default Core Foundation allocator.
    let url = unsafe {
        CFURLCreateFromFileSystemRepresentation(
            ptr::null(),
            path_bytes.as_ptr(),
            path_length,
            u8::from(is_directory),
        )
    };
    let url = CfOwned(NonNull::new(url.cast_mut()).ok_or(LauncherError::InvalidBundleSignature)?);
    let mut code = ptr::null();
    // SAFETY: `url` is a valid retained CFURL and `code` points to writable
    // storage for the retained SecStaticCode result.
    let status =
        unsafe { SecStaticCodeCreateWithPath(url.as_ptr(), SEC_CS_DEFAULT_FLAGS, &mut code) };
    if status != 0 {
        return Err(LauncherError::InvalidBundleSignature);
    }
    Ok(CfOwned(
        NonNull::new(code.cast_mut()).ok_or(LauncherError::InvalidBundleSignature)?,
    ))
}

fn requirement(text: &str) -> Result<CfOwned, LauncherError> {
    let string = cf_string(text)?;
    let mut requirement = ptr::null();
    // SAFETY: `string` is a valid retained CFString and `requirement` points to
    // writable storage for the retained SecRequirement result.
    let status = unsafe {
        SecRequirementCreateWithString(string.as_ptr(), SEC_CS_DEFAULT_FLAGS, &mut requirement)
    };
    if status != 0 {
        return Err(LauncherError::InvalidBundleSignature);
    }
    Ok(CfOwned(
        NonNull::new(requirement.cast_mut()).ok_or(LauncherError::InvalidBundleSignature)?,
    ))
}

fn cf_string(text: &str) -> Result<CfOwned, LauncherError> {
    let text_length =
        CfIndex::try_from(text.len()).map_err(|_| LauncherError::InvalidBundleSignature)?;
    // SAFETY: `text` is UTF-8 and remains valid for the call; Core Foundation
    // copies the bytes into a retained string.
    let string = unsafe {
        CFStringCreateWithBytes(
            ptr::null(),
            text.as_ptr(),
            text_length,
            CF_STRING_ENCODING_UTF8,
            0,
        )
    };
    Ok(CfOwned(
        NonNull::new(string.cast_mut()).ok_or(LauncherError::InvalidBundleSignature)?,
    ))
}

fn check(code: &CfOwned, flags: SecCsFlags, requirement: &CfOwned) -> Result<(), LauncherError> {
    // SAFETY: `code` and `requirement` are valid retained Security objects for
    // the duration of this synchronous validation call.
    let status = unsafe { SecStaticCodeCheckValidity(code.as_ptr(), flags, requirement.as_ptr()) };
    if status == 0 {
        Ok(())
    } else {
        Err(LauncherError::InvalidBundleSignature)
    }
}

#[derive(Debug, Clone, Copy)]
enum EntitlementProfile {
    Outer,
    Worker,
}

fn validate_entitlements(code: &CfOwned, profile: EntitlementProfile) -> Result<(), LauncherError> {
    let mut information = ptr::null();
    // SAFETY: `code` is a valid retained static-code object and `information`
    // points to writable storage for the retained signing-information result.
    let status = unsafe {
        SecCodeCopySigningInformation(code.as_ptr(), SEC_CS_DEFAULT_FLAGS, &mut information)
    };
    if status != 0 {
        return Err(LauncherError::InvalidBundleSignature);
    }
    let information =
        CfOwned(NonNull::new(information.cast_mut()).ok_or(LauncherError::InvalidBundleSignature)?);
    require_hardened_runtime(&information)?;
    // SAFETY: Security.framework exports this immutable non-null CFString key.
    let entitlements_key = unsafe { kSecCodeInfoEntitlementsDict };
    if entitlements_key.is_null() {
        return Err(LauncherError::InvalidBundleSignature);
    }
    // SAFETY: `information` is the CFDictionary returned by
    // `SecCodeCopySigningInformation`; the borrowed value remains live while
    // that retained dictionary is held.
    let entitlements = unsafe { CFDictionaryGetValue(information.as_ptr(), entitlements_key) };
    if entitlements.is_null() {
        return match profile {
            EntitlementProfile::Outer => Ok(()),
            EntitlementProfile::Worker => Err(LauncherError::InvalidBundleSignature),
        };
    }
    // SAFETY: `entitlements` is a live borrowed CF object from `information`.
    if unsafe { CFGetTypeID(entitlements) } != unsafe { CFDictionaryGetTypeID() } {
        return Err(LauncherError::InvalidBundleSignature);
    }
    // SAFETY: The type check above establishes a live CFDictionary.
    let count = unsafe { CFDictionaryGetCount(entitlements) };
    match profile {
        EntitlementProfile::Outer if count == 0 => Ok(()),
        EntitlementProfile::Outer => Err(LauncherError::InvalidBundleSignature),
        EntitlementProfile::Worker if count == 2 => {
            require_true_entitlement(entitlements, crate::layout::APP_SANDBOX_ENTITLEMENT)?;
            require_true_entitlement(entitlements, crate::layout::HYPERVISOR_ENTITLEMENT)
        }
        EntitlementProfile::Worker => Err(LauncherError::InvalidBundleSignature),
    }
}

fn require_hardened_runtime(information: &CfOwned) -> Result<(), LauncherError> {
    // SAFETY: Security.framework exports this immutable non-null CFString key.
    let flags_key = unsafe { kSecCodeInfoFlags };
    if flags_key.is_null() {
        return Err(LauncherError::InvalidBundleSignature);
    }
    // SAFETY: `information` is the live CFDictionary returned by
    // `SecCodeCopySigningInformation`; the borrowed value remains live while
    // that retained dictionary is held.
    let flags = unsafe { CFDictionaryGetValue(information.as_ptr(), flags_key) };
    if flags.is_null() {
        return Err(LauncherError::InvalidBundleSignature);
    }
    // SAFETY: `flags` is a live borrowed CF object from `information`.
    if unsafe { CFGetTypeID(flags) } != unsafe { CFNumberGetTypeID() } {
        return Err(LauncherError::InvalidBundleSignature);
    }
    let mut value = 0_i32;
    // SAFETY: The type check above establishes a live CFNumber, and `value`
    // points to writable storage of the requested signed 32-bit type.
    if unsafe {
        CFNumberGetValue(
            flags,
            CF_NUMBER_SINT32_TYPE,
            (&raw mut value).cast::<c_void>(),
        )
    } == 0
        || (value as u32) & CODE_SIGNATURE_RUNTIME == 0
    {
        return Err(LauncherError::InvalidBundleSignature);
    }
    Ok(())
}

fn require_true_entitlement(dictionary: *const c_void, key: &str) -> Result<(), LauncherError> {
    let key = cf_string(key)?;
    // SAFETY: `dictionary` is a live CFDictionary checked by the caller and
    // `key` is a live CFString for this lookup.
    let value = unsafe { CFDictionaryGetValue(dictionary, key.as_ptr()) };
    if value.is_null() {
        return Err(LauncherError::InvalidBundleSignature);
    }
    // SAFETY: `value` is borrowed from the live dictionary. Short-circuiting
    // calls `CFBooleanGetValue` only after its Core Foundation type is confirmed.
    let is_true =
        unsafe { CFGetTypeID(value) == CFBooleanGetTypeID() && CFBooleanGetValue(value) != 0 };
    if !is_true {
        return Err(LauncherError::InvalidBundleSignature);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::layout::{APP_SANDBOX_ENTITLEMENT, HYPERVISOR_ENTITLEMENT};

    use super::*;

    #[test]
    fn worker_requirement_uses_the_stable_identity_and_entitlements() {
        assert!(WORKER_REQUIREMENT.contains(crate::WORKER_BUNDLE_IDENTIFIER));
        assert!(WORKER_REQUIREMENT.contains(APP_SANDBOX_ENTITLEMENT));
        assert!(WORKER_REQUIREMENT.contains(HYPERVISOR_ENTITLEMENT));
        assert!(WORKER_REQUIREMENT.matches(" exists").count() == 2);
        assert!(OUTER_REQUIREMENT.contains(crate::LAUNCHER_BUNDLE_IDENTIFIER));
        assert!(OUTER_REQUIREMENT.matches(" absent").count() == 2);
    }

    #[test]
    fn static_requirements_compile() {
        requirement(OUTER_REQUIREMENT).expect("outer requirement should compile");
        requirement(WORKER_REQUIREMENT).expect("worker requirement should compile");
    }
}
