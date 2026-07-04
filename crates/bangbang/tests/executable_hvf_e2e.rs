// clippy.toml allows these in #[test] bodies, but integration-test helpers are
// ordinary functions in the test crate. Keep the exception scoped to this test.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

mod support;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos_arm64 {
    use std::fs;
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use crate::support::{
        BangbangProcess, TestDir, assert_bad_request_response, assert_clean_shutdown,
        assert_no_content_response, assert_ok_response, assert_response_contains, http_get,
        http_put_json, json_string, path_text,
    };

    const BANGBANG_GUEST_KERNEL_PATH_ENV: &str = "BANGBANG_GUEST_KERNEL_PATH";
    const BANGBANG_GUEST_INITRD_PATH_ENV: &str = "BANGBANG_GUEST_INITRD_PATH";
    const BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV: &str = "BANGBANG_GUEST_EXT4_ROOTFS_PATH";
    const BLOCK_WRITE_MARKER: &[u8] = b"BANGBANG_BLOCK_WRITE_OK";
    const DIRECT_ROOTFS_BLOCK_MARKER: &[u8] = b"BANGBANG_DIRECT_ROOTFS_BLOCK_OK";
    const GUEST_BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 rdinit=/init";
    const DIRECT_ROOTFS_BOOT_ARGS: &str =
        "console=ttyS0 reboot=k panic=1 quiet loglevel=1 init=/bangbang-direct-rootfs-init";
    const GUEST_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);

    #[test]
    fn signed_executable_starts_instance_and_guest_writes_block_marker() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let backing_path = test_dir.path().join("data.img");
        let serial_output_path = test_dir.path().join("serial.out");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let initrd_path = env_path(BANGBANG_GUEST_INITRD_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&backing_path);
        create_empty_file(&serial_output_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let instance_info = http_get(&socket_path, "/");
        assert_ok_response(&instance_info, "GET / before InstanceStart");

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(&machine_response, "PUT /machine-config");

        let kernel_path_json = json_string(path_text(&kernel_path));
        let initrd_path_json = json_string(path_text(&initrd_path));
        let boot_args_json = json_string(GUEST_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "initrd_path":{initrd_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source");

        let backing_path_json = json_string(path_text(&backing_path));
        let drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let drive_response = http_put_json(&socket_path, "/drives/data", &drive_body);
        assert_no_content_response(&drive_response, "PUT /drives/data");

        let serial_output_path_json = json_string(path_text(&serial_output_path));
        let serial_body = format!(r#"{{"serial_out_path":{serial_output_path_json}}}"#);
        let serial_response = http_put_json(&socket_path, "/serial", &serial_body);
        assert_no_content_response(&serial_response, "PUT /serial");

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(&start_response, "PUT /actions InstanceStart");

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(&running_instance_info, "GET / after InstanceStart");
        assert_response_contains(
            &running_instance_info,
            &format!(r#""id":"{instance_id}""#),
            "GET / after InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after InstanceStart",
        );

        let flush_metrics_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"FlushMetrics"}"#,
        );
        assert_no_content_response(&flush_metrics_response, "PUT /actions FlushMetrics");

        let second_start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_bad_request_response(&second_start_response, "PUT /actions second InstanceStart");
        assert_response_contains(
            &second_start_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: InstanceStart"}"#,
            "PUT /actions second InstanceStart",
        );

        let post_start_machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_bad_request_response(
            &post_start_machine_response,
            "PUT /machine-config after InstanceStart",
        );
        assert_response_contains(
            &post_start_machine_response,
            r#"{"fault_message":"The requested operation is not supported in Running state: PutMachineConfig"}"#,
            "PUT /machine-config after InstanceStart",
        );

        if let Err(err) =
            wait_for_file_prefix_marker(&backing_path, BLOCK_WRITE_MARKER, GUEST_EXECUTION_TIMEOUT)
        {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "guest did not write block marker through signed bangbang executable: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        if let Err(err) = wait_for_file_contains_marker(
            &serial_output_path,
            BLOCK_WRITE_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "guest serial output file did not contain block marker through signed bangbang executable: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang");
    }

    #[test]
    fn signed_executable_boots_direct_rootfs_and_writes_block_marker() {
        let test_dir = TestDir::new();
        let socket_path = test_dir.path().join("api.socket");
        let data_backing_path = test_dir.path().join("data.img");
        let kernel_path = env_path(BANGBANG_GUEST_KERNEL_PATH_ENV);
        let rootfs_path = env_path(BANGBANG_GUEST_EXT4_ROOTFS_PATH_ENV);
        let instance_id = test_dir.instance_id();

        create_zeroed_block_backing(&data_backing_path);

        let mut bangbang = BangbangProcess::start(&socket_path, &instance_id);

        let machine_response = http_put_json(
            &socket_path,
            "/machine-config",
            r#"{"vcpu_count":1,"mem_size_mib":256}"#,
        );
        assert_no_content_response(&machine_response, "PUT /machine-config direct rootfs");

        let kernel_path_json = json_string(path_text(&kernel_path));
        let boot_args_json = json_string(DIRECT_ROOTFS_BOOT_ARGS);
        let boot_body = format!(
            r#"{{
                "kernel_image_path":{kernel_path_json},
                "boot_args":{boot_args_json}
            }}"#
        );
        let boot_response = http_put_json(&socket_path, "/boot-source", &boot_body);
        assert_no_content_response(&boot_response, "PUT /boot-source direct rootfs");

        let rootfs_path_json = json_string(path_text(&rootfs_path));
        let rootfs_body = format!(
            r#"{{
                "drive_id":"rootfs",
                "path_on_host":{rootfs_path_json},
                "is_root_device":true,
                "is_read_only":true
            }}"#
        );
        let rootfs_response = http_put_json(&socket_path, "/drives/rootfs", &rootfs_body);
        assert_no_content_response(&rootfs_response, "PUT /drives/rootfs direct rootfs");

        let data_backing_path_json = json_string(path_text(&data_backing_path));
        let data_drive_body = format!(
            r#"{{
                "drive_id":"data",
                "path_on_host":{data_backing_path_json},
                "is_root_device":false,
                "is_read_only":false
            }}"#
        );
        let data_drive_response = http_put_json(&socket_path, "/drives/data", &data_drive_body);
        assert_no_content_response(&data_drive_response, "PUT /drives/data direct rootfs");

        let start_response = http_put_json(
            &socket_path,
            "/actions",
            r#"{"action_type":"InstanceStart"}"#,
        );
        assert_no_content_response(&start_response, "PUT /actions InstanceStart direct rootfs");

        let running_instance_info = http_get(&socket_path, "/");
        assert_ok_response(
            &running_instance_info,
            "GET / after direct rootfs InstanceStart",
        );
        assert_response_contains(
            &running_instance_info,
            r#""state":"Running""#,
            "GET / after direct rootfs InstanceStart",
        );

        if let Err(err) = wait_for_file_prefix_marker(
            &data_backing_path,
            DIRECT_ROOTFS_BLOCK_MARKER,
            GUEST_EXECUTION_TIMEOUT,
        ) {
            let output = bangbang.force_stop_and_collect();
            panic!(
                "direct rootfs guest did not write block marker through signed bangbang executable: {err}; status: {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status, output.stdout, output.stderr
            );
        }

        assert_clean_shutdown(bangbang.terminate(), &socket_path, "bangbang direct rootfs");
    }

    fn env_path(name: &str) -> PathBuf {
        match std::env::var_os(name) {
            Some(value) if value.is_empty() => panic!("{name} must not be empty"),
            Some(value) => PathBuf::from(value),
            None => panic!("{name} must be set"),
        }
    }

    fn create_zeroed_block_backing(path: &Path) {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("guest block backing should create");
        file.set_len(bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE)
            .expect("guest block backing should be one sector");
    }

    fn create_empty_file(path: &Path) {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("empty test output file should create");
    }

    fn wait_for_file_prefix_marker(
        path: &Path,
        marker: &[u8],
        timeout: Duration,
    ) -> Result<(), String> {
        let file = fs::File::open(path).map_err(|err| {
            format!(
                "failed to open block backing {} for marker wait: {err}",
                path.display()
            )
        })?;
        if file_starts_with_marker(path, marker)? {
            return Ok(());
        }

        let kqueue = Kqueue::new()?;
        kqueue.watch_writes(&file)?;
        let started_at = Instant::now();

        loop {
            if file_starts_with_marker(path, marker)? {
                return Ok(());
            }

            let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
                return Err(format!(
                    "timed out after {:?} waiting for marker {:?} in {}",
                    timeout,
                    String::from_utf8_lossy(marker),
                    path.display()
                ));
            };

            kqueue.wait_for_write(remaining)?;
        }
    }

    fn wait_for_file_contains_marker(
        path: &Path,
        marker: &[u8],
        timeout: Duration,
    ) -> Result<(), String> {
        let file = fs::File::open(path).map_err(|err| {
            format!(
                "failed to open serial output {} for marker wait: {err}",
                path.display()
            )
        })?;
        if file_contains_marker(path, marker)? {
            return Ok(());
        }

        let kqueue = Kqueue::new()?;
        kqueue.watch_writes(&file)?;
        let started_at = Instant::now();

        loop {
            if file_contains_marker(path, marker)? {
                return Ok(());
            }

            let Some(remaining) = timeout.checked_sub(started_at.elapsed()) else {
                return Err(format!(
                    "timed out after {:?} waiting for marker {:?} in {}",
                    timeout,
                    String::from_utf8_lossy(marker),
                    path.display()
                ));
            };

            kqueue.wait_for_write(remaining)?;
        }
    }

    fn file_starts_with_marker(path: &Path, marker: &[u8]) -> Result<bool, String> {
        let mut file = fs::File::open(path)
            .map_err(|err| format!("failed to open block backing {}: {err}", path.display()))?;
        let mut buffer = vec![0; marker.len()];
        let bytes_read = file
            .read(&mut buffer)
            .map_err(|err| format!("failed to read block backing {}: {err}", path.display()))?;

        Ok(bytes_read == marker.len() && buffer == marker)
    }

    fn file_contains_marker(path: &Path, marker: &[u8]) -> Result<bool, String> {
        if marker.is_empty() {
            return Ok(true);
        }

        let bytes = fs::read(path)
            .map_err(|err| format!("failed to read serial output {}: {err}", path.display()))?;

        Ok(bytes.windows(marker.len()).any(|window| window == marker))
    }

    #[derive(Debug)]
    struct Kqueue {
        fd: libc::c_int,
    }

    impl Kqueue {
        fn new() -> Result<Self, String> {
            // SAFETY: `kqueue` has no preconditions and returns either a new file
            // descriptor or -1 with errno set.
            let fd = unsafe { libc::kqueue() };
            if fd >= 0 {
                Ok(Self { fd })
            } else {
                Err(format!(
                    "failed to create kqueue: {}",
                    std::io::Error::last_os_error()
                ))
            }
        }

        fn watch_writes(&self, file: &fs::File) -> Result<(), String> {
            use std::os::fd::AsRawFd;

            let ident = libc::uintptr_t::try_from(file.as_raw_fd())
                .map_err(|_| "block backing file descriptor did not fit uintptr_t".to_string())?;
            let change = libc::kevent {
                ident,
                filter: libc::EVFILT_VNODE,
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags: libc::NOTE_WRITE | libc::NOTE_EXTEND,
                data: 0,
                udata: std::ptr::null_mut(),
            };

            // SAFETY: `self.fd` is an open kqueue descriptor, `change` points to
            // one initialized event, and no output events are requested.
            let result = unsafe {
                libc::kevent(
                    self.fd,
                    &change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            };
            if result == 0 {
                Ok(())
            } else {
                Err(format!(
                    "failed to register block backing kqueue watch: {}",
                    std::io::Error::last_os_error()
                ))
            }
        }

        fn wait_for_write(&self, timeout: Duration) -> Result<(), String> {
            let timeout = duration_to_timespec(timeout)?;
            let mut event = libc::kevent {
                ident: 0,
                filter: 0,
                flags: 0,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            };

            loop {
                // SAFETY: `self.fd` is an open kqueue descriptor, `event`
                // points to writable storage for one event, and `timeout`
                // lives for the call.
                let result =
                    unsafe { libc::kevent(self.fd, std::ptr::null(), 0, &mut event, 1, &timeout) };

                if result > 0 {
                    return Ok(());
                }
                if result == 0 {
                    return Err("timed out waiting for block backing write event".to_string());
                }

                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::Interrupted {
                    return Err(format!(
                        "failed while waiting for block backing write: {err}"
                    ));
                }
            }
        }
    }

    impl Drop for Kqueue {
        fn drop(&mut self) {
            // SAFETY: `self.fd` was returned by `kqueue` and is owned by this
            // guard.
            let _ = unsafe { libc::close(self.fd) };
        }
    }

    fn duration_to_timespec(duration: Duration) -> Result<libc::timespec, String> {
        let tv_sec = libc::time_t::try_from(duration.as_secs())
            .map_err(|_| format!("duration {duration:?} does not fit time_t"))?;
        let tv_nsec = libc::c_long::from(duration.subsec_nanos());

        Ok(libc::timespec { tv_sec, tv_nsec })
    }
}
