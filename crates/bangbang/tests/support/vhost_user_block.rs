#![allow(
    dead_code,
    reason = "the signed executable test selects backend controls per focused scenario"
)]

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::mem::{MaybeUninit, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::FileExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{Ordering, fence};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const VHOST_USER_HEADER_SIZE: usize = 12;
const VHOST_USER_VERSION: u32 = 1;
const VHOST_USER_REPLY: u32 = 1 << 2;
const VHOST_USER_NEED_REPLY: u32 = 1 << 3;
const VHOST_USER_MAX_BODY: usize = 0x1000;
const VHOST_USER_MAX_FDS: usize = 32;

const VHOST_USER_GET_FEATURES: u32 = 1;
const VHOST_USER_SET_FEATURES: u32 = 2;
const VHOST_USER_SET_OWNER: u32 = 3;
const VHOST_USER_SET_MEMORY_TABLE: u32 = 5;
const VHOST_USER_SET_VRING_NUM: u32 = 8;
const VHOST_USER_SET_VRING_ADDR: u32 = 9;
const VHOST_USER_SET_VRING_BASE: u32 = 10;
const VHOST_USER_SET_VRING_KICK: u32 = 12;
const VHOST_USER_SET_VRING_CALL: u32 = 13;
const VHOST_USER_GET_PROTOCOL_FEATURES: u32 = 15;
const VHOST_USER_SET_PROTOCOL_FEATURES: u32 = 16;
const VHOST_USER_SET_VRING_ENABLE: u32 = 18;
const VHOST_USER_GET_CONFIG: u32 = 24;

const VIRTIO_BLK_F_RO: u64 = 1 << 5;
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
const VHOST_USER_F_PROTOCOL_FEATURES: u64 = 1 << 30;
const VIRTIO_F_VERSION_1: u64 = 1 << 32;
const VHOST_USER_PROTOCOL_F_REPLY_ACK: u64 = 1 << 3;
const VHOST_USER_PROTOCOL_F_CONFIG: u64 = 1 << 9;
const REQUIRED_VIRTIO_FEATURES: u64 = VHOST_USER_F_PROTOCOL_FEATURES | VIRTIO_F_VERSION_1;
const PROTOCOL_FEATURES: u64 = VHOST_USER_PROTOCOL_F_REPLY_ACK | VHOST_USER_PROTOCOL_F_CONFIG;

const VIRTIO_BLOCK_CONFIG_SIZE: usize = 60;
const VIRTIO_BLOCK_SECTOR_SIZE: u64 = 512;
const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;
const VIRTIO_BLK_T_FLUSH: u32 = 4;
const VIRTIO_BLK_T_GET_ID: u32 = 8;
const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;
const VIRTQ_DESC_F_INDIRECT: u16 = 4;
const NOTIFICATION_SIZE: usize = 8;
const BACKEND_POLL_MILLIS: i32 = 25;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
pub(crate) struct VhostUserBlockBackendOptions {
    read_only: bool,
    advertise_flush: bool,
    advertised_features: Option<u64>,
    advertised_protocol_features: u64,
}

impl VhostUserBlockBackendOptions {
    pub(crate) const fn regular(read_only: bool) -> Self {
        Self {
            read_only,
            advertise_flush: !read_only,
            advertised_features: None,
            advertised_protocol_features: PROTOCOL_FEATURES,
        }
    }

    pub(crate) const fn missing_version_one(read_only: bool) -> Self {
        Self::regular(read_only).without_required_virtio_feature(VIRTIO_F_VERSION_1)
    }

    pub(crate) const fn without_required_virtio_feature(mut self, feature: u64) -> Self {
        let regular = REQUIRED_VIRTIO_FEATURES
            | if self.read_only { VIRTIO_BLK_F_RO } else { 0 }
            | if self.advertise_flush {
                VIRTIO_BLK_F_FLUSH
            } else {
                0
            };
        self.advertised_features = Some(regular & !feature);
        self
    }

    pub(crate) const fn without_config_protocol(mut self) -> Self {
        self.advertised_protocol_features &= !VHOST_USER_PROTOCOL_F_CONFIG;
        self
    }

    const fn features(self) -> u64 {
        match self.advertised_features {
            Some(features) => features,
            None => {
                REQUIRED_VIRTIO_FEATURES
                    | if self.read_only { VIRTIO_BLK_F_RO } else { 0 }
                    | if self.advertise_flush {
                        VIRTIO_BLK_F_FLUSH
                    } else {
                        0
                    }
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct VhostUserBlockBackendReport {
    pub(crate) owner_requests: u64,
    pub(crate) config_requests: u64,
    pub(crate) discovery_rejected: bool,
    pub(crate) guest_features: Option<u64>,
    pub(crate) memory_regions: usize,
    pub(crate) queue_size: Option<u16>,
    pub(crate) activated: bool,
    pub(crate) kicks: u64,
    pub(crate) calls: u64,
    pub(crate) requests: u64,
    pub(crate) reads: u64,
    pub(crate) writes: u64,
    pub(crate) flushes: u64,
    pub(crate) errors: u64,
    pub(crate) frontend_closed: bool,
    pub(crate) terminal_error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum BackendControl {
    Disconnect,
}

#[derive(Debug)]
pub(crate) struct VhostUserBlockBackend {
    control: Sender<BackendControl>,
    report: Arc<Mutex<VhostUserBlockBackendReport>>,
    worker: Option<JoinHandle<Result<(), String>>>,
}

impl VhostUserBlockBackend {
    pub(crate) fn start(
        socket_path: &Path,
        backing_path: &Path,
        options: VhostUserBlockBackendOptions,
    ) -> Result<Self, String> {
        let listener = UnixListener::bind(socket_path).map_err(|error| {
            format!(
                "test vhost-user backend could not bind {}: {:?}",
                socket_path.display(),
                error.kind()
            )
        })?;
        listener.set_nonblocking(true).map_err(|error| {
            format!(
                "test vhost-user backend could not configure listener: {:?}",
                error.kind()
            )
        })?;
        let backing = open_backing(backing_path, options.read_only)?;
        let socket_path = socket_path.to_path_buf();
        let report = Arc::new(Mutex::new(VhostUserBlockBackendReport::default()));
        let worker_report = Arc::clone(&report);
        let (control, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _socket_guard = SocketPathGuard(socket_path);
            let result = (|| {
                let stream = accept_frontend(&listener, &receiver)?;
                run_backend(stream, backing, options, &receiver, &worker_report)
            })();
            if let Err(error) = &result {
                worker_report
                    .lock()
                    .expect("test vhost-user backend report should not be poisoned")
                    .terminal_error = Some(error.clone());
            }
            result
        });

        Ok(Self {
            control,
            report,
            worker: Some(worker),
        })
    }

    pub(crate) fn report(&self) -> VhostUserBlockBackendReport {
        self.report
            .lock()
            .expect("test vhost-user backend report should not be poisoned")
            .clone()
    }

    pub(crate) fn wait_for_activation(&self, timeout: Duration) -> Result<(), String> {
        let started = Instant::now();
        loop {
            if self.report().activated {
                return Ok(());
            }
            if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
                return Err(format!(
                    "test vhost-user backend exited before activation: {:?}",
                    self.report()
                ));
            }
            if started.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {timeout:?} waiting for test vhost-user activation"
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn wait_for_flush(&self, timeout: Duration) -> Result<(), String> {
        let started = Instant::now();
        loop {
            let report = self.report();
            if report.flushes > 0 {
                return Ok(());
            }
            if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
                return Err(format!(
                    "test vhost-user backend exited before a flush: {report:?}"
                ));
            }
            if started.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {timeout:?} waiting for a test vhost-user flush: {report:?}"
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn wait_for_frontend_close(&self, timeout: Duration) -> Result<(), String> {
        let started = Instant::now();
        loop {
            if self.report().frontend_closed {
                return Ok(());
            }
            if started.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {timeout:?} waiting for test vhost-user frontend closure"
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn disconnect(&self) -> Result<(), String> {
        self.control
            .send(BackendControl::Disconnect)
            .map_err(|_| "test vhost-user backend already stopped".to_string())
    }

    pub(crate) fn finish(mut self) -> Result<VhostUserBlockBackendReport, String> {
        self.finish_inner()?;
        Ok(self.report())
    }

    fn finish_inner(&mut self) -> Result<(), String> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        let _ = self.control.send(BackendControl::Disconnect);
        worker
            .join()
            .map_err(|_| "test vhost-user backend thread panicked".to_string())?
    }
}

impl Drop for VhostUserBlockBackend {
    fn drop(&mut self) {
        let _ = self.finish_inner();
    }
}

#[derive(Debug)]
struct SocketPathGuard(PathBuf);

impl Drop for SocketPathGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn open_backing(path: &Path, read_only: bool) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(!read_only)
        .open(path)
        .map_err(|error| {
            format!(
                "test vhost-user backend could not open backing {}: {:?}",
                path.display(),
                error.kind()
            )
        })
}

fn accept_frontend(
    listener: &UnixListener,
    control: &Receiver<BackendControl>,
) -> Result<UnixStream, String> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(io_kind("configure accepted vhost-user stream"))?;
                return Ok(stream);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => {
                return Err(format!(
                    "test vhost-user backend accept failed: {:?}",
                    error.kind()
                ));
            }
        }
        match control.try_recv() {
            Ok(BackendControl::Disconnect) | Err(TryRecvError::Disconnected) => {
                return Err("test vhost-user backend stopped before connection".to_string());
            }
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn run_backend(
    mut stream: UnixStream,
    backing: File,
    options: VhostUserBlockBackendOptions,
    control: &Receiver<BackendControl>,
    report: &Arc<Mutex<VhostUserBlockBackendReport>>,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(io_kind("configure vhost-user read timeout"))?;
    stream
        .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
        .map_err(io_kind("configure vhost-user write timeout"))?;
    let mut state = BackendState::new(backing, options, Arc::clone(report))?;

    let owner = expect_request(&mut stream, VHOST_USER_SET_OWNER, false, 0)?;
    if !owner.body.is_empty() {
        return Err("SET_OWNER body was not empty".to_string());
    }
    state.update_report(|report| report.owner_requests += 1);

    let features = expect_request(&mut stream, VHOST_USER_GET_FEATURES, false, 0)?;
    if !features.body.is_empty() {
        return Err("GET_FEATURES body was not empty".to_string());
    }
    send_reply(
        &mut stream,
        VHOST_USER_GET_FEATURES,
        &options.features().to_ne_bytes(),
    )?;
    if options.features() & REQUIRED_VIRTIO_FEATURES != REQUIRED_VIRTIO_FEATURES {
        state.update_report(|report| report.discovery_rejected = true);
        return Ok(());
    }

    let protocols = expect_request(&mut stream, VHOST_USER_GET_PROTOCOL_FEATURES, false, 0)?;
    if !protocols.body.is_empty() {
        return Err("GET_PROTOCOL_FEATURES body was not empty".to_string());
    }
    send_reply(
        &mut stream,
        VHOST_USER_GET_PROTOCOL_FEATURES,
        &options.advertised_protocol_features.to_ne_bytes(),
    )?;
    if options.advertised_protocol_features & VHOST_USER_PROTOCOL_F_CONFIG == 0 {
        state.update_report(|report| report.discovery_rejected = true);
        return Ok(());
    }

    let set_protocols = expect_request(&mut stream, VHOST_USER_SET_PROTOCOL_FEATURES, false, 0)?;
    let selected_protocols = read_u64(&set_protocols.body, 0)?;
    if selected_protocols
        != VHOST_USER_PROTOCOL_F_CONFIG
            | (options.advertised_protocol_features & VHOST_USER_PROTOCOL_F_REPLY_ACK)
    {
        return Err("frontend selected unexpected protocol features".to_string());
    }
    state.reply_ack = selected_protocols & VHOST_USER_PROTOCOL_F_REPLY_ACK != 0;

    let get_config = expect_request(&mut stream, VHOST_USER_GET_CONFIG, false, 0)?;
    state.handle_get_config(&mut stream, &get_config)?;

    let set_features = expect_request(&mut stream, VHOST_USER_SET_FEATURES, state.reply_ack, 0)?;
    state.handle_set_features(&set_features)?;
    acknowledge(&mut stream, &set_features)?;

    let memory = expect_request(
        &mut stream,
        VHOST_USER_SET_MEMORY_TABLE,
        state.reply_ack,
        usize::MAX,
    )?;
    state.install_memory(memory)?;
    acknowledge_code(&mut stream, VHOST_USER_SET_MEMORY_TABLE, state.reply_ack)?;

    let number = expect_request(&mut stream, VHOST_USER_SET_VRING_NUM, state.reply_ack, 0)?;
    state.set_queue_size(&number.body)?;
    acknowledge(&mut stream, &number)?;

    let address = expect_request(&mut stream, VHOST_USER_SET_VRING_ADDR, state.reply_ack, 0)?;
    state.set_queue_addresses(&address.body)?;
    acknowledge(&mut stream, &address)?;

    let base = expect_request(&mut stream, VHOST_USER_SET_VRING_BASE, state.reply_ack, 0)?;
    state.set_queue_base(&base.body)?;
    acknowledge(&mut stream, &base)?;

    let call = expect_request(&mut stream, VHOST_USER_SET_VRING_CALL, state.reply_ack, 1)?;
    state.set_call(call)?;
    acknowledge_code(&mut stream, VHOST_USER_SET_VRING_CALL, state.reply_ack)?;

    let kick = expect_request(&mut stream, VHOST_USER_SET_VRING_KICK, state.reply_ack, 1)?;
    state.set_kick(kick)?;
    acknowledge_code(&mut stream, VHOST_USER_SET_VRING_KICK, state.reply_ack)?;

    let enable = expect_request(&mut stream, VHOST_USER_SET_VRING_ENABLE, state.reply_ack, 0)?;
    state.enable_queue(&enable.body)?;
    acknowledge(&mut stream, &enable)?;
    state.update_report(|report| report.activated = true);

    stream
        .set_read_timeout(None)
        .map_err(io_kind("clear vhost-user read timeout"))?;
    stream
        .set_write_timeout(None)
        .map_err(io_kind("clear vhost-user write timeout"))?;
    state.run_active(&mut stream, control)
}

#[derive(Debug)]
struct VhostUserRequest {
    code: u32,
    need_reply: bool,
    body: Vec<u8>,
    descriptors: Vec<OwnedFd>,
}

fn receive_request(stream: &UnixStream) -> Result<VhostUserRequest, String> {
    let mut header = [0_u8; VHOST_USER_HEADER_SIZE];
    let mut descriptors = receive_exact_with_fds(stream, &mut header)?;
    let code = read_u32(&header, 0)?;
    let flags = read_u32(&header, 4)?;
    let body_size = usize::try_from(read_u32(&header, 8)?)
        .map_err(|_| "vhost-user body size did not fit usize".to_string())?;
    if flags & 0x3 != VHOST_USER_VERSION
        || flags & VHOST_USER_REPLY != 0
        || flags & !(0x3 | VHOST_USER_REPLY | VHOST_USER_NEED_REPLY) != 0
        || body_size > VHOST_USER_MAX_BODY
    {
        return Err("vhost-user request header was invalid".to_string());
    }
    let mut body = vec![0_u8; body_size];
    descriptors.extend(receive_exact_with_fds(stream, &mut body)?);
    if descriptors.len() > VHOST_USER_MAX_FDS {
        return Err("vhost-user request attached too many descriptors".to_string());
    }
    Ok(VhostUserRequest {
        code,
        need_reply: flags & VHOST_USER_NEED_REPLY != 0,
        body,
        descriptors,
    })
}

fn expect_request(
    stream: &mut UnixStream,
    code: u32,
    need_reply: bool,
    descriptor_count: usize,
) -> Result<VhostUserRequest, String> {
    let request = receive_request(stream)?;
    if request.code != code || request.need_reply != need_reply {
        return Err(format!(
            "unexpected vhost-user request: code={}, need_reply={}",
            request.code, request.need_reply
        ));
    }
    if descriptor_count != usize::MAX && request.descriptors.len() != descriptor_count {
        return Err(format!(
            "vhost-user request {code} attached {} descriptors instead of {descriptor_count}",
            request.descriptors.len()
        ));
    }
    Ok(request)
}

fn receive_exact_with_fds(stream: &UnixStream, bytes: &mut [u8]) -> Result<Vec<OwnedFd>, String> {
    let mut received = 0_usize;
    let mut descriptors = Vec::new();
    while received < bytes.len() {
        let (count, mut attempt_descriptors) =
            receive_once(stream.as_raw_fd(), &mut bytes[received..])?;
        if count == 0 {
            return Err("vhost-user frontend disconnected".to_string());
        }
        received = received
            .checked_add(count)
            .ok_or_else(|| "vhost-user receive count overflowed".to_string())?;
        descriptors.append(&mut attempt_descriptors);
    }
    Ok(descriptors)
}

fn receive_once(descriptor: RawFd, bytes: &mut [u8]) -> Result<(usize, Vec<OwnedFd>), String> {
    let mut iovec = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut control = [0_usize; 64];
    // SAFETY: An all-zero msghdr is valid. The live writable byte and aligned
    // control buffers are installed before the synchronous recvmsg call.
    let mut message: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
    message.msg_iov = &raw mut iovec;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = (control.len() * size_of::<usize>()) as _;
    // SAFETY: `message` refers only to the live buffers declared above.
    let result = unsafe { libc::recvmsg(descriptor, &raw mut message, 0) };
    if result < 0 {
        return Err(format!(
            "vhost-user recvmsg failed: {:?}",
            io::Error::last_os_error().kind()
        ));
    }
    if message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err("vhost-user recvmsg truncated a frame".to_string());
    }
    let count = usize::try_from(result)
        .map_err(|_| "vhost-user receive count did not fit usize".to_string())?;
    if count > bytes.len() {
        return Err("vhost-user recvmsg exceeded its destination".to_string());
    }

    let mut descriptors = Vec::new();
    // SAFETY: The kernel initialized the returned control region. Every
    // nonnegative SCM_RIGHTS descriptor is adopted exactly once here.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&raw const message);
        while !header.is_null() {
            if (*header).cmsg_level != libc::SOL_SOCKET || (*header).cmsg_type != libc::SCM_RIGHTS {
                return Err("vhost-user recvmsg contained unexpected control data".to_string());
            }
            let header_size = usize::try_from(libc::CMSG_LEN(0))
                .map_err(|_| "control header size did not fit usize".to_string())?;
            let declared = usize::try_from((*header).cmsg_len)
                .map_err(|_| "control data size did not fit usize".to_string())?;
            if declared < header_size || (declared - header_size) % size_of::<i32>() != 0 {
                return Err("SCM_RIGHTS control length was invalid".to_string());
            }
            let descriptor_count = (declared - header_size) / size_of::<i32>();
            for index in 0..descriptor_count {
                let raw = std::ptr::read_unaligned(
                    libc::CMSG_DATA(header)
                        .add(index * size_of::<i32>())
                        .cast::<i32>(),
                );
                if raw < 0 {
                    return Err("SCM_RIGHTS returned a negative descriptor".to_string());
                }
                descriptors.push(OwnedFd::from_raw_fd(raw));
            }
            header = libc::CMSG_NXTHDR(&raw const message, header);
        }
    }
    for descriptor in &descriptors {
        set_cloexec(descriptor.as_raw_fd())?;
    }
    Ok((count, descriptors))
}

fn set_cloexec(descriptor: RawFd) -> Result<(), String> {
    // SAFETY: F_GETFD/F_SETFD operate on one live received descriptor.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 {
        return Err("could not inspect received descriptor flags".to_string());
    }
    if flags & libc::FD_CLOEXEC == 0 {
        // SAFETY: The descriptor remains live and `flags` came from F_GETFD.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags | libc::FD_CLOEXEC) } != 0 {
            return Err("could not set close-on-exec on received descriptor".to_string());
        }
    }
    Ok(())
}

fn send_reply(stream: &mut UnixStream, code: u32, body: &[u8]) -> Result<(), String> {
    let mut frame = Vec::with_capacity(VHOST_USER_HEADER_SIZE + body.len());
    frame.extend_from_slice(&code.to_ne_bytes());
    frame.extend_from_slice(&(VHOST_USER_VERSION | VHOST_USER_REPLY).to_ne_bytes());
    frame.extend_from_slice(
        &u32::try_from(body.len())
            .map_err(|_| "vhost-user reply body was too large".to_string())?
            .to_ne_bytes(),
    );
    frame.extend_from_slice(body);
    stream
        .write_all(&frame)
        .map_err(io_kind("write vhost-user reply"))
}

fn acknowledge(stream: &mut UnixStream, request: &VhostUserRequest) -> Result<(), String> {
    acknowledge_code(stream, request.code, request.need_reply)
}

fn acknowledge_code(stream: &mut UnixStream, code: u32, need_reply: bool) -> Result<(), String> {
    if need_reply {
        send_reply(stream, code, &0_u64.to_ne_bytes())?;
    }
    Ok(())
}

fn io_kind(context: &'static str) -> impl FnOnce(io::Error) -> String {
    move |error| format!("{context} failed: {:?}", error.kind())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| "u16 offset overflowed".to_string())?;
    let raw: [u8; 2] = bytes
        .get(offset..end)
        .ok_or_else(|| "u16 field was truncated".to_string())?
        .try_into()
        .map_err(|_| "u16 field length was invalid".to_string())?;
    Ok(u16::from_le_bytes(raw))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "u32 offset overflowed".to_string())?;
    let raw: [u8; 4] = bytes
        .get(offset..end)
        .ok_or_else(|| "u32 field was truncated".to_string())?
        .try_into()
        .map_err(|_| "u32 field length was invalid".to_string())?;
    Ok(u32::from_ne_bytes(raw))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, String> {
    let end = offset
        .checked_add(8)
        .ok_or_else(|| "u64 offset overflowed".to_string())?;
    let raw: [u8; 8] = bytes
        .get(offset..end)
        .ok_or_else(|| "u64 field was truncated".to_string())?
        .try_into()
        .map_err(|_| "u64 field length was invalid".to_string())?;
    Ok(u64::from_ne_bytes(raw))
}

#[derive(Debug)]
struct SharedMemoryRegion {
    guest_phys_addr: u64,
    userspace_addr: u64,
    size: usize,
    mapping: NonNull<u8>,
    descriptor: OwnedFd,
}

// SAFETY: Each region owns its mmap and backing descriptor, is moved into one
// backend thread, and is unmapped only after that thread stops accessing it.
unsafe impl Send for SharedMemoryRegion {}

impl SharedMemoryRegion {
    fn map(
        guest_phys_addr: u64,
        memory_size: u64,
        userspace_addr: u64,
        mmap_offset: u64,
        descriptor: OwnedFd,
    ) -> Result<Self, String> {
        let size = usize::try_from(memory_size)
            .map_err(|_| "vhost-user memory size did not fit usize".to_string())?;
        let offset: libc::off_t = mmap_offset
            .try_into()
            .map_err(|_| "vhost-user mmap offset did not fit off_t".to_string())?;
        if size == 0 {
            return Err("vhost-user memory region was empty".to_string());
        }
        // SAFETY: The received descriptor is live and sized by the frontend;
        // the successful mapping is retained and unmapped by Drop.
        let mapping = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                descriptor.as_raw_fd(),
                offset,
            )
        };
        if mapping == libc::MAP_FAILED {
            return Err(format!(
                "test backend could not map shared guest memory: {:?}",
                io::Error::last_os_error().kind()
            ));
        }
        let mapping = NonNull::new(mapping.cast::<u8>())
            .ok_or_else(|| "test backend received a null mapping".to_string())?;
        Ok(Self {
            guest_phys_addr,
            userspace_addr,
            size,
            mapping,
            descriptor,
        })
    }

    fn contains(&self, base: u64, address: u64, length: usize) -> Option<usize> {
        let offset = address.checked_sub(base)?;
        let offset = usize::try_from(offset).ok()?;
        let end = offset.checked_add(length)?;
        (end <= self.size).then_some(offset)
    }

    fn guest_offset(&self, address: u64, length: usize) -> Option<usize> {
        self.contains(self.guest_phys_addr, address, length)
    }

    fn userspace_offset(&self, address: u64, length: usize) -> Option<usize> {
        self.contains(self.userspace_addr, address, length)
    }

    fn read_at(&self, offset: usize, destination: &mut [u8]) {
        for (index, byte) in destination.iter_mut().enumerate() {
            // SAFETY: Translation checked the complete range and the mapping
            // remains live. Volatile access preserves cross-process ring I/O.
            *byte = unsafe { self.mapping.as_ptr().add(offset + index).read_volatile() };
        }
    }

    fn write_at(&self, offset: usize, source: &[u8]) {
        for (index, byte) in source.iter().copied().enumerate() {
            // SAFETY: Translation checked the complete range and the mapping
            // remains live. Volatile access preserves cross-process ring I/O.
            unsafe {
                self.mapping
                    .as_ptr()
                    .add(offset + index)
                    .write_volatile(byte);
            }
        }
    }
}

