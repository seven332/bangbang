use std::ffi::c_void;
use std::fmt;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{self, NonNull};

use crate::{BundleLayout, LauncherError};

/// Exact statically and dynamically validated worker entitlement profile.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum WorkerProfile {
    /// App Sandbox plus Hypervisor.framework, with no vmnet entitlement.
    Networkless,
    /// Exact production vmnet claims bound to one approved team and application identifier.
    Vmnet {
        application_identifier: String,
        team_identifier: String,
    },
}

impl fmt::Debug for WorkerProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Networkless => formatter.write_str("Networkless"),
            Self::Vmnet { .. } => formatter.write_str("Vmnet(<redacted>)"),
        }
    }
}

impl WorkerProfile {
    pub(crate) fn admits(&self, authority: bangbang_session::VmnetAuthority) -> bool {
        match self {
            Self::Networkless => authority.is_denied(),
            Self::Vmnet { .. } => !authority.is_denied(),
        }
    }
}

type CfIndex = isize;
type CfNumberType = CfIndex;
type CfStringEncoding = u32;
type CfTypeId = usize;
type OsStatus = i32;
type SecCsFlags = u32;

#[repr(C)]
struct CfDictionaryKeyCallbacks {
    fields: [usize; 6],
}

#[repr(C)]
struct CfDictionaryValueCallbacks {
    fields: [usize; 5],
}

