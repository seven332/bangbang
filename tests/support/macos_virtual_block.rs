// Shared by signed macOS block-device integration targets.
#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Cursor;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use plist::{Dictionary, Value};

const HDIUTIL: &str = "/usr/bin/hdiutil";
const IMAGE_SIZE_ARGUMENT: &str = "4m";
const EXPECTED_IMAGE_BYTES: u64 = 4 * 1024 * 1024;
const DEVICE_PREFIX: &str = "/dev/disk";
const VIRTIO_SECTOR_BYTES: u64 = 512;
const DARWIN_IOC_OUT: libc::c_ulong = 0x4000_0000;
const DARWIN_IOC_VOID: libc::c_ulong = 0x2000_0000;
const DARWIN_IOCPARM_MASK: libc::c_ulong = 0x1fff;

const fn darwin_ioctl_request(
    direction: libc::c_ulong,
    group: u8,
    number: u8,
    len: usize,
) -> libc::c_ulong {
    direction
        | (((len as libc::c_ulong) & DARWIN_IOCPARM_MASK) << 16)
        | ((group as libc::c_ulong) << 8)
        | number as libc::c_ulong
}

const DKIOCGETBLOCKSIZE: libc::c_ulong =
    darwin_ioctl_request(DARWIN_IOC_OUT, b'd', 24, std::mem::size_of::<u32>());
const DKIOCGETBLOCKCOUNT: libc::c_ulong =
    darwin_ioctl_request(DARWIN_IOC_OUT, b'd', 25, std::mem::size_of::<u64>());
const DKIOCSYNCHRONIZECACHE: libc::c_ulong = darwin_ioctl_request(DARWIN_IOC_VOID, b'd', 22, 0);

static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MacosVirtualBlockError;

impl fmt::Display for MacosVirtualBlockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("temporary virtual block media failure")
    }
}

impl std::error::Error for MacosVirtualBlockError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MacosVirtualBlockAccess {
    ReadOnly,
    ReadWrite,
}