impl Drop for SharedMemoryRegion {
    fn drop(&mut self) {
        // Keep the descriptor observably owned through munmap.
        let _descriptor = self.descriptor.as_raw_fd();
        // SAFETY: `mapping` and `size` are the exact successful mmap result
        // retained exclusively by this owner.
        let _ = unsafe { libc::munmap(self.mapping.as_ptr().cast(), self.size) };
    }
}

#[derive(Debug, Clone, Copy)]
struct VirtqueueDescriptor {
    address: u64,
    length: u32,
    flags: u16,
    next: u16,
}

#[derive(Debug, Default)]
struct BackendQueue {
    size: u16,
    descriptor_table: u64,
    used_ring: u64,
    available_ring: u64,
    next_available: u16,
    next_used: u16,
    call: Option<OwnedFd>,
    kick: Option<OwnedFd>,
    enabled: bool,
}

struct BackendState {
    backing: File,
    capacity_bytes: u64,
    options: VhostUserBlockBackendOptions,
    reply_ack: bool,
    memory: Vec<SharedMemoryRegion>,
    queue: BackendQueue,
    report: Arc<Mutex<VhostUserBlockBackendReport>>,
}

impl BackendState {
    fn new(
        backing: File,
        options: VhostUserBlockBackendOptions,
        report: Arc<Mutex<VhostUserBlockBackendReport>>,
    ) -> Result<Self, String> {
        let capacity_bytes = backing
            .metadata()
            .map_err(io_kind("inspect test vhost-user backing"))?
            .len();
        if capacity_bytes == 0 || capacity_bytes % VIRTIO_BLOCK_SECTOR_SIZE != 0 {
            return Err("test vhost-user backing must contain complete sectors".to_string());
        }
        Ok(Self {
            backing,
            capacity_bytes,
            options,
            reply_ack: false,
            memory: Vec::new(),
            queue: BackendQueue::default(),
            report,
        })
    }