const CF_STRING_ENCODING_UTF8: CfStringEncoding = 0x0800_0100;
const CF_NUMBER_SINT32_TYPE: CfNumberType = 3;
const CODE_SIGNATURE_RUNTIME: u32 = 0x0001_0000;
const SEC_CS_DEFAULT_FLAGS: SecCsFlags = 0;
const SEC_CS_CHECK_ALL_ARCHITECTURES: SecCsFlags = 1 << 0;
const SEC_CS_CHECK_NESTED_CODE: SecCsFlags = 1 << 3;
const SEC_CS_STRICT_VALIDATE: SecCsFlags = 1 << 4;
const SEC_CS_RESTRICT_SYMLINKS: SecCsFlags = 1 << 7;

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
    fn CFStringGetTypeID() -> CfTypeId;
    fn CFStringGetLength(string: *const c_void) -> CfIndex;
    fn CFStringGetCString(
        string: *const c_void,
        buffer: *mut i8,
        buffer_size: CfIndex,
        encoding: CfStringEncoding,
    ) -> u8;
    fn CFGetTypeID(value: *const c_void) -> CfTypeId;
    fn CFDictionaryGetCount(dictionary: *const c_void) -> CfIndex;
    fn CFDictionaryGetTypeID() -> CfTypeId;
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
    fn CFNumberGetTypeID() -> CfTypeId;
    fn CFNumberGetValue(number: *const c_void, number_type: CfNumberType, value: *mut c_void)
    -> u8;
    fn CFNumberCreate(
        allocator: *const c_void,
        number_type: CfNumberType,
        value: *const c_void,
    ) -> *const c_void;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: CfIndex,
        key_callbacks: *const CfDictionaryKeyCallbacks,
        value_callbacks: *const CfDictionaryValueCallbacks,
    ) -> *const c_void;
    fn CFBooleanGetTypeID() -> CfTypeId;
    fn CFBooleanGetValue(boolean: *const c_void) -> u8;
    fn CFRelease(value: *const c_void);
    static kCFTypeDictionaryKeyCallBacks: CfDictionaryKeyCallbacks;
    static kCFTypeDictionaryValueCallBacks: CfDictionaryValueCallbacks;
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
    fn SecCodeCopyGuestWithAttributes(
        host: *const c_void,
        attributes: *const c_void,
        flags: SecCsFlags,
        code: *mut *const c_void,
    ) -> OsStatus;
    fn SecCodeCheckValidity(
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
    static kSecGuestAttributePid: *const c_void;
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

pub(crate) fn validate_bundle(layout: &BundleLayout) -> Result<WorkerProfile, LauncherError> {
    let outer_requirement = requirement(&outer_requirement_text())?;
    let outer = static_code(layout.outer_bundle(), true)?;
    check(
        &outer,
        SEC_CS_CHECK_ALL_ARCHITECTURES
            | SEC_CS_CHECK_NESTED_CODE
            | SEC_CS_STRICT_VALIDATE
            | SEC_CS_RESTRICT_SYMLINKS,
        &outer_requirement,
    )?;
    if validate_entitlements(&outer, EntitlementProfile::Outer)?.is_some() {
        return Err(LauncherError::InvalidBundleSignature);
    }

    let worker_requirement = requirement(&worker_requirement_text())?;
    let worker = static_code(layout.worker_bundle(), true)?;
    check(
        &worker,
        SEC_CS_CHECK_ALL_ARCHITECTURES | SEC_CS_STRICT_VALIDATE | SEC_CS_RESTRICT_SYMLINKS,
        &worker_requirement,
    )?;
    let profile = validate_entitlements(&worker, EntitlementProfile::Worker)?
        .ok_or(LauncherError::InvalidBundleSignature)?;
    validate_embedded_profile(layout, &profile)?;
    Ok(profile)
}

pub(crate) fn validate_worker_process(pid: libc::pid_t) -> Result<WorkerProfile, LauncherError> {
    validate_process(
        pid,
        &worker_requirement_text(),
        EntitlementProfile::Worker,
        LauncherError::InvalidWorkerIdentity,
    )?
    .ok_or(LauncherError::InvalidWorkerIdentity)
}

pub(crate) fn validate_launcher_process(pid: libc::pid_t) -> Result<(), LauncherError> {
    let profile = validate_process(
        pid,
        &outer_requirement_text(),
        EntitlementProfile::Outer,
        LauncherError::InvalidBundleSignature,
    )?;
    if profile.is_none() {
        Ok(())
    } else {
        Err(LauncherError::InvalidBundleSignature)
    }
}

fn validate_process(
    pid: libc::pid_t,
    requirement_text: &str,
    profile: EntitlementProfile,
    failure: LauncherError,
) -> Result<Option<WorkerProfile>, LauncherError> {
    if pid <= 0 {
        return Err(failure);
    }
    let pid_value = pid;
    // SAFETY: `pid_value` remains live for the synchronous call and Core
    // Foundation copies it into a retained number.
    let pid_number = unsafe {
        CFNumberCreate(
            ptr::null(),
            CF_NUMBER_SINT32_TYPE,
            (&raw const pid_value).cast(),
        )
    };
    let pid_number = CfOwned(NonNull::new(pid_number.cast_mut()).ok_or(failure)?);
    // SAFETY: Security.framework exports this immutable CFString key.
    let pid_key = unsafe { kSecGuestAttributePid };
    if pid_key.is_null() {
        return Err(failure);
    }
    let keys = [pid_key];
    let values = [pid_number.as_ptr()];
    // SAFETY: Key/value arrays and exported callbacks remain live for this
    // synchronous creation, which retains their CFType contents.
    let attributes = unsafe {
        CFDictionaryCreate(
            ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    };
    let attributes = CfOwned(NonNull::new(attributes.cast_mut()).ok_or(failure)?);
    let mut code = ptr::null();
    // SAFETY: `attributes` is a retained CFDictionary and `code` is writable
    // storage for the retained dynamic SecCode result.
    if unsafe {
        SecCodeCopyGuestWithAttributes(
            ptr::null(),
            attributes.as_ptr(),
            SEC_CS_DEFAULT_FLAGS,
            &raw mut code,
        )
    } != 0
    {
        return Err(failure);
    }
    let code = CfOwned(NonNull::new(code.cast_mut()).ok_or(failure)?);
    let requirement = requirement(requirement_text).map_err(|_| failure)?;
    // SAFETY: `code` and `requirement` are live retained Security objects for
    // this synchronous dynamic validity check.
    if unsafe { SecCodeCheckValidity(code.as_ptr(), SEC_CS_DEFAULT_FLAGS, requirement.as_ptr()) }
        != 0
    {
        return Err(failure);
    }
    validate_entitlements(&code, profile).map_err(|_| failure)
}

fn outer_requirement_text() -> String {
    format!(
        "identifier \"{}\" and entitlement[\"{}\"] absent and entitlement[\"{}\"] absent",
        crate::LAUNCHER_BUNDLE_IDENTIFIER,
        crate::layout::APP_SANDBOX_ENTITLEMENT,
        crate::layout::HYPERVISOR_ENTITLEMENT
    )
}

fn worker_requirement_text() -> String {
    format!(
        "identifier \"{}\" and entitlement[\"{}\"] exists and entitlement[\"{}\"] exists",
        crate::WORKER_BUNDLE_IDENTIFIER,
        crate::layout::APP_SANDBOX_ENTITLEMENT,
        crate::layout::HYPERVISOR_ENTITLEMENT
    )
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

fn validate_entitlements(
    code: &CfOwned,
    profile: EntitlementProfile,
) -> Result<Option<WorkerProfile>, LauncherError> {
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
            EntitlementProfile::Outer => Ok(None),
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
        EntitlementProfile::Outer if count == 0 => Ok(None),
        EntitlementProfile::Outer => Err(LauncherError::InvalidBundleSignature),
        EntitlementProfile::Worker if count == 2 => {
            require_true_entitlement(entitlements, crate::layout::APP_SANDBOX_ENTITLEMENT)?;
            require_true_entitlement(entitlements, crate::layout::HYPERVISOR_ENTITLEMENT)?;
            classify_worker_profile(count, false, None, None).map(Some)
        }
        EntitlementProfile::Worker if count == 5 => {
            require_true_entitlement(entitlements, crate::layout::APP_SANDBOX_ENTITLEMENT)?;
            require_true_entitlement(entitlements, crate::layout::HYPERVISOR_ENTITLEMENT)?;
            require_true_entitlement(entitlements, crate::layout::VMNET_ENTITLEMENT)?;
            let application_identifier = require_string_entitlement(
                entitlements,
                crate::layout::APPLICATION_IDENTIFIER_ENTITLEMENT,
            )?;
            let team_identifier = require_string_entitlement(
                entitlements,
                crate::layout::TEAM_IDENTIFIER_ENTITLEMENT,
            )?;
            classify_worker_profile(
                count,
                true,
                Some(application_identifier),
                Some(team_identifier),
            )
            .map(Some)
        }
        EntitlementProfile::Worker => Err(LauncherError::InvalidBundleSignature),
    }
}

fn classify_worker_profile(
    count: CfIndex,
    vmnet: bool,
    application_identifier: Option<String>,
    team_identifier: Option<String>,
) -> Result<WorkerProfile, LauncherError> {
    match (count, vmnet, application_identifier, team_identifier) {
        (2, false, None, None) => Ok(WorkerProfile::Networkless),
        (5, true, Some(application_identifier), Some(team_identifier))
            if crate::provisioning_profile::valid_application_identifier(
                &application_identifier,
            ) && crate::provisioning_profile::valid_team_identifier(&team_identifier) =>
        {
            Ok(WorkerProfile::Vmnet {
                application_identifier,
                team_identifier,
            })
        }
        _ => Err(LauncherError::InvalidBundleSignature),
    }
}

fn validate_embedded_profile(
    layout: &BundleLayout,
    profile: &WorkerProfile,
) -> Result<(), LauncherError> {
    let path = layout.worker_provisioning_profile();
    match profile {
        WorkerProfile::Networkless => match fs::symlink_metadata(path) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) | Err(_) => Err(LauncherError::InvalidBundleSignature),
        },
        WorkerProfile::Vmnet { .. } => {
            let metadata =
                fs::symlink_metadata(path).map_err(|_| LauncherError::InvalidBundleSignature)?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() == 0
                || metadata.len()
                    > crate::provisioning_profile::MAX_PROVISIONING_PROFILE_BYTES as u64
            {
                return Err(LauncherError::InvalidBundleSignature);
            }
            Ok(())
        }
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

fn require_string_entitlement(
    dictionary: *const c_void,
    key: &str,
) -> Result<String, LauncherError> {
    const MAX_ENTITLEMENT_STRING_BYTES: usize = 128;

    let key = cf_string(key)?;
    // SAFETY: `dictionary` is a live CFDictionary checked by the caller and
    // `key` is a live CFString for this lookup.
    let value = unsafe { CFDictionaryGetValue(dictionary, key.as_ptr()) };
    if value.is_null() {
        return Err(LauncherError::InvalidBundleSignature);
    }
    // SAFETY: `value` is borrowed from the live dictionary, and the type-ID
    // query does not consume it; Core Foundation exports the string type ID.
    if unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return Err(LauncherError::InvalidBundleSignature);
    }
    // SAFETY: The type check establishes a live CFString for this bounded query.
    let length = unsafe { CFStringGetLength(value) };
    if length <= 0 || length as usize > MAX_ENTITLEMENT_STRING_BYTES {
        return Err(LauncherError::InvalidBundleSignature);
    }
    let mut buffer = [0_i8; MAX_ENTITLEMENT_STRING_BYTES + 1];
    // SAFETY: `value` is a live CFString and `buffer` is writable for the exact
    // capacity supplied, including the terminating NUL byte.
    if unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr(),
            buffer.len() as CfIndex,
            CF_STRING_ENCODING_UTF8,
        )
    } == 0
    {
        return Err(LauncherError::InvalidBundleSignature);
    }
    let bytes = buffer
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    if bytes.len() != length as usize {
        return Err(LauncherError::InvalidBundleSignature);
    }
    String::from_utf8(bytes).map_err(|_| LauncherError::InvalidBundleSignature)
}