impl MacosVirtualBlockAccess {
    const fn open_flags(self) -> libc::c_int {
        match self {
            Self::ReadOnly => libc::O_RDONLY,
            Self::ReadWrite => libc::O_RDWR,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct MacosVirtualBlockIdentity {
    device: u64,
    inode: u64,
    target_device: u64,
}

impl MacosVirtualBlockIdentity {
    pub(crate) const fn device(self) -> u64 {
        self.device
    }

    pub(crate) const fn inode(self) -> u64 {
        self.inode
    }

    pub(crate) const fn target_device(self) -> u64 {
        self.target_device
    }
}

impl fmt::Debug for MacosVirtualBlockIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MacosVirtualBlockIdentity(<redacted>)")
    }
}

#[derive(Clone)]
struct Attachment {
    device_path: PathBuf,
    access: MacosVirtualBlockAccess,
    identity: MacosVirtualBlockIdentity,
    logical_block_size: u32,
    block_count: u64,
}

impl Attachment {
    fn len(&self) -> Option<u64> {
        u64::from(self.logical_block_size).checked_mul(self.block_count)
    }
}

pub(crate) struct MacosVirtualBlock {
    directory: PathBuf,
    image: PathBuf,
    attachment: Option<Attachment>,
    attachment_uncertain: bool,
    cleaned: bool,
}

impl fmt::Debug for MacosVirtualBlock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MacosVirtualBlock")
            .field("storage", &"<redacted>")
            .field(
                "attachment",
                &self.attachment.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl MacosVirtualBlock {
    pub(crate) fn create(access: MacosVirtualBlockAccess) -> Result<Self, MacosVirtualBlockError> {
        let directory = unique_fixture_directory()?;
        let image = directory.join("media.dmg");
        let output = run_hdiutil(&[
            OsString::from("create"),
            OsString::from("-size"),
            OsString::from(IMAGE_SIZE_ARGUMENT),
            OsString::from("-layout"),
            OsString::from("NONE"),
            image.as_os_str().to_os_string(),
        ]);
        if output.is_err() {
            let _ = fs::remove_file(&image);
            let _ = fs::remove_dir(&directory);
            return Err(MacosVirtualBlockError);
        }
        let mut media = Self {
            directory,
            image,
            attachment: None,
            attachment_uncertain: false,
            cleaned: false,
        };
        media.image = fs::canonicalize(&media.image).map_err(|_| MacosVirtualBlockError)?;
        media.attach(access)?;
        Ok(media)
    }

    pub(crate) fn attach(
        &mut self,
        access: MacosVirtualBlockAccess,
    ) -> Result<(), MacosVirtualBlockError> {
        if self.cleaned || self.attachment.is_some() || self.attachment_uncertain {
            return Err(MacosVirtualBlockError);
        }
        let mut arguments = vec![
            OsString::from("attach"),
            OsString::from("-nomount"),
            OsString::from("-plist"),
        ];
        if access == MacosVirtualBlockAccess::ReadOnly {
            arguments.push(OsString::from("-readonly"));
        }
        arguments.push(self.image.as_os_str().to_os_string());
        self.attachment_uncertain = true;
        let output = match run_hdiutil(&arguments) {
            Ok(output) => output,
            Err(error) => {
                if matches!(mapped_device_for_image(&self.image), Ok(None)) {
                    self.attachment_uncertain = false;
                }
                return Err(error);
            }
        };
        let attachment = match parse_single_unmounted_device(&output.stdout).and_then(|path| {
            if mapped_device_for_image(&self.image)? != Some(path.clone()) {
                return Err(MacosVirtualBlockError);
            }
            let descriptor = open_device(&path, access)?;
            inspect_attachment(&descriptor, path, access)
        }) {
            Ok(attachment) => attachment,
            Err(error) => {
                if matches!(mapped_device_for_image(&self.image), Ok(None)) {
                    self.attachment_uncertain = false;
                }
                return Err(error);
            }
        };
        let valid_length = attachment.len() == Some(EXPECTED_IMAGE_BYTES);
        self.attachment = Some(attachment);
        self.attachment_uncertain = false;
        if !valid_length {
            return Err(MacosVirtualBlockError);
        }
        if !self.mapping_matches()? {
            return Err(MacosVirtualBlockError);
        }
        Ok(())
    }

    pub(crate) fn detach(&mut self) -> Result<(), MacosVirtualBlockError> {
        self.detach_inner()
    }

    pub(crate) fn reattach(
        &mut self,
        access: MacosVirtualBlockAccess,
    ) -> Result<(), MacosVirtualBlockError> {
        self.detach()?;
        self.attach(access)
    }

    pub(crate) fn device_path(&self) -> Result<&Path, MacosVirtualBlockError> {
        self.attachment
            .as_ref()
            .map(|attachment| attachment.device_path.as_path())
            .ok_or(MacosVirtualBlockError)
    }

    pub(crate) fn access(&self) -> Result<MacosVirtualBlockAccess, MacosVirtualBlockError> {
        self.attachment
            .as_ref()
            .map(|attachment| attachment.access)
            .ok_or(MacosVirtualBlockError)
    }

    pub(crate) fn identity(&self) -> Result<MacosVirtualBlockIdentity, MacosVirtualBlockError> {
        self.attachment
            .as_ref()
            .map(|attachment| attachment.identity)
            .ok_or(MacosVirtualBlockError)
    }

    pub(crate) fn logical_block_size(&self) -> Result<u32, MacosVirtualBlockError> {
        self.attachment
            .as_ref()
            .map(|attachment| attachment.logical_block_size)
            .ok_or(MacosVirtualBlockError)
    }

    pub(crate) fn block_count(&self) -> Result<u64, MacosVirtualBlockError> {
        self.attachment
            .as_ref()
            .map(|attachment| attachment.block_count)
            .ok_or(MacosVirtualBlockError)
    }

    pub(crate) fn len(&self) -> Result<u64, MacosVirtualBlockError> {
        self.attachment
            .as_ref()
            .and_then(Attachment::len)
            .ok_or(MacosVirtualBlockError)
    }

    pub(crate) fn open_descriptor(&self) -> Result<File, MacosVirtualBlockError> {
        let attachment = self.attachment.as_ref().ok_or(MacosVirtualBlockError)?;
        let descriptor = open_device(&attachment.device_path, attachment.access)?;
        if inspect_attachment(
            &descriptor,
            attachment.device_path.clone(),
            attachment.access,
        )? != *attachment
        {
            return Err(MacosVirtualBlockError);
        }
        Ok(descriptor)
    }

    pub(crate) fn read_at(
        &self,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>, MacosVirtualBlockError> {
        let end = offset
            .checked_add(u64::try_from(len).map_err(|_| MacosVirtualBlockError)?)
            .ok_or(MacosVirtualBlockError)?;
        if end > self.len()? {
            return Err(MacosVirtualBlockError);
        }
        let descriptor = self.open_descriptor()?;
        let mut bytes = vec![0_u8; len];
        descriptor
            .read_exact_at(&mut bytes, offset)
            .map_err(|_| MacosVirtualBlockError)?;
        Ok(bytes)
    }

    pub(crate) fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), MacosVirtualBlockError> {
        if self.access()? != MacosVirtualBlockAccess::ReadWrite {
            return Err(MacosVirtualBlockError);
        }
        let end = offset
            .checked_add(u64::try_from(bytes.len()).map_err(|_| MacosVirtualBlockError)?)
            .ok_or(MacosVirtualBlockError)?;
        if end > self.len()? {
            return Err(MacosVirtualBlockError);
        }
        let descriptor = self.open_descriptor()?;
        descriptor
            .write_all_at(bytes, offset)
            .map_err(|_| MacosVirtualBlockError)?;
        synchronize_cache(descriptor.as_raw_fd())
    }

    pub(crate) fn cleanup(mut self) -> Result<(), MacosVirtualBlockError> {
        self.detach_inner()?;
        self.remove_storage()
    }

    fn mapping_matches(&self) -> Result<bool, MacosVirtualBlockError> {
        let attachment = self.attachment.as_ref().ok_or(MacosVirtualBlockError)?;
        let Some(reported_device) = mapped_device_for_image(&self.image)? else {
            return Ok(false);
        };
        if reported_device != attachment.device_path {
            return Ok(false);
        }
        let descriptor = open_device(&attachment.device_path, MacosVirtualBlockAccess::ReadOnly)?;
        Ok(inspect_attachment(
            &descriptor,
            attachment.device_path.clone(),
            MacosVirtualBlockAccess::ReadOnly,
        )?
        .same_media(attachment))
    }

    fn detach_inner(&mut self) -> Result<(), MacosVirtualBlockError> {
        let Some(attachment) = self.attachment.clone() else {
            return if self.attachment_uncertain {
                Err(MacosVirtualBlockError)
            } else {
                Ok(())
            };
        };
        if !self.mapping_matches()? {
            if self.clear_attachment_if_mapping_absent()? {
                return Ok(());
            }
            return Err(MacosVirtualBlockError);
        }
        let ordinary = run_hdiutil(&[
            OsString::from("detach"),
            attachment.device_path.as_os_str().to_os_string(),
        ]);
        if ordinary.is_err() {
            if self.clear_attachment_if_mapping_absent()? {
                return Ok(());
            }
            if !self.mapping_matches()? {
                return Err(MacosVirtualBlockError);
            }
            let forced = run_hdiutil(&[
                OsString::from("detach"),
                attachment.device_path.as_os_str().to_os_string(),
                OsString::from("-force"),
            ]);
            if forced.is_err() && !self.clear_attachment_if_mapping_absent()? {
                return Err(MacosVirtualBlockError);
            }
        }
        if !self.clear_attachment_if_mapping_absent()? {
            return Err(MacosVirtualBlockError);
        }
        Ok(())
    }

    fn clear_attachment_if_mapping_absent(&mut self) -> Result<bool, MacosVirtualBlockError> {
        if mapped_device_for_image(&self.image)?.is_some() {
            return Ok(false);
        }
        self.attachment = None;
        self.attachment_uncertain = false;
        Ok(true)
    }

    fn remove_storage(&mut self) -> Result<(), MacosVirtualBlockError> {
        if self.attachment.is_some() || self.attachment_uncertain || self.cleaned {
            return if self.cleaned {
                Ok(())
            } else {
                Err(MacosVirtualBlockError)
            };
        }
        fs::remove_file(&self.image).map_err(|_| MacosVirtualBlockError)?;
        fs::remove_dir(&self.directory).map_err(|_| MacosVirtualBlockError)?;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for MacosVirtualBlock {
    fn drop(&mut self) {
        if self.cleaned || self.attachment_uncertain {
            return;
        }
        if self.attachment.is_some() && self.detach_inner().is_err() {
            return;
        }
        let _ = self.remove_storage();
    }
}

impl PartialEq for Attachment {
    fn eq(&self, other: &Self) -> bool {
        self.device_path == other.device_path
            && self.access == other.access
            && self.identity == other.identity
            && self.logical_block_size == other.logical_block_size
            && self.block_count == other.block_count
    }
}

impl Attachment {
    fn same_media(&self, other: &Self) -> bool {
        self.device_path == other.device_path
            && self.identity == other.identity
            && self.logical_block_size == other.logical_block_size
            && self.block_count == other.block_count
    }
}

fn unique_fixture_directory() -> Result<PathBuf, MacosVirtualBlockError> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| MacosVirtualBlockError)?
        .as_nanos();
    for _ in 0..1_024 {
        let sequence = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bangbang-virtual-block-{}-{timestamp}-{sequence}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return fs::canonicalize(path).map_err(|_| MacosVirtualBlockError),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(MacosVirtualBlockError),
        }
    }
    Err(MacosVirtualBlockError)
}

fn run_hdiutil(arguments: &[OsString]) -> Result<Output, MacosVirtualBlockError> {
    let output = Command::new(HDIUTIL)
        .args(arguments)
        .output()
        .map_err(|_| MacosVirtualBlockError)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(MacosVirtualBlockError)
    }
}