    fn update_report(&self, update: impl FnOnce(&mut VhostUserBlockBackendReport)) {
        update(
            &mut self
                .report
                .lock()
                .expect("test vhost-user backend report should not be poisoned"),
        );
    }

    fn handle_get_config(
        &self,
        stream: &mut UnixStream,
        request: &VhostUserRequest,
    ) -> Result<(), String> {
        if request.body.len() != 12 + VIRTIO_BLOCK_CONFIG_SIZE
            || read_u32(&request.body, 0)? != 0
            || read_u32(&request.body, 4)? != VIRTIO_BLOCK_CONFIG_SIZE as u32
            || read_u32(&request.body, 8)? != 1
            || request.body[12..].iter().any(|byte| *byte != 0)
        {
            return Err("frontend requested an unexpected block config range".to_string());
        }
        let mut config = [0_u8; VIRTIO_BLOCK_CONFIG_SIZE];
        config[..8]
            .copy_from_slice(&(self.capacity_bytes / VIRTIO_BLOCK_SECTOR_SIZE).to_le_bytes());
        let mut reply = Vec::with_capacity(12 + config.len());
        reply.extend_from_slice(&0_u32.to_ne_bytes());
        reply.extend_from_slice(&(VIRTIO_BLOCK_CONFIG_SIZE as u32).to_ne_bytes());
        reply.extend_from_slice(&1_u32.to_ne_bytes());
        reply.extend_from_slice(&config);
        send_reply(stream, VHOST_USER_GET_CONFIG, &reply)?;
        self.update_report(|report| report.config_requests += 1);
        Ok(())
    }

