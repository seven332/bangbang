// clippy.toml allows these in #[test] bodies, but integration-test helpers are
// ordinary functions in the test crate. Keep the exception scoped here.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use bangbang::host_network;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos_arm64 {
    use std::env;
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use bangbang_session::credential::{CredentialPrefix, CredentialTarget};
    use bangbang_session::macos::credential::{attest_current_process, transition_process};
    use serde_json::{Value, json};
    use sha2::{Digest as _, Sha256};

    use crate::host_network::vmnet::{
        StartedVmnetPacketIoBackend, SystemVmnetInterfaceBackend, VmnetInterfaceConfig,
        VmnetPacketAvailableCallback, VmnetPacketIoBackend, VmnetReadPacket, VmnetWritePacket,
    };

    const BANGBANG_ENV: &str = "BANGBANG_ELEVATED_VMNET_BANGBANG";
    const KERNEL_ENV: &str = "BANGBANG_ELEVATED_VMNET_KERNEL";
    const ROOTFS_ENV: &str = "BANGBANG_ELEVATED_VMNET_ROOTFS";
    const ROOTFS_SIDECAR_ENV: &str = "BANGBANG_ELEVATED_VMNET_ROOTFS_SIDECAR";
    const TARGET_UID_ENV: &str = "BANGBANG_ELEVATED_VMNET_TARGET_UID";
    const TARGET_GID_ENV: &str = "BANGBANG_ELEVATED_VMNET_TARGET_GID";
    const KERNEL_NAME: &str = "vmlinux-6.1.155";
    const ROOTFS_NAME: &str = "ubuntu-24.04-512M-direct-boot-v111.ext4";
    const SIDECAR_NAME: &str = "ubuntu-24.04-512M-direct-boot-v111.ext4.bangbang.json";
    const BANGBANG_NAME: &str = "bangbang";
    const API_SOCKET_NAME: &str = "api.sock";
    const SERIAL_NAME: &str = "serial.out";
    const CONTROL_NAME: &str = "control.bin";
    const CONTROL_MAGIC: &[u8; 8] = b"BBEVNET2";
    const CONTROL_BYTES: usize = 512;
    const CONTROL_PREFIX_BYTES: usize = 64;
    const CONTROL_DIGEST_BYTES: usize = 32;
    const CONTROL_VERSION: u16 = 2;
    const CONTROL_SHARED_MODE: u8 = 1;
    const CONTROL_DHCP_ROUTER_ENDPOINT: u8 = 1;
    const TCP_REQUEST_MAGIC: &[u8; 8] = b"BBVREQ1\0";
    const TCP_RESPONSE_MAGIC: &[u8; 8] = b"BBVRES1\0";
    const TCP_RECORD_BYTES: usize = 40;
    const BEGIN_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_BEGIN";
    const SUCCESS_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_OK";
    const FAILURE_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_";
    const FAILURE_CONTROL_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_CONTROL";
    const FAILURE_INTERFACE_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_INTERFACE";
    const FAILURE_DHCP_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_DHCP";
    const FAILURE_CONFIGURE_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_CONFIGURE";
    const FAILURE_TCP_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_TCP";
    const FAILURE_CLEANUP_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_CLEANUP";
    const FAILURE_INTERNAL_MARKER: &[u8] = b"BANGBANG_ELEVATED_VMNET_CERTIFICATION_FAIL_INTERNAL";
    const BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init bangbang.elevated-vmnet-certification=1";
    const HTTP_LIMIT: usize = 64 * 1024;
    const SERIAL_LIMIT: usize = 64 * 1024;
    const KERNEL_LIMIT: u64 = 256 * 1024 * 1024;
    const ROOTFS_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
    const BINARY_LIMIT: u64 = 512 * 1024 * 1024;
    const SIDECAR_LIMIT: u64 = 64 * 1024;
    const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
    const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
    const GUEST_TIMEOUT: Duration = Duration::from_secs(90);
    const FIXTURE_TIMEOUT: Duration = Duration::from_secs(60);
    const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum EvidenceError {
        Authority,
        Artifact,
        Process,
        Api,
        DenialStatus,
        DenialCategory,
        Vmnet,
        Credential,
        Control,
        Fixture,
        Guest,
        GuestProcessExit,
        GuestBootTimeout,
        GuestCertificationTimeout,
        GuestControl,
        GuestInterface,
        GuestDhcp,
        GuestConfigure,
        GuestTcp,
        GuestCleanup,
        GuestInternal,
        Cleanup,
    }

    impl EvidenceError {
        const fn name(self) -> &'static str {
            match self {
                Self::Authority => "authority",
                Self::Artifact => "artifact",
                Self::Process => "process",
                Self::Api => "api",
                Self::DenialStatus => "denial-status",
                Self::DenialCategory => "denial-category",
                Self::Vmnet => "vmnet",
                Self::Credential => "credential",
                Self::Control => "control",
                Self::Fixture => "fixture",
                Self::Guest => "guest",
                Self::GuestProcessExit => "guest-process-exit",
                Self::GuestBootTimeout => "guest-boot-timeout",
                Self::GuestCertificationTimeout => "guest-certification-timeout",
                Self::GuestControl => "guest-control",
                Self::GuestInterface => "guest-interface",
                Self::GuestDhcp => "guest-dhcp",
                Self::GuestConfigure => "guest-configure",
                Self::GuestTcp => "guest-tcp",
                Self::GuestCleanup => "guest-cleanup",
                Self::GuestInternal => "guest-internal",
                Self::Cleanup => "cleanup",
            }
        }
    }

    type EvidenceResult<T> = Result<T, EvidenceError>;

    struct Artifacts {
        bangbang: PathBuf,
        kernel: PathBuf,
        rootfs: PathBuf,
    }

    struct RunRoot {
        path: PathBuf,
    }

    impl RunRoot {
        fn create() -> EvidenceResult<Self> {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| EvidenceError::Artifact)?
                .as_nanos();
            let path = env::temp_dir().join(format!("bbe-{}-{nanos}", std::process::id()));
            std::os::unix::fs::DirBuilderExt::mode(&mut fs::DirBuilder::new(), 0o700)
                .create(&path)
                .map_err(|_| EvidenceError::Artifact)?;
            let metadata = fs::symlink_metadata(&path).map_err(|_| EvidenceError::Artifact)?;
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.mode() & 0o077 != 0
            {
                return Err(EvidenceError::Artifact);
            }
            Ok(Self { path })
        }

        fn child(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for RunRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    struct Product {
        child: Option<Child>,
        process_group: libc::pid_t,
        socket: PathBuf,
    }

    impl Product {
        fn start(binary: &Path, socket: &Path) -> EvidenceResult<Self> {
            if os_path_len(socket) >= 104 {
                return Err(EvidenceError::Artifact);
            }
            let mut command = Command::new(binary);
            command
                .arg("--api-sock")
                .arg(socket)
                .arg("--id")
                .arg("elevated-vmnet-evidence")
                .current_dir("/")
                .env_clear()
                .env("LANG", "C")
                .env("LC_ALL", "C")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            let mut child = command.spawn().map_err(|_| EvidenceError::Process)?;
            let process_group = libc::pid_t::try_from(child.id()).map_err(|_| {
                let _ = child.kill();
                let _ = child.wait();
                EvidenceError::Process
            })?;
            let deadline = Instant::now() + STARTUP_TIMEOUT;
            loop {
                if UnixStream::connect(socket).is_ok() {
                    break;
                }
                match child.try_wait() {
                    Ok(Some(_status)) => {
                        let _ = child.wait();
                        return Err(EvidenceError::Process);
                    }
                    Ok(None) => {}
                    Err(_) => {
                        terminate_group(process_group, libc::SIGKILL);
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(EvidenceError::Process);
                    }
                }
                if Instant::now() >= deadline {
                    terminate_group(process_group, libc::SIGKILL);
                    let _ = child.wait();
                    return Err(EvidenceError::Process);
                }
                thread::sleep(Duration::from_millis(10));
            }
            Ok(Self {
                child: Some(child),
                process_group,
                socket: socket.to_path_buf(),
            })
        }

        fn terminate(mut self) -> EvidenceResult<()> {
            let Some(mut child) = self.child.take() else {
                return Err(EvidenceError::Cleanup);
            };
            terminate_group(self.process_group, libc::SIGTERM);
            let status = wait_child(&mut child, SHUTDOWN_TIMEOUT)?;
            if !status.success() {
                return Err(EvidenceError::Cleanup);
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while self.socket.exists() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if self.socket.exists() || process_group_exists(self.process_group)? {
                return Err(EvidenceError::Cleanup);
            }
            Ok(())
        }

        fn is_running(&mut self) -> EvidenceResult<bool> {
            let child = self.child.as_mut().ok_or(EvidenceError::Process)?;
            child
                .try_wait()
                .map(|status| status.is_none())
                .map_err(|_| EvidenceError::Process)
        }
    }

    impl Drop for Product {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                terminate_group(self.process_group, libc::SIGKILL);
                let _ = child.wait();
            }
        }
    }

    fn os_path_len(path: &Path) -> usize {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().len()
    }

    fn terminate_group(process_group: libc::pid_t, signal: libc::c_int) {
        if process_group <= 0 {
            return;
        }
        // SAFETY: The negative, checked process-group id targets only the child
        // group created by `CommandExt::process_group(0)`; no pointer is used.
        let _ = unsafe { libc::kill(-process_group, signal) };
    }

    fn process_group_exists(process_group: libc::pid_t) -> EvidenceResult<bool> {
        // SAFETY: Signal zero performs a synchronous existence/permission query
        // against the checked child process-group id and dereferences no pointer.
        if unsafe { libc::kill(-process_group, 0) } == 0 {
            return Ok(true);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            _ => Err(EvidenceError::Cleanup),
        }
    }

    fn wait_child(child: &mut Child, timeout: Duration) -> EvidenceResult<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => {}
                Err(_) => {
                    if let Ok(process_group) = libc::pid_t::try_from(child.id()) {
                        terminate_group(process_group, libc::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(EvidenceError::Cleanup);
                }
            }
            if Instant::now() >= deadline {
                if let Ok(process_group) = libc::pid_t::try_from(child.id()) {
                    terminate_group(process_group, libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(EvidenceError::Cleanup);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn current_ids() -> (u32, u32, u32, u32) {
        // SAFETY: Darwin credential getters have no pointer or ownership contract.
        unsafe {
            (
                libc::getuid(),
                libc::geteuid(),
                libc::getgid(),
                libc::getegid(),
            )
        }
    }

    fn require_root() -> EvidenceResult<()> {
        if current_ids() == (0, 0, 0, 0) {
            Ok(())
        } else {
            Err(EvidenceError::Authority)
        }
    }

    fn require_non_root() -> EvidenceResult<()> {
        let (uid, euid, gid, egid) = current_ids();
        if uid != 0 && euid == uid && gid != 0 && egid == gid {
            Ok(())
        } else {
            Err(EvidenceError::Authority)
        }
    }

    fn parse_target(name: &str) -> EvidenceResult<u32> {
        let value = env::var(name).map_err(|_| EvidenceError::Authority)?;
        if value.is_empty()
            || value.starts_with('0')
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(EvidenceError::Authority);
        }
        value
            .parse::<u32>()
            .ok()
            .filter(|value| *value != 0)
            .ok_or(EvidenceError::Authority)
    }

    fn env_path(name: &str, expected_name: &str, maximum: u64) -> EvidenceResult<PathBuf> {
        let path = PathBuf::from(env::var_os(name).ok_or(EvidenceError::Artifact)?);
        if !path.is_absolute()
            || path.file_name().and_then(|value| value.to_str()) != Some(expected_name)
        {
            return Err(EvidenceError::Artifact);
        }
        let metadata = fs::symlink_metadata(&path).map_err(|_| EvidenceError::Artifact)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() == 0
            || metadata.len() > maximum
            || metadata.nlink() != 1
            || metadata.mode() & 0o022 != 0
        {
            return Err(EvidenceError::Artifact);
        }
        Ok(path)
    }

    fn artifacts() -> EvidenceResult<Artifacts> {
        let bangbang = env_path(BANGBANG_ENV, BANGBANG_NAME, BINARY_LIMIT)?;
        let kernel = env_path(KERNEL_ENV, KERNEL_NAME, KERNEL_LIMIT)?;
        let rootfs = env_path(ROOTFS_ENV, ROOTFS_NAME, ROOTFS_LIMIT)?;
        let sidecar = env_path(ROOTFS_SIDECAR_ENV, SIDECAR_NAME, SIDECAR_LIMIT)?;
        let sidecar_bytes = fs::read(sidecar).map_err(|_| EvidenceError::Artifact)?;
        let document: Value =
            serde_json::from_slice(&sidecar_bytes).map_err(|_| EvidenceError::Artifact)?;
        let object = document.as_object().ok_or(EvidenceError::Artifact)?;
        if object.get("schema_version").and_then(Value::as_u64) != Some(1)
            || object.get("variant").and_then(Value::as_str) != Some("direct-boot-v111")
            || object.get("output_size_bytes").and_then(Value::as_u64)
                != fs::metadata(&rootfs).ok().map(|metadata| metadata.len())
            || object
                .get("output_sha256")
                .and_then(Value::as_str)
                .is_none_or(|digest| {
                    digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
        {
            return Err(EvidenceError::Artifact);
        }
        let status = Command::new("/usr/bin/codesign")
            .arg("--verify")
            .arg("--strict")
            .arg(&bangbang)
            .current_dir("/")
            .env_clear()
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| EvidenceError::Artifact)?;
        if !status.success() {
            return Err(EvidenceError::Artifact);
        }
        Ok(Artifacts {
            bangbang,
            kernel,
            rootfs,
        })
    }

    fn create_private_file(path: &Path) -> EvidenceResult<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| EvidenceError::Artifact)
    }

    fn secure_nonce() -> EvidenceResult<[u8; 32]> {
        let mut source = File::open("/dev/urandom").map_err(|_| EvidenceError::Control)?;
        for _ in 0..8 {
            let mut nonce = [0_u8; 32];
            source
                .read_exact(&mut nonce)
                .map_err(|_| EvidenceError::Control)?;
            if nonce.iter().any(|byte| *byte != 0) {
                return Ok(nonce);
            }
        }
        Err(EvidenceError::Control)
    }

    fn write_control(path: &Path, port: u16, nonce: &[u8; 32]) -> EvidenceResult<()> {
        if port == 0 || nonce.iter().all(|byte| *byte == 0) {
            return Err(EvidenceError::Control);
        }
        let mut data = [0_u8; CONTROL_BYTES];
        data[..8].copy_from_slice(CONTROL_MAGIC);
        data[8..10].copy_from_slice(&CONTROL_VERSION.to_be_bytes());
        data[10] = CONTROL_SHARED_MODE;
        data[11] = CONTROL_DHCP_ROUTER_ENDPOINT;
        data[16..18].copy_from_slice(&port.to_be_bytes());
        data[18..50].copy_from_slice(nonce);
        let digest = Sha256::digest(&data[..CONTROL_PREFIX_BYTES]);
        data[CONTROL_PREFIX_BYTES..CONTROL_PREFIX_BYTES + CONTROL_DIGEST_BYTES]
            .copy_from_slice(&digest);
        let mut file = create_private_file(path)?;
        file.write_all(&data).map_err(|_| EvidenceError::Control)?;
        file.sync_all().map_err(|_| EvidenceError::Control)
    }

    struct HttpResponse {
        status: u16,
        bytes: Vec<u8>,
    }

    fn http_json(socket: &Path, path: &str, body: &Value) -> EvidenceResult<HttpResponse> {
        let body = serde_json::to_vec(body).map_err(|_| EvidenceError::Api)?;
        if body.len() > HTTP_LIMIT {
            return Err(EvidenceError::Api);
        }
        let request = format!(
            "PUT {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let mut stream = UnixStream::connect(socket).map_err(|_| EvidenceError::Api)?;
        stream
            .set_read_timeout(Some(HTTP_TIMEOUT))
            .map_err(|_| EvidenceError::Api)?;
        stream
            .set_write_timeout(Some(HTTP_TIMEOUT))
            .map_err(|_| EvidenceError::Api)?;
        stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.write_all(&body))
            .map_err(|_| EvidenceError::Api)?;
        let mut bytes = Vec::new();
        stream
            .take((HTTP_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| EvidenceError::Api)?;
        if bytes.len() > HTTP_LIMIT {
            return Err(EvidenceError::Api);
        }
        let line_end = bytes
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or(EvidenceError::Api)?;
        let line = std::str::from_utf8(&bytes[..line_end]).map_err(|_| EvidenceError::Api)?;
        let mut fields = line.split(' ');
        if fields.next() != Some("HTTP/1.1") {
            return Err(EvidenceError::Api);
        }
        let status = fields
            .next()
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or(EvidenceError::Api)?;
        Ok(HttpResponse { status, bytes })
    }

    fn require_no_content(response: HttpResponse) -> EvidenceResult<()> {
        if response.status == 204
            && response
                .bytes
                .windows(19)
                .any(|window| window == b"Content-Length: 0\r\n")
        {
            Ok(())
        } else {
            Err(EvidenceError::Api)
        }
    }

    fn configure(
        product: &Product,
        artifacts: &Artifacts,
        root: &RunRoot,
        control: &Path,
    ) -> EvidenceResult<HttpResponse> {
        let socket = &product.socket;
        require_no_content(http_json(
            socket,
            "/machine-config",
            &json!({"vcpu_count": 1, "mem_size_mib": 256}),
        )?)?;
        require_no_content(http_json(
            socket,
            "/boot-source",
            &json!({
                "kernel_image_path": artifacts.kernel,
                "boot_args": BOOT_ARGS,
            }),
        )?)?;
        require_no_content(http_json(
            socket,
            "/drives/rootfs",
            &json!({
                "drive_id": "rootfs",
                "path_on_host": artifacts.rootfs,
                "is_root_device": true,
                "is_read_only": true,
            }),
        )?)?;
        require_no_content(http_json(
            socket,
            "/drives/control",
            &json!({
                "drive_id": "control",
                "path_on_host": control,
                "is_root_device": false,
                "is_read_only": true,
            }),
        )?)?;
        require_no_content(http_json(
            socket,
            "/serial",
            &json!({"serial_out_path": root.child(SERIAL_NAME)}),
        )?)?;
        require_no_content(http_json(
            socket,
            "/network-interfaces/eth0",
            &json!({"iface_id": "eth0", "host_dev_name": "vmnet:shared"}),
        )?)?;
        http_json(socket, "/actions", &json!({"action_type": "InstanceStart"}))
    }

    fn read_serial(path: &Path) -> EvidenceResult<Vec<u8>> {
        let metadata = fs::symlink_metadata(path).map_err(|_| EvidenceError::Guest)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > SERIAL_LIMIT as u64
        {
            return Err(EvidenceError::Guest);
        }
        let file = File::open(path).map_err(|_| EvidenceError::Guest)?;
        let mut bytes = Vec::new();
        file.take((SERIAL_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| EvidenceError::Guest)?;
        if bytes.len() > SERIAL_LIMIT {
            return Err(EvidenceError::Guest);
        }
        Ok(bytes)
    }

    fn contains_marker_line(bytes: &[u8], marker: &[u8]) -> bool {
        bytes
            .windows(marker.len())
            .enumerate()
            .any(|(offset, window)| {
                if window != marker || (offset != 0 && bytes[offset - 1] != b'\n') {
                    return false;
                }
                let end = offset + marker.len();
                bytes.get(end) == Some(&b'\n')
                    || (bytes.get(end) == Some(&b'\r') && bytes.get(end + 1) == Some(&b'\n'))
            })
    }

    fn wait_for_guest(product: &mut Product, path: &Path) -> EvidenceResult<()> {
        let deadline = Instant::now() + GUEST_TIMEOUT;
        loop {
            if !product.is_running()? {
                return Err(EvidenceError::GuestProcessExit);
            }
            let bytes = read_serial(path)?;
            let failure_categories = [
                (FAILURE_CONTROL_MARKER, EvidenceError::GuestControl),
                (FAILURE_INTERFACE_MARKER, EvidenceError::GuestInterface),
                (FAILURE_DHCP_MARKER, EvidenceError::GuestDhcp),
                (FAILURE_CONFIGURE_MARKER, EvidenceError::GuestConfigure),
                (FAILURE_TCP_MARKER, EvidenceError::GuestTcp),
                (FAILURE_CLEANUP_MARKER, EvidenceError::GuestCleanup),
                (FAILURE_INTERNAL_MARKER, EvidenceError::GuestInternal),
            ];
            for (marker, error) in failure_categories {
                if contains_marker_line(&bytes, marker) {
                    return Err(error);
                }
            }
            if bytes
                .windows(FAILURE_MARKER.len())
                .any(|window| window == FAILURE_MARKER)
            {
                return Err(EvidenceError::Guest);
            }
            let began = contains_marker_line(&bytes, BEGIN_MARKER);
            let succeeded = contains_marker_line(&bytes, SUCCESS_MARKER);
            if began && succeeded {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(if began {
                    EvidenceError::GuestCertificationTimeout
                } else {
                    EvidenceError::GuestBootTimeout
                });
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn handle_fixture_connection(mut stream: TcpStream, nonce: &[u8; 32]) -> EvidenceResult<bool> {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| EvidenceError::Fixture)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| EvidenceError::Fixture)?;
        let mut request = [0_u8; TCP_RECORD_BYTES];
        if stream.read_exact(&mut request).is_err() {
            return Ok(false);
        }
        let mut trailing = [0_u8; 1];
        if stream.read(&mut trailing).ok() != Some(0)
            || request[..8] != *TCP_REQUEST_MAGIC
            || request[8..] != nonce[..]
        {
            return Ok(false);
        }
        let mut response = [0_u8; TCP_RECORD_BYTES];
        response[..8].copy_from_slice(TCP_RESPONSE_MAGIC);
        response[8..].copy_from_slice(nonce);
        stream
            .write_all(&response)
            .map_err(|_| EvidenceError::Fixture)?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|_| EvidenceError::Fixture)?;
        Ok(true)
    }

    fn run_fixture(
        listener: TcpListener,
        nonce: [u8; 32],
        cancelled: &AtomicBool,
    ) -> EvidenceResult<()> {
        listener
            .set_nonblocking(true)
            .map_err(|_| EvidenceError::Fixture)?;
        let deadline = Instant::now() + FIXTURE_TIMEOUT;
        let mut accepted = 0_u8;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(EvidenceError::Fixture);
            }
            match listener.accept() {
                Ok((stream, _peer)) => {
                    accepted = accepted.checked_add(1).ok_or(EvidenceError::Fixture)?;
                    if accepted > 16 {
                        return Err(EvidenceError::Fixture);
                    }
                    if handle_fixture_connection(stream, &nonce)? {
                        return Ok(());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err(EvidenceError::Fixture),
            }
            if Instant::now() >= deadline {
                return Err(EvidenceError::Fixture);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn run_ordinary_denial() -> EvidenceResult<()> {
        require_non_root()?;
        let artifacts = artifacts()?;
        let root = RunRoot::create()?;
        create_private_file(&root.child(SERIAL_NAME))?;
        let listener = TcpListener::bind(("0.0.0.0", 0)).map_err(|_| EvidenceError::Fixture)?;
        let port = listener
            .local_addr()
            .map_err(|_| EvidenceError::Fixture)?
            .port();
        let nonce = secure_nonce()?;
        let control = root.child(CONTROL_NAME);
        write_control(&control, port, &nonce)?;
        let product = Product::start(&artifacts.bangbang, &root.child(API_SOCKET_NAME))?;
        let response = configure(&product, &artifacts, &root, &control)?;
        let denied_status = response.status == 400;
        let vmnet_start_failed = response
            .bytes
            .windows(b"vmnet_start_interface failed".len())
            .any(|window| window == b"vmnet_start_interface failed");
        let denied_category = vmnet_start_failed
            && (response
                .bytes
                .windows(b"VMNET_NOT_AUTHORIZED".len())
                .any(|window| window == b"VMNET_NOT_AUTHORIZED")
                || response
                    .bytes
                    .windows(b"VMNET_FAILURE".len())
                    .any(|window| window == b"VMNET_FAILURE"));
        product.terminate()?;
        drop(listener);
        if !denied_status {
            return Err(EvidenceError::DenialStatus);
        }
        if !denied_category {
            return Err(EvidenceError::DenialCategory);
        }
        Ok(())
    }

    fn run_dropped_owner() -> EvidenceResult<()> {
        require_root()?;
        let target_uid = parse_target(TARGET_UID_ENV)?;
        let target_gid = parse_target(TARGET_GID_ENV)?;
        let (mut backend, mut interface) = StartedVmnetPacketIoBackend::start(
            SystemVmnetInterfaceBackend::new(),
            &VmnetInterfaceConfig::shared(),
        )
        .map_err(|_| EvidenceError::Vmnet)?;
        let (packet_size, realized_mac) = {
            let parameters = backend.parameters();
            let packet_size = parameters
                .packet_buffer_size()
                .filter(|size| (60..=65_536).contains(size))
                .ok_or(EvidenceError::Vmnet)?;
            if parameters.effective_mtu() < 68
                || parameters
                    .read_max_packets()
                    .is_some_and(|count| count == 0)
                || parameters
                    .write_max_packets()
                    .is_some_and(|count| count == 0)
            {
                return Err(EvidenceError::Vmnet);
            }
            (packet_size, parameters.realized_mac().octets())
        };
        let target =
            CredentialTarget::new(target_uid, target_gid).map_err(|_| EvidenceError::Credential)?;
        let transition = transition_process(target).map_err(|_| EvidenceError::Credential)?;
        if transition.prefix() != CredentialPrefix::Irreversible {
            return Err(EvidenceError::Credential);
        }
        attest_current_process(target).map_err(|_| EvidenceError::Credential)?;
        backend
            .enable_packet_available_callback(VmnetPacketAvailableCallback::new(
                |_estimated_packets| {},
            ))
            .map_err(|_| EvidenceError::Vmnet)?;
        let mut frame = [0_u8; 60];
        frame[..6].fill(0xff);
        frame[6..12].copy_from_slice(&realized_mac);
        frame[12..14].copy_from_slice(&[0x88, 0xb5]);
        let mut write = VmnetWritePacket::new(&frame).map_err(|_| EvidenceError::Vmnet)?;
        backend
            .write_packet(&mut interface, &mut write)
            .map_err(|_| EvidenceError::Vmnet)?;
        let mut buffer = vec![0_u8; packet_size];
        let mut read = VmnetReadPacket::new(&mut buffer).map_err(|_| EvidenceError::Vmnet)?;
        backend
            .read_packet(&mut interface, &mut read)
            .map_err(|_| EvidenceError::Vmnet)?;
        backend.stop().map_err(|_| EvidenceError::Cleanup)?;
        Ok(())
    }

    fn run_elevated_guest() -> EvidenceResult<()> {
        require_root()?;
        let artifacts = artifacts()?;
        let root = RunRoot::create()?;
        create_private_file(&root.child(SERIAL_NAME))?;
        let listener = TcpListener::bind(("0.0.0.0", 0)).map_err(|_| EvidenceError::Fixture)?;
        let port = listener
            .local_addr()
            .map_err(|_| EvidenceError::Fixture)?
            .port();
        let nonce = secure_nonce()?;
        let control = root.child(CONTROL_NAME);
        write_control(&control, port, &nonce)?;
        let mut product = Product::start(&artifacts.bangbang, &root.child(API_SOCKET_NAME))?;
        require_no_content(configure(&product, &artifacts, &root, &control)?)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let fixture_cancelled = Arc::clone(&cancelled);
        let fixture = thread::Builder::new()
            .name("elevated-vmnet-fixture".to_owned())
            .spawn(move || run_fixture(listener, nonce, &fixture_cancelled))
            .map_err(|_| EvidenceError::Fixture)?;
        let guest_result = wait_for_guest(&mut product, &root.child(SERIAL_NAME));
        cancelled.store(true, Ordering::Release);
        let fixture_result = fixture.join().map_err(|_| EvidenceError::Fixture)?;
        let cleanup_result = product.terminate();
        guest_result?;
        fixture_result?;
        cleanup_result?;
        Ok(())
    }

    fn assert_result(result: EvidenceResult<()>) {
        if let Err(error) = result {
            panic!(
                "bangbang elevated vmnet evidence failed category={}",
                error.name()
            );
        }
    }

    #[test]
    fn ordinary_user_vmnet_start_is_denied() {
        assert_result(run_ordinary_denial());
    }

    #[test]
    fn dropped_owner_retains_bounded_vmnet_io() {
        assert_result(run_dropped_owner());
    }

    #[test]
    fn elevated_direct_guest_uses_shared_vmnet() {
        assert_result(run_elevated_guest());
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod unsupported {
    #[test]
    fn ordinary_user_vmnet_start_is_denied() {
        panic!("bangbang elevated vmnet evidence failed category=platform");
    }

    #[test]
    fn dropped_owner_retains_bounded_vmnet_io() {
        panic!("bangbang elevated vmnet evidence failed category=platform");
    }

    #[test]
    fn elevated_direct_guest_uses_shared_vmnet() {
        panic!("bangbang elevated vmnet evidence failed category=platform");
    }
}