fn parse_single_unmounted_device(bytes: &[u8]) -> Result<PathBuf, MacosVirtualBlockError> {
    let value = Value::from_reader(Cursor::new(bytes)).map_err(|_| MacosVirtualBlockError)?;
    let dictionary = value.as_dictionary().ok_or(MacosVirtualBlockError)?;
    let entities = system_entities(dictionary)?;
    let [entity] = entities else {
        return Err(MacosVirtualBlockError);
    };
    entity_device(entity)
}

fn mapped_device_for_image(image: &Path) -> Result<Option<PathBuf>, MacosVirtualBlockError> {
    let output = run_hdiutil(&[OsString::from("info"), OsString::from("-plist")])?;
    let value =
        Value::from_reader(Cursor::new(output.stdout)).map_err(|_| MacosVirtualBlockError)?;
    let dictionary = value.as_dictionary().ok_or(MacosVirtualBlockError)?;
    let images = dictionary
        .get("images")
        .and_then(Value::as_array)
        .ok_or(MacosVirtualBlockError)?;
    let mut matched = None;
    for candidate in images {
        let Some(candidate) = candidate.as_dictionary() else {
            continue;
        };
        let Some(reported_path) = candidate.get("image-path").and_then(Value::as_string) else {
            continue;
        };
        if Path::new(reported_path) != image {
            continue;
        }
        if matched.is_some() {
            return Err(MacosVirtualBlockError);
        }
        let entities = system_entities(candidate)?;
        let [entity] = entities else {
            return Err(MacosVirtualBlockError);
        };
        matched = Some(entity_device(entity)?);
    }
    Ok(matched)
}