    fn handle_set_features(&self, request: &VhostUserRequest) -> Result<(), String> {
        let features = read_u64(&request.body, 0)?;
        if request.body.len() != 8
            || features & !self.options.features() != 0
            || features & REQUIRED_VIRTIO_FEATURES != REQUIRED_VIRTIO_FEATURES
        {
            return Err("frontend selected invalid virtio features".to_string());
        }
        self.update_report(|report| report.guest_features = Some(features));
        Ok(())
    }

    fn install_memory(&mut self, request: VhostUserRequest) -> Result<(), String> {
        let region_count = usize::try_from(read_u32(&request.body, 0)?)
            .map_err(|_| "vhost-user memory region count did not fit usize".to_string())?;
        if region_count == 0
            || read_u32(&request.body, 4)? != 0
            || request.body.len() != 8 + region_count * 32
            || request.descriptors.len() != region_count
        {
            return Err("vhost-user memory table was invalid".to_string());
        }
        let mut descriptors = request.descriptors.into_iter();
        let mut regions = Vec::with_capacity(region_count);
        for index in 0..region_count {
            let offset = 8 + index * 32;
            let descriptor = descriptors
                .next()
                .ok_or_else(|| "vhost-user memory descriptor was missing".to_string())?;
            regions.push(SharedMemoryRegion::map(
                read_u64(&request.body, offset)?,
                read_u64(&request.body, offset + 8)?,
                read_u64(&request.body, offset + 16)?,
                read_u64(&request.body, offset + 24)?,
                descriptor,
            )?);
        }
        self.memory = regions;
        self.update_report(|report| report.memory_regions = region_count);
        Ok(())
    }