#[cfg(test)]
mod tests {
    use crate::layout::{APP_SANDBOX_ENTITLEMENT, HYPERVISOR_ENTITLEMENT};

    use super::*;

    #[test]
    fn worker_requirement_uses_the_stable_identity_and_entitlements() {
        let worker = worker_requirement_text();
        let outer = outer_requirement_text();
        assert!(worker.contains(crate::WORKER_BUNDLE_IDENTIFIER));
        assert!(worker.contains(APP_SANDBOX_ENTITLEMENT));
        assert!(worker.contains(HYPERVISOR_ENTITLEMENT));
        assert!(worker.matches(" exists").count() == 2);
        assert!(outer.contains(crate::LAUNCHER_BUNDLE_IDENTIFIER));
        assert!(outer.matches(" absent").count() == 2);
    }

    #[test]
    fn static_requirements_compile() {
        requirement(&outer_requirement_text()).expect("outer requirement should compile");
        requirement(&worker_requirement_text()).expect("worker requirement should compile");
    }

    #[test]
    fn classifies_only_the_two_closed_worker_profiles() {
        assert_eq!(
            classify_worker_profile(2, false, None, None),
            Ok(WorkerProfile::Networkless)
        );
        assert_eq!(
            classify_worker_profile(
                5,
                true,
                Some("APPID12345.dev.bangbang.worker".to_owned()),
                Some("TEAM123456".to_owned()),
            ),
            Ok(WorkerProfile::Vmnet {
                application_identifier: "APPID12345.dev.bangbang.worker".to_owned(),
                team_identifier: "TEAM123456".to_owned(),
            })
        );
        for invalid in [
            classify_worker_profile(3, false, None, None),
            classify_worker_profile(5, false, None, None),
            classify_worker_profile(
                5,
                true,
                Some("APPID12345.dev.other".to_owned()),
                Some("TEAM123456".to_owned()),
            ),
            classify_worker_profile(
                5,
                true,
                Some("APPID12345.dev.bangbang.worker".to_owned()),
                Some("TEAM-12345".to_owned()),
            ),
        ] {
            assert_eq!(invalid, Err(LauncherError::InvalidBundleSignature));
        }
    }

    #[test]
    fn package_profile_admission_requires_matching_policy_presence() {
        let allowed = bangbang_session::VmnetAuthority::try_new(true, false, 1, &[])
            .expect("nonempty authority should construct");
        let denied = bangbang_session::VmnetAuthority::denied();
        let networkless = WorkerProfile::Networkless;
        let vmnet = WorkerProfile::Vmnet {
            application_identifier: "APPID12345.dev.bangbang.worker".to_owned(),
            team_identifier: "TEAM123456".to_owned(),
        };
        assert!(networkless.admits(denied));
        assert!(!networkless.admits(allowed));
        assert!(!vmnet.admits(denied));
        assert!(vmnet.admits(allowed));
    }

    #[test]
    fn vmnet_profile_debug_redacts_identity_values() {
        let profile = WorkerProfile::Vmnet {
            application_identifier: "PRIVATE123.dev.bangbang.worker".to_owned(),
            team_identifier: "SECRET1234".to_owned(),
        };
        let debug = format!("{profile:?}");
        assert_eq!(debug, "Vmnet(<redacted>)");
        assert!(!debug.contains("PRIVATE") && !debug.contains("SECRET"));
    }
}