fn system_entities(dictionary: &Dictionary) -> Result<&[Value], MacosVirtualBlockError> {
    dictionary
        .get("system-entities")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or(MacosVirtualBlockError)
}

fn entity_device(value: &Value) -> Result<PathBuf, MacosVirtualBlockError> {
    let dictionary = value.as_dictionary().ok_or(MacosVirtualBlockError)?;
    if dictionary.contains_key("mount-point") {
        return Err(MacosVirtualBlockError);
    }
    let device = dictionary
        .get("dev-entry")
        .and_then(Value::as_string)
        .ok_or(MacosVirtualBlockError)?;
    let Some(suffix) = device.strip_prefix(DEVICE_PREFIX) else {
        return Err(MacosVirtualBlockError);
    };
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(MacosVirtualBlockError);
    }
    Ok(PathBuf::from(device))
}

fn open_device(
    path: &Path,
    access: MacosVirtualBlockAccess,
) -> Result<File, MacosVirtualBlockError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(access == MacosVirtualBlockAccess::ReadWrite)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let descriptor = options.open(path).map_err(|_| MacosVirtualBlockError)?;
    // SAFETY: F_GETFL only reads status flags from the live descriptor.
    let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
    // SAFETY: F_GETFD only reads per-descriptor flags from the same live fd.
    let descriptor_flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) };
    if flags < 0
        || descriptor_flags < 0
        || descriptor_flags & libc::FD_CLOEXEC == 0
        || flags & libc::O_ACCMODE != access.open_flags()
        || flags & libc::O_APPEND != 0
    {
        return Err(MacosVirtualBlockError);
    }
    Ok(descriptor)
}