    fn set_queue_size(&mut self, body: &[u8]) -> Result<(), String> {
        let index = read_u32(body, 0)?;
        let size = u16::try_from(read_u32(body, 4)?)
            .map_err(|_| "vhost-user queue size did not fit u16".to_string())?;
        if body.len() != 8 || index != 0 || size == 0 || !size.is_power_of_two() {
            return Err("vhost-user queue size was invalid".to_string());
        }
        self.queue.size = size;
        self.update_report(|report| report.queue_size = Some(size));
        Ok(())
    }

    fn set_queue_addresses(&mut self, body: &[u8]) -> Result<(), String> {
        if body.len() != 40
            || read_u32(body, 0)? != 0
            || read_u32(body, 4)? != 0
            || read_u64(body, 32)? != 0
        {
            return Err("vhost-user queue addresses were invalid".to_string());
        }
        self.queue.descriptor_table = read_u64(body, 8)?;
        self.queue.used_ring = read_u64(body, 16)?;
        self.queue.available_ring = read_u64(body, 24)?;
        self.read_userspace(self.queue.descriptor_table, 16)?;
        self.read_userspace(self.queue.available_ring, 4)?;
        self.read_userspace(self.queue.used_ring, 4)?;
        Ok(())
    }

    fn set_queue_base(&mut self, body: &[u8]) -> Result<(), String> {
        let base = u16::try_from(read_u32(body, 4)?)
            .map_err(|_| "vhost-user queue base did not fit u16".to_string())?;
        if body.len() != 8 || read_u32(body, 0)? != 0 || base >= self.queue.size {
            return Err("vhost-user queue base was invalid".to_string());
        }
        self.queue.next_available = base;
        Ok(())
    }

    fn set_call(&mut self, mut request: VhostUserRequest) -> Result<(), String> {
        if request.body.len() != 8 || read_u64(&request.body, 0)? != 0 {
            return Err("vhost-user call endpoint was invalid".to_string());
        }
        self.queue.call = request.descriptors.pop();
        Ok(())
    }

    fn set_kick(&mut self, mut request: VhostUserRequest) -> Result<(), String> {
        if request.body.len() != 8 || read_u64(&request.body, 0)? != 0 {
            return Err("vhost-user kick endpoint was invalid".to_string());
        }
        self.queue.kick = request.descriptors.pop();
        Ok(())
    }

    fn enable_queue(&mut self, body: &[u8]) -> Result<(), String> {
        if body.len() != 8
            || read_u32(body, 0)? != 0
            || read_u32(body, 4)? != 1
            || self.queue.call.is_none()
            || self.queue.kick.is_none()
        {
            return Err("vhost-user queue enable was invalid".to_string());
        }
        self.queue.next_used = self.read_userspace_u16(self.queue.used_ring + 2)?;
        self.queue.enabled = true;
        Ok(())
    }

    fn run_active(
        &mut self,
        stream: &mut UnixStream,
        control: &Receiver<BackendControl>,
    ) -> Result<(), String> {
        let socket_fd = stream.as_raw_fd();
        loop {
            match control.try_recv() {
                Ok(BackendControl::Disconnect) | Err(TryRecvError::Disconnected) => return Ok(()),
                Err(TryRecvError::Empty) => {}
            }
            let kick_fd = self
                .queue
                .kick
                .as_ref()
                .ok_or_else(|| "active queue lost its kick endpoint".to_string())?
                .as_raw_fd();
            let mut descriptors = [
                libc::pollfd {
                    fd: kick_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: socket_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: Both initialized poll entries remain writable for this
            // bounded synchronous poll call.
            let result = unsafe {
                libc::poll(
                    descriptors.as_mut_ptr(),
                    descriptors.len() as libc::nfds_t,
                    BACKEND_POLL_MILLIS,
                )
            };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("test vhost-user poll failed: {:?}", error.kind()));
            }
            if descriptors[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                self.update_report(|report| report.frontend_closed = true);
                return Ok(());
            }
            if descriptors[1].revents & libc::POLLIN != 0 {
                match receive_request(stream) {
                    Ok(request)
                        if request.code == VHOST_USER_SET_VRING_ENABLE
                            && read_u32(&request.body, 4).ok() == Some(0) =>
                    {
                        self.queue.enabled = false;
                        acknowledge(stream, &request)?;
                    }
                    Ok(_) => return Err("active frontend sent an unexpected request".to_string()),
                    Err(error) if error == "vhost-user frontend disconnected" => {
                        self.update_report(|report| report.frontend_closed = true);
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                }
            }
            if descriptors[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                return Ok(());
            }
            if descriptors[0].revents & libc::POLLIN != 0 {
                self.drain_kicks()?;
                if self.queue.enabled {
                    self.process_available_requests()?;
                }
            }
        }
    }

    fn drain_kicks(&self) -> Result<(), String> {
        let descriptor = self
            .queue
            .kick
            .as_ref()
            .ok_or_else(|| "active queue lost its kick endpoint".to_string())?
            .as_raw_fd();
        let mut buffer = [0_u8; NOTIFICATION_SIZE * 64];
        let mut notifications = 0_u64;
        loop {
            // SAFETY: The received kick descriptor is a live pipe reader and
            // the complete buffer is writable for this synchronous read.
            let result =
                unsafe { libc::read(descriptor, buffer.as_mut_ptr().cast(), buffer.len()) };
            if result == 0 {
                break;
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                match error.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => break,
                    _ => {
                        return Err(format!(
                            "test vhost-user kick read failed: {:?}",
                            error.kind()
                        ));
                    }
                }
            }
            let count = usize::try_from(result)
                .map_err(|_| "kick read count did not fit usize".to_string())?;
            if count % NOTIFICATION_SIZE != 0 {
                return Err("vhost-user kick notification was partial".to_string());
            }
            notifications = notifications.saturating_add((count / NOTIFICATION_SIZE) as u64);
        }
        self.update_report(|report| report.kicks = report.kicks.saturating_add(notifications));
        Ok(())
    }

    fn process_available_requests(&mut self) -> Result<(), String> {
        fence(Ordering::Acquire);
        let available_index = self.read_userspace_u16(self.queue.available_ring + 2)?;
        if available_index.wrapping_sub(self.queue.next_available) > self.queue.size {
            return Err("vhost-user available ring advanced beyond queue size".to_string());
        }
        let mut processed = 0_u64;
        while self.queue.next_available != available_index {
            let slot = self.queue.next_available % self.queue.size;
            let entry = self
                .queue
                .available_ring
                .checked_add(4 + u64::from(slot) * 2)
                .ok_or_else(|| "available ring entry overflowed".to_string())?;
            let head = self.read_userspace_u16(entry)?;
            let used_length = match self.process_request(head) {
                Ok(length) => length,
                Err(_) => {
                    self.update_report(|report| report.errors = report.errors.saturating_add(1));
                    0
                }
            };
            let used_slot = self.queue.next_used % self.queue.size;
            let used_entry = self
                .queue
                .used_ring
                .checked_add(4 + u64::from(used_slot) * 8)
                .ok_or_else(|| "used ring entry overflowed".to_string())?;
            self.write_userspace(used_entry, &u32::from(head).to_le_bytes())?;
            self.write_userspace(used_entry + 4, &used_length.to_le_bytes())?;
            self.queue.next_available = self.queue.next_available.wrapping_add(1);
            self.queue.next_used = self.queue.next_used.wrapping_add(1);
            fence(Ordering::Release);
            self.write_userspace(
                self.queue.used_ring + 2,
                &self.queue.next_used.to_le_bytes(),
            )?;
            processed = processed.saturating_add(1);
        }
        if processed != 0 {
            self.update_report(|report| {
                report.requests = report.requests.saturating_add(processed)
            });
            self.signal_call()?;
        }
        Ok(())
    }