fn inspect_attachment(
    descriptor: &File,
    device_path: PathBuf,
    access: MacosVirtualBlockAccess,
) -> Result<Attachment, MacosVirtualBlockError> {
    let metadata = descriptor.metadata().map_err(|_| MacosVirtualBlockError)?;
    if metadata.mode() & u32::from(libc::S_IFMT) != u32::from(libc::S_IFBLK) {
        return Err(MacosVirtualBlockError);
    }
    let target_device = metadata.rdev();
    if target_device == 0 {
        return Err(MacosVirtualBlockError);
    }
    let (logical_block_size, block_count) = block_geometry(descriptor.as_raw_fd())?;
    let len = u64::from(logical_block_size)
        .checked_mul(block_count)
        .ok_or(MacosVirtualBlockError)?;
    if logical_block_size == 0
        || block_count == 0
        || u64::from(logical_block_size) % VIRTIO_SECTOR_BYTES != 0
        || len == 0
        || len % VIRTIO_SECTOR_BYTES != 0
        || i64::try_from(len).is_err()
    {
        return Err(MacosVirtualBlockError);
    }
    Ok(Attachment {
        device_path,
        access,
        identity: MacosVirtualBlockIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            target_device,
        },
        logical_block_size,
        block_count,
    })
}

fn block_geometry(descriptor: RawFd) -> Result<(u32, u64), MacosVirtualBlockError> {
    let mut logical_block_size = 0_u32;
    // SAFETY: DKIOCGETBLOCKSIZE writes one u32 through the valid pointer and
    // only inspects the live block descriptor during this call.
    if unsafe { libc::ioctl(descriptor, DKIOCGETBLOCKSIZE, &mut logical_block_size) } < 0 {
        return Err(MacosVirtualBlockError);
    }
    let mut block_count = 0_u64;
    // SAFETY: DKIOCGETBLOCKCOUNT writes one u64 through the valid pointer and
    // only inspects the live block descriptor during this call.
    if unsafe { libc::ioctl(descriptor, DKIOCGETBLOCKCOUNT, &mut block_count) } < 0 {
        return Err(MacosVirtualBlockError);
    }
    Ok((logical_block_size, block_count))
}

fn synchronize_cache(descriptor: RawFd) -> Result<(), MacosVirtualBlockError> {
    // SAFETY: DKIOCSYNCHRONIZECACHE has no pointer payload and only operates on
    // the live block descriptor during this call.
    if unsafe { libc::ioctl(descriptor, DKIOCSYNCHRONIZECACHE) } < 0 {
        Err(MacosVirtualBlockError)
    } else {
        Ok(())
    }
}