    fn process_request(&mut self, head: u16) -> Result<u32, String> {
        let chain = self.descriptor_chain(head)?;
        if chain.len() < 2 {
            return Err("virtio-block descriptor chain was too short".to_string());
        }
        let header = chain[0];
        let status = *chain
            .last()
            .ok_or_else(|| "virtio-block status descriptor was missing".to_string())?;
        if header.flags & VIRTQ_DESC_F_WRITE != 0
            || header.length < 16
            || status.flags & VIRTQ_DESC_F_WRITE == 0
            || status.length == 0
        {
            return Err("virtio-block request descriptor permissions were invalid".to_string());
        }
        let header_bytes = self.read_guest(header.address, 16)?;
        let request_type = u32::from_le_bytes(
            header_bytes[0..4]
                .try_into()
                .map_err(|_| "virtio-block request type was truncated".to_string())?,
        );
        let sector = u64::from_le_bytes(
            header_bytes[8..16]
                .try_into()
                .map_err(|_| "virtio-block request sector was truncated".to_string())?,
        );
        let data = &chain[1..chain.len() - 1];
        let outcome = match request_type {
            VIRTIO_BLK_T_IN => self.read_request(sector, data),
            VIRTIO_BLK_T_OUT => self.write_request(sector, data),
            VIRTIO_BLK_T_FLUSH => self.flush_request(data),
            VIRTIO_BLK_T_GET_ID => self.id_request(data),
            _ => Ok((VIRTIO_BLK_S_UNSUPP, 0)),
        };
        let (status_byte, written) = match outcome {
            Ok(outcome) => outcome,
            Err(_) => {
                self.update_report(|report| report.errors = report.errors.saturating_add(1));
                (VIRTIO_BLK_S_IOERR, 0)
            }
        };
        self.write_guest(status.address, &[status_byte])?;
        written
            .checked_add(1)
            .ok_or_else(|| "virtio-block used length overflowed".to_string())
    }

    fn descriptor_chain(&self, head: u16) -> Result<Vec<VirtqueueDescriptor>, String> {
        if head >= self.queue.size {
            return Err("virtio-block descriptor head exceeded queue size".to_string());
        }
        let mut chain = Vec::new();
        let mut index = head;
        for _ in 0..self.queue.size {
            let address = self
                .queue
                .descriptor_table
                .checked_add(u64::from(index) * 16)
                .ok_or_else(|| "descriptor table address overflowed".to_string())?;
            let bytes = self.read_userspace(address, 16)?;
            let descriptor = VirtqueueDescriptor {
                address: u64::from_le_bytes(
                    bytes[0..8]
                        .try_into()
                        .map_err(|_| "descriptor address was truncated".to_string())?,
                ),
                length: u32::from_le_bytes(
                    bytes[8..12]
                        .try_into()
                        .map_err(|_| "descriptor length was truncated".to_string())?,
                ),
                flags: read_u16(&bytes, 12)?,
                next: read_u16(&bytes, 14)?,
            };
            if descriptor.flags & VIRTQ_DESC_F_INDIRECT != 0 {
                return Err("unadvertised indirect descriptor was used".to_string());
            }
            chain.push(descriptor);
            if descriptor.flags & VIRTQ_DESC_F_NEXT == 0 {
                return Ok(chain);
            }
            if descriptor.next >= self.queue.size {
                return Err("descriptor next index exceeded queue size".to_string());
            }
            index = descriptor.next;
        }
        Err("virtio-block descriptor chain contained a loop".to_string())
    }

    fn read_request(
        &mut self,
        sector: u64,
        descriptors: &[VirtqueueDescriptor],
    ) -> Result<(u8, u32), String> {
        if descriptors
            .iter()
            .any(|descriptor| descriptor.flags & VIRTQ_DESC_F_WRITE == 0)
        {
            return Err("virtio-block read data descriptor was not writable".to_string());
        }
        let length = descriptor_bytes(descriptors)?;
        let offset = self.checked_backing_range(sector, length)?;
        let mut file_offset = offset;
        for descriptor in descriptors {
            let length = usize::try_from(descriptor.length)
                .map_err(|_| "read descriptor length did not fit usize".to_string())?;
            let mut bytes = vec![0_u8; length];
            read_file_exact_at(&self.backing, &mut bytes, file_offset)?;
            self.write_guest(descriptor.address, &bytes)?;
            file_offset = file_offset
                .checked_add(u64::from(descriptor.length))
                .ok_or_else(|| "read file offset overflowed".to_string())?;
        }
        self.update_report(|report| report.reads = report.reads.saturating_add(1));
        Ok((VIRTIO_BLK_S_OK, length))
    }

    fn write_request(
        &mut self,
        sector: u64,
        descriptors: &[VirtqueueDescriptor],
    ) -> Result<(u8, u32), String> {
        if self.options.read_only
            || descriptors
                .iter()
                .any(|descriptor| descriptor.flags & VIRTQ_DESC_F_WRITE != 0)
        {
            return Err("virtio-block write was not permitted".to_string());
        }
        let length = descriptor_bytes(descriptors)?;
        let offset = self.checked_backing_range(sector, length)?;
        let mut file_offset = offset;
        for descriptor in descriptors {
            let length = usize::try_from(descriptor.length)
                .map_err(|_| "write descriptor length did not fit usize".to_string())?;
            let bytes = self.read_guest(descriptor.address, length)?;
            write_file_all_at(&self.backing, &bytes, file_offset)?;
            file_offset = file_offset
                .checked_add(u64::from(descriptor.length))
                .ok_or_else(|| "write file offset overflowed".to_string())?;
        }
        self.update_report(|report| report.writes = report.writes.saturating_add(1));
        Ok((VIRTIO_BLK_S_OK, 0))
    }

    fn flush_request(&mut self, descriptors: &[VirtqueueDescriptor]) -> Result<(u8, u32), String> {
        if !descriptors.is_empty() || !self.options.advertise_flush {
            return Err("virtio-block flush was not supported".to_string());
        }
        self.backing
            .sync_data()
            .map_err(io_kind("flush test vhost-user backing"))?;
        self.update_report(|report| report.flushes = report.flushes.saturating_add(1));
        Ok((VIRTIO_BLK_S_OK, 0))
    }

    fn id_request(&self, descriptors: &[VirtqueueDescriptor]) -> Result<(u8, u32), String> {
        const DEVICE_ID: &[u8; 20] = b"bangbang-vhost-test0";
        let mut remaining = DEVICE_ID.as_slice();
        let mut written = 0_u32;
        for descriptor in descriptors {
            if descriptor.flags & VIRTQ_DESC_F_WRITE == 0 {
                return Err("virtio-block ID descriptor was not writable".to_string());
            }
            let length = usize::try_from(descriptor.length)
                .map_err(|_| "ID descriptor length did not fit usize".to_string())?;
            let count = length.min(remaining.len());
            self.write_guest(descriptor.address, &remaining[..count])?;
            remaining = &remaining[count..];
            written = written
                .checked_add(
                    u32::try_from(count)
                        .map_err(|_| "ID response length did not fit u32".to_string())?,
                )
                .ok_or_else(|| "ID response length overflowed".to_string())?;
            if remaining.is_empty() {
                break;
            }
        }
        Ok((VIRTIO_BLK_S_OK, written))
    }

    fn checked_backing_range(&self, sector: u64, length: u32) -> Result<u64, String> {
        let offset = sector
            .checked_mul(VIRTIO_BLOCK_SECTOR_SIZE)
            .ok_or_else(|| "virtio-block sector offset overflowed".to_string())?;
        let end = offset
            .checked_add(u64::from(length))
            .ok_or_else(|| "virtio-block request end overflowed".to_string())?;
        if end > self.capacity_bytes {
            return Err("virtio-block request exceeded backing capacity".to_string());
        }
        Ok(offset)
    }

    fn signal_call(&self) -> Result<(), String> {
        let descriptor = self
            .queue
            .call
            .as_ref()
            .ok_or_else(|| "active queue lost its call endpoint".to_string())?
            .as_raw_fd();
        let notification = [0_u8; NOTIFICATION_SIZE];
        loop {
            // SAFETY: The received call descriptor is a live pipe writer and
            // the fixed notification is readable for this synchronous write.
            let result = unsafe {
                libc::write(descriptor, notification.as_ptr().cast(), notification.len())
            };
            if result == NOTIFICATION_SIZE as isize {
                self.update_report(|report| report.calls = report.calls.saturating_add(1));
                return Ok(());
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                match error.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => return Ok(()),
                    _ => {
                        return Err(format!(
                            "test vhost-user call write failed: {:?}",
                            error.kind()
                        ));
                    }
                }
            }
            return Err("vhost-user call notification was partial".to_string());
        }
    }

    fn read_guest(&self, address: u64, length: usize) -> Result<Vec<u8>, String> {
        self.read_memory(address, length, false)
    }

    fn write_guest(&self, address: u64, source: &[u8]) -> Result<(), String> {
        self.write_memory(address, source, false)
    }

    fn read_userspace(&self, address: u64, length: usize) -> Result<Vec<u8>, String> {
        self.read_memory(address, length, true)
    }

    fn write_userspace(&self, address: u64, source: &[u8]) -> Result<(), String> {
        self.write_memory(address, source, true)
    }

    fn read_userspace_u16(&self, address: u64) -> Result<u16, String> {
        let bytes = self.read_userspace(address, 2)?;
        read_u16(&bytes, 0)
    }

    fn read_memory(&self, address: u64, length: usize, userspace: bool) -> Result<Vec<u8>, String> {
        let mut bytes = vec![0_u8; length];
        let (region, offset) = self.translate(address, length, userspace)?;
        region.read_at(offset, &mut bytes);
        Ok(bytes)
    }

    fn write_memory(&self, address: u64, source: &[u8], userspace: bool) -> Result<(), String> {
        let (region, offset) = self.translate(address, source.len(), userspace)?;
        region.write_at(offset, source);
        Ok(())
    }

    fn translate(
        &self,
        address: u64,
        length: usize,
        userspace: bool,
    ) -> Result<(&SharedMemoryRegion, usize), String> {
        self.memory
            .iter()
            .find_map(|region| {
                let offset = if userspace {
                    region.userspace_offset(address, length)
                } else {
                    region.guest_offset(address, length)
                }?;
                Some((region, offset))
            })
            .ok_or_else(|| "vhost-user address was outside shared guest memory".to_string())
    }
}

fn descriptor_bytes(descriptors: &[VirtqueueDescriptor]) -> Result<u32, String> {
    descriptors.iter().try_fold(0_u32, |total, descriptor| {
        total
            .checked_add(descriptor.length)
            .ok_or_else(|| "virtio-block data length overflowed".to_string())
    })
}

fn read_file_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> Result<(), String> {
    while !bytes.is_empty() {
        let read = file
            .read_at(bytes, offset)
            .map_err(io_kind("read test vhost-user backing"))?;
        if read == 0 {
            return Err("test vhost-user backing reached unexpected EOF".to_string());
        }
        offset = offset
            .checked_add(read as u64)
            .ok_or_else(|| "test vhost-user read offset overflowed".to_string())?;
        bytes = &mut bytes[read..];
    }
    Ok(())
}

fn write_file_all_at(file: &File, mut bytes: &[u8], mut offset: u64) -> Result<(), String> {
    while !bytes.is_empty() {
        let written = file
            .write_at(bytes, offset)
            .map_err(io_kind("write test vhost-user backing"))?;
        if written == 0 {
            return Err("test vhost-user backing accepted a zero-length write".to_string());
        }
        offset = offset
            .checked_add(written as u64)
            .ok_or_else(|| "test vhost-user write offset overflowed".to_string())?;
        bytes = &bytes[written..];
    }
    Ok(())
}
