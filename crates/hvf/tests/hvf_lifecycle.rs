// clippy.toml allows these in #[test] bodies, but integration-test helpers are
// ordinary functions in the test crate. Keep the exception scoped to this test.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[cfg(target_os = "macos")]
#[path = "../../../tests/support/macos_virtual_block.rs"]
mod macos_virtual_block;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static HVF_LIFECYCLE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static NEXT_HVF_TEST_FILE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VTIMER_WRITABLE_CONTROL_MASK: u64 = 0b11;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VTIMER_TEST_OFFSET: u64 = 0x1234_5678_9abc_def0;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VTIMER_TEST_COMPARE_VALUE: u64 = 0xfedc_ba98_7654_3210;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_TEST_CNTKCTL_EL1: u64 = 3;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_TEST_CNTP_CTL_EL0: u64 = 2;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_TEST_CNTP_CVAL_EL0: u64 = 0x1234_5678;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_WRITABLE_CONTROL_MASK: u64 = 0b11;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_ISTATUS_MASK: u64 = 0b100;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_DEFINED_CONTROL_MASK: u64 = 0b111;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn is_app_sandbox_hvf_lifecycle_replay() -> bool {
    std::env::current_exe()
        .ok()
        .is_some_and(|executable| is_app_sandbox_hvf_lifecycle_executable(&executable))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn is_app_sandbox_hvf_lifecycle_executable(executable: &std::path::Path) -> bool {
    let Some(macos) = executable.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    let Some(bundle) = contents.parent() else {
        return false;
    };
    executable.file_name() == Some(std::ffi::OsStr::new("hvf_lifecycle"))
        && macos.file_name() == Some(std::ffi::OsStr::new("MacOS"))
        && contents.file_name() == Some(std::ffi::OsStr::new("Contents"))
        && bundle.file_name() == Some(std::ffi::OsStr::new("BangbangHvfLifecycleSandbox.app"))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn app_sandbox_hvf_lifecycle_replay_requires_exact_bundle_layout() {
    use std::path::Path;

    assert!(is_app_sandbox_hvf_lifecycle_executable(Path::new(
        "/tmp/BangbangHvfLifecycleSandbox.app/Contents/MacOS/hvf_lifecycle"
    )));
    assert!(!is_app_sandbox_hvf_lifecycle_executable(Path::new(
        "/tmp/BangbangHvfLifecycleSandbox.app/target/hvf_lifecycle"
    )));
    assert!(!is_app_sandbox_hvf_lifecycle_executable(Path::new(
        "/tmp/BangbangHvfLifecycleSandbox.app/Contents/MacOS/hvf_lifecycle-deadbeef"
    )));
    assert!(!is_app_sandbox_hvf_lifecycle_executable(Path::new(
        "/tmp/Other.app/Contents/MacOS/hvf_lifecycle"
    )));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn temporary_virtual_block_fixture_preserves_rw_ro_bytes_and_exact_cleanup() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use crate::macos_virtual_block::{MacosVirtualBlock, MacosVirtualBlockAccess};
    use bangbang_runtime::block::{
        BlockDeviceControl, BlockDeviceControlError, BlockDeviceGeometry, BlockFileBacking,
        BlockFileBackingError, SnapshotBlockFileBackingError, VirtioBlockDeviceId,
    };

    #[derive(Debug)]
    struct RejectInspectControl;

    impl BlockDeviceControl for RejectInspectControl {
        fn inspect(
            &self,
            _file: &std::fs::File,
        ) -> Result<BlockDeviceGeometry, BlockDeviceControlError> {
            Err(BlockDeviceControlError::new(
                std::io::ErrorKind::PermissionDenied,
            ))
        }

        fn synchronize_cache(&self, _file: &std::fs::File) -> Result<(), BlockDeviceControlError> {
            panic!("failed inspection must not publish a backing")
        }
    }

    #[derive(Debug)]
    struct RejectSyncControl {
        geometry: BlockDeviceGeometry,
    }

    impl BlockDeviceControl for RejectSyncControl {
        fn inspect(
            &self,
            _file: &std::fs::File,
        ) -> Result<BlockDeviceGeometry, BlockDeviceControlError> {
            Ok(self.geometry)
        }

        fn synchronize_cache(&self, _file: &std::fs::File) -> Result<(), BlockDeviceControlError> {
            Err(BlockDeviceControlError::new(
                std::io::ErrorKind::PermissionDenied,
            ))
        }
    }

    #[derive(Debug)]
    struct ChangingGeometryControl {
        initial: BlockDeviceGeometry,
        changed: BlockDeviceGeometry,
        inspections: AtomicUsize,
    }

    impl BlockDeviceControl for ChangingGeometryControl {
        fn inspect(
            &self,
            _file: &std::fs::File,
        ) -> Result<BlockDeviceGeometry, BlockDeviceControlError> {
            if self.inspections.fetch_add(1, AtomicOrdering::Relaxed) == 0 {
                Ok(self.initial)
            } else {
                Ok(self.changed)
            }
        }

        fn synchronize_cache(&self, _file: &std::fs::File) -> Result<(), BlockDeviceControlError> {
            Ok(())
        }
    }

    // The wrapper already runs this test as a directly signed binary. Its App Sandbox replay
    // cannot launch the test-only `hdiutil` fixture process and is covered by #1465 instead.
    if is_app_sandbox_hvf_lifecycle_replay() {
        return;
    }
    let _guard = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut media = MacosVirtualBlock::create(MacosVirtualBlockAccess::ReadWrite)
        .expect("temporary virtual block media should create");
    let marker = b"bangbang-virtual-block";
    let device = media
        .device_path()
        .expect("attached device path should exist")
        .to_path_buf();
    let identity = media
        .identity()
        .expect("attached identity should be available");

    assert_eq!(
        media.len().expect("media length should read"),
        4 * 1024 * 1024
    );
    assert_ne!(identity.target_device(), 0);
    assert_eq!(
        u64::from(
            media
                .logical_block_size()
                .expect("logical block size should read")
        ) * media.block_count().expect("block count should read"),
        media.len().expect("media length should read")
    );
    assert!(format!("{media:?}").contains("<redacted>"));
    assert!(!format!("{media:?}").contains(device.to_string_lossy().as_ref()));
    assert!(!format!("{identity:?}").contains(&identity.target_device().to_string()));

    let logical_block_size = media
        .logical_block_size()
        .expect("logical block size should read");
    let block_count = media.block_count().expect("block count should read");
    let geometry = BlockDeviceGeometry::new(logical_block_size, block_count)
        .expect("fixture geometry should validate");
    let inspect_error = BlockFileBacking::from_file_with_block_device_control(
        media
            .open_descriptor()
            .expect("inspect-failure block descriptor should open"),
        false,
        Arc::new(RejectInspectControl),
    )
    .expect_err("control inspection failure should reject adoption");
    assert!(matches!(
        inspect_error,
        BlockFileBackingError::ReadBlockGeometry { source }
            if source.kind() == std::io::ErrorKind::PermissionDenied
    ));

    let rejecting_sync = BlockFileBacking::from_file_with_block_device_control(
        media
            .open_descriptor()
            .expect("sync-failure block descriptor should open"),
        false,
        Arc::new(RejectSyncControl { geometry }),
    )
    .expect("valid injected inspection should adopt the real block descriptor");
    assert!(matches!(
        rejecting_sync.flush(),
        Err(BlockFileBackingError::FlushBlockDevice { source })
            if source.kind() == std::io::ErrorKind::PermissionDenied
    ));
    drop(rejecting_sync);

    let changed_geometry = BlockDeviceGeometry::new(
        logical_block_size,
        block_count
            .checked_sub(1)
            .expect("fixture should have more than one logical block"),
    )
    .expect("changed fixture geometry should remain structurally valid");
    let changing = BlockFileBacking::from_file_with_block_device_control(
        media
            .open_descriptor()
            .expect("geometry-change block descriptor should open"),
        false,
        Arc::new(ChangingGeometryControl {
            initial: geometry,
            changed: changed_geometry,
            inspections: AtomicUsize::new(0),
        }),
    )
    .expect("initial injected geometry should adopt the real block descriptor");
    assert_eq!(
        changing.snapshot_identity(),
        Err(SnapshotBlockFileBackingError::InvalidMetadata)
    );
    drop(changing);

    {
        let backing = BlockFileBacking::from_file(
            media
                .open_descriptor()
                .expect("read-write block descriptor should open"),
            false,
        )
        .expect("runtime should adopt read-write block descriptor");
        assert!(backing.kind().is_block_device());
        assert_eq!(
            backing.kind().logical_block_size(),
            Some(
                media
                    .logical_block_size()
                    .expect("logical block size should read")
            )
        );
        assert_eq!(
            backing.len(),
            media.len().expect("media length should read")
        );
        assert_eq!(
            backing.device_id(),
            VirtioBlockDeviceId::from_bytes(
                format!(
                    "{}{}{}",
                    identity.device(),
                    identity.target_device(),
                    identity.inode()
                )
                .as_bytes()
            )
        );
        let backing_debug = format!("{backing:?}");
        assert!(backing_debug.contains("<redacted>"));
        assert!(!backing_debug.contains(&backing.len().to_string()));
        assert!(!backing_debug.contains(&identity.target_device().to_string()));
        backing
            .write_at(4096, marker)
            .expect("runtime block write should succeed");
        backing
            .flush()
            .expect("runtime block cache synchronization should succeed");
        let mut readback = vec![0_u8; marker.len()];
        backing
            .read_at(4096, &mut readback)
            .expect("runtime block read should succeed");
        assert_eq!(readback, marker);
        let capture_identity = backing
            .snapshot_identity()
            .expect("runtime block capture identity should revalidate");
        assert!(capture_identity.kind().is_block_device());
        assert_eq!(
            capture_identity.target_device(),
            Some(identity.target_device())
        );
        assert_eq!(
            capture_identity.block_count(),
            Some(media.block_count().expect("block count should read"))
        );
    }
    assert_eq!(
        media
            .read_at(4096, marker.len())
            .expect("read-write attachment should read marker"),
        marker
    );
    media
        .reattach(MacosVirtualBlockAccess::ReadOnly)
        .expect("media should reattach read-only");
    assert_eq!(
        media
            .read_at(4096, marker.len())
            .expect("read-only attachment should read persisted marker"),
        marker
    );
    let read_only_backing = BlockFileBacking::from_file(
        media
            .open_descriptor()
            .expect("read-only block descriptor should open"),
        true,
    )
    .expect("runtime should adopt read-only block descriptor");
    let mut readback = vec![0_u8; marker.len()];
    read_only_backing
        .read_at(4096, &mut readback)
        .expect("runtime read-only block read should succeed");
    assert_eq!(readback, marker);
    assert!(matches!(
        read_only_backing.write_at(4096, b"rejected"),
        Err(BlockFileBackingError::ReadOnlyWrite)
    ));
    drop(read_only_backing);
    assert!(media.write_at(4096, b"rejected").is_err());
    media
        .cleanup()
        .expect("temporary virtual block media should detach and clean up");
    assert!(!device.exists());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn temporary_virtual_block_traverses_async_flush_and_capture_ready_owner() {
    use std::sync::Arc;
    use std::time::Instant;

    use crate::macos_virtual_block::{MacosVirtualBlock, MacosVirtualBlockAccess};
    use bangbang_hvf::{HvfArm64BootSessionConfig, OwnedHvfArm64BootSession};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::async_executor::{
        BlockAsyncApplyOutcome, BlockAsyncCompletionDisposition, BlockAsyncDrive,
        BlockAsyncDriveGeneration, BlockAsyncExecutor, BlockAsyncOperation,
        BlockAsyncOperationKind, BlockAsyncOperationStatus, BlockAsyncRequestIdentity,
        BlockAsyncScheduleOutcome,
    };
    use bangbang_runtime::block::{
        BlockCaptureIoEngine, BlockDeviceControl, BlockDeviceControlError, BlockDeviceGeometry,
        BlockFileBacking, BlockFileBackingError, BlockMmioLayout, DriveCacheType, DriveConfigInput,
        DriveIoEngine, DriveLiveUpdateMode,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
    };
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::storage_capture::{CaptureReadyStorageConfigs, StorageTransportState};
    use bangbang_runtime::vsock::VsockMmioLayout;

    #[derive(Debug)]
    struct RejectReplacementInspectControl;

    impl BlockDeviceControl for RejectReplacementInspectControl {
        fn inspect(
            &self,
            _file: &std::fs::File,
        ) -> Result<BlockDeviceGeometry, BlockDeviceControlError> {
            Err(BlockDeviceControlError::new(
                std::io::ErrorKind::PermissionDenied,
            ))
        }

        fn synchronize_cache(&self, _file: &std::fs::File) -> Result<(), BlockDeviceControlError> {
            panic!("failed replacement inspection must not publish a backing")
        }
    }

    // The wrapper already runs this test as a directly signed binary. Its App Sandbox replay
    // cannot launch the test-only `hdiutil` fixture process and is covered by #1465 instead.
    if is_app_sandbox_hvf_lifecycle_replay() {
        return;
    }
    let _guard = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let media = MacosVirtualBlock::create(MacosVirtualBlockAccess::ReadWrite)
        .expect("temporary virtual block media should create");
    let device = media
        .device_path()
        .expect("attached device path should exist")
        .to_path_buf();
    let media_identity = media
        .identity()
        .expect("attached identity should be available");
    let media_len = media.len().expect("media length should read");
    let logical_block_size = media
        .logical_block_size()
        .expect("logical block size should read");
    let block_count = media.block_count().expect("block count should read");

    let backing = Arc::new(
        BlockFileBacking::from_file(
            media
                .open_descriptor()
                .expect("async block descriptor should open"),
            false,
        )
        .expect("runtime should adopt the async block descriptor"),
    );
    let expected_device_id = backing.device_id();
    let mut executor = BlockAsyncExecutor::new().expect("production async executor should start");
    let completion_fd = executor
        .completion_fd()
        .expect("production async executor should expose a completion descriptor");
    let mut drive = BlockAsyncDrive::new(
        BlockAsyncDriveGeneration::new(1),
        Arc::clone(&backing),
        DriveCacheType::Writeback,
        executor.handle(),
    )
    .expect("block drive should bind to the production async executor");
    let layout = GuestMemoryLayout::new(vec![
        GuestMemoryRange::new(GuestAddress::new(0), 16 * 1024)
            .expect("async flush guest range should validate"),
    ])
    .expect("async flush guest layout should validate");
    let mut memory =
        GuestMemory::allocate(&layout).expect("async flush guest memory should allocate");
    let operation = drive
        .admit(BlockAsyncOperation::flush(BlockAsyncRequestIdentity::new(
            0,
            0,
            GuestAddress::new(0),
        )))
        .expect("real block flush should admit");
    assert!(matches!(
        drive
            .schedule_one(&memory)
            .expect("real block flush should schedule"),
        BlockAsyncScheduleOutcome::Submitted {
            operation: submitted,
            chunk_offset: 0,
            chunk_len: 0,
        } if submitted == operation
    ));
    let mut readiness = libc::pollfd {
        fd: completion_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: One initialized pollfd is writable for the bounded wait.
    let ready = unsafe { libc::poll(&raw mut readiness, 1, 5_000) };
    assert_eq!(
        ready, 1,
        "real block flush should complete before the deadline"
    );
    assert_ne!(readiness.revents & libc::POLLIN, 0);
    let completion = executor
        .try_recv_completion()
        .expect("production completion queue should remain connected")
        .expect("readiness should publish the real block flush completion");
    let BlockAsyncApplyOutcome::Completed(applied) = drive
        .apply_completion(
            &mut memory,
            completion,
            BlockAsyncCompletionDisposition::Apply,
        )
        .expect("real block flush completion should apply")
    else {
        panic!("real block flush should complete in one host operation");
    };
    assert_eq!(applied.kind(), BlockAsyncOperationKind::Flush);
    assert_eq!(applied.status(), BlockAsyncOperationStatus::Success);
    assert_eq!(applied.bytes_transferred(), 0);
    drop(drive);
    executor
        .shutdown()
        .expect("production async executor should stop");
    drop(executor);
    drop(backing);

    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("block-capture-ready-kernel", &image)
        .expect("block capture kernel should create");
    let root = TempFile::new_len("block-capture-ready-root", 4096)
        .expect("regular block capture root should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("block capture boot source should configure");
    controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", root.path(), true)
                .with_is_read_only(true)
                .with_io_engine(DriveIoEngine::Sync),
        ))
        .expect("regular capture root should configure");
    controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("blockdata", "blockdata", device.as_path(), false)
                .with_is_read_only(false)
                .with_cache_type(DriveCacheType::Writeback)
                .with_io_engine(DriveIoEngine::Async),
        ))
        .expect("real block data drive should configure");
    let session_config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut session = OwnedHvfArm64BootSession::new(&controller, session_config)
        .expect("signed block capture session should prepare");
    let configs = CaptureReadyStorageConfigs::new(controller.drive_configs().to_vec(), Vec::new());
    let retry_guard = session
        .quiesce_limiter_retry_wakeups()
        .expect("block capture retry publishers should quiesce");
    let first = session
        .capture_ready_storage_state_at(&configs, &retry_guard, Instant::now())
        .expect("real block drive should become capture-ready");
    let second = session
        .capture_ready_storage_state_at(&configs, &retry_guard, Instant::now())
        .expect("real block Async admission should reopen for a second capture");
    assert_eq!(first.block_devices().len(), 2);
    assert_eq!(
        first.block_devices()[1].config(),
        &controller.drive_configs()[1]
    );
    assert!(
        first.block_devices()[0]
            .device()
            .backing()
            .kind()
            .is_regular_file()
    );
    assert!(matches!(
        first.block_devices()[1].transport(),
        StorageTransportState::Mmio(_)
    ));
    let first_device = first.block_devices()[1].device();
    let second_device = second.block_devices()[1].device();
    let captured_backing = first_device.backing();
    assert!(captured_backing.kind().is_block_device());
    assert_eq!(
        captured_backing.target_device(),
        Some(media_identity.target_device())
    );
    assert_eq!(captured_backing.len(), media_len);
    assert_eq!(
        captured_backing.kind().logical_block_size(),
        Some(logical_block_size)
    );
    assert_eq!(captured_backing.block_count(), Some(block_count));
    assert_eq!(
        first_device.config_space().capacity_sectors(),
        media_len / 512
    );
    assert_eq!(first_device.device_id(), expected_device_id);
    assert_eq!(second_device.backing(), captured_backing);
    assert_eq!(second_device.device_id(), first_device.device_id());
    let BlockCaptureIoEngine::Async(first_async) = first_device.io_engine() else {
        panic!("real block drive should retain Async continuation state");
    };
    let BlockCaptureIoEngine::Async(second_async) = second_device.io_engine() else {
        panic!("second real block capture should retain Async continuation state");
    };
    assert_eq!(second_async.generation(), first_async.generation());
    assert!(first_async.admission_stopped());
    assert_eq!(first_async.owned_operations(), 0);
    assert_eq!(first_async.parked_host_completions(), 0);
    assert_eq!(first_async.final_completions(), 0);

    let failed_replacement = BlockFileBacking::from_file_with_block_device_control(
        media
            .open_descriptor()
            .expect("failed replacement block descriptor should open"),
        false,
        Arc::new(RejectReplacementInspectControl),
    )
    .expect_err("failed replacement inspection should reject before publication");
    assert!(matches!(
        failed_replacement,
        BlockFileBackingError::ReadBlockGeometry { source }
            if source.kind() == std::io::ErrorKind::PermissionDenied
    ));
    let after_failed_replacement = session
        .capture_ready_storage_state_at(&configs, &retry_guard, Instant::now())
        .expect("failed replacement preparation must leave the prior owner capture-ready");
    let failed_device = after_failed_replacement.block_devices()[1].device();
    assert_eq!(failed_device.backing(), captured_backing);
    assert_eq!(failed_device.device_id(), expected_device_id);
    let BlockCaptureIoEngine::Async(failed_async) = failed_device.io_engine() else {
        panic!("failed replacement must retain the prior Async engine");
    };
    assert_eq!(failed_async.generation(), second_async.generation());

    drop(retry_guard);
    let replacement_config = controller.drive_configs()[1].clone();
    let replacement_backing = BlockFileBacking::from_file(
        media
            .open_descriptor()
            .expect("successful replacement block descriptor should open"),
        false,
    )
    .expect("successful replacement block backing should prepare");
    session
        .update_live_block_device_with_opened(
            &replacement_config,
            Some(replacement_backing),
            None,
            DriveLiveUpdateMode::Replacement,
        )
        .expect("real block replacement should commit through the MMIO owner");
    let replacement_guard = session
        .quiesce_limiter_retry_wakeups()
        .expect("replacement capture retry publishers should quiesce");
    let after_successful_replacement = session
        .capture_ready_storage_state_at(&configs, &replacement_guard, Instant::now())
        .expect("successfully replaced real block drive should become capture-ready");
    let replacement_device = after_successful_replacement.block_devices()[1].device();
    assert_eq!(replacement_device.backing(), captured_backing);
    assert_eq!(replacement_device.device_id(), expected_device_id);
    assert_eq!(
        replacement_device.config_space().capacity_sectors(),
        media_len / 512
    );
    let BlockCaptureIoEngine::Async(replacement_async) = replacement_device.io_engine() else {
        panic!("successful block replacement should retain the configured Async engine");
    };
    assert_ne!(replacement_async.generation(), second_async.generation());
    assert!(replacement_async.admission_stopped());
    assert_eq!(replacement_async.owned_operations(), 0);
    drop(replacement_guard);
    session
        .shutdown()
        .expect("signed block capture session should shut down");
    drop(session);
    media
        .cleanup()
        .expect("temporary virtual block media should detach and clean up");
    assert!(!device.exists());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn mach_counter_sample() -> u64 {
    // SAFETY: `mach_absolute_time` takes no arguments and returns one monotonic sample.
    unsafe { mach_absolute_time() }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn mach_ticks_for(duration: std::time::Duration) -> Option<u64> {
    let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: `info` is a valid, exclusively borrowed output object for the call.
    assert_eq!(unsafe { mach_timebase_info(&mut info) }, 0);
    assert_ne!(info.numer, 0);
    assert_ne!(info.denom, 0);

    let nanoseconds = duration.as_nanos();
    let numerator = nanoseconds.checked_mul(u128::from(info.denom))?;
    let rounded = numerator.checked_add(u128::from(info.numer) - 1)?;
    let ticks = rounded / u128::from(info.numer);
    u64::try_from(ticks).ok()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_normalized_timer_restore_equivalent(
    source: bangbang_hvf::HvfArm64SnapshotTimerState,
    recaptured: bangbang_hvf::HvfArm64SnapshotTimerState,
) {
    assert_eq!(
        recaptured.virtual_timer_exit_masked(),
        source.virtual_timer_exit_masked()
    );
    assert_eq!(recaptured.cntkctl_el1(), source.cntkctl_el1());
    assert_eq!(recaptured.virtual_control(), source.virtual_control());
    assert_eq!(
        recaptured.virtual_compare_value(),
        source.virtual_compare_value()
    );
    assert_eq!(recaptured.physical_control(), source.physical_control());

    let virtual_elapsed = recaptured
        .virtual_count()
        .wrapping_sub(source.virtual_count());
    let physical_elapsed = source
        .physical_compare_delta()
        .wrapping_sub(recaptured.physical_compare_delta());
    assert_eq!(
        virtual_elapsed, physical_elapsed,
        "virtual count and physical comparator distance should advance by one shared host-counter interval"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_pstate_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmePstate, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmePstate>, bangbang_hvf::HvfVcpuRunnerError> {
    use bangbang_hvf::HvfVcpuRunnerError;
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::Backend(BackendError::Unsupported(message))) => {
            assert_eq!(
                message,
                "Hypervisor.framework SME state capture requires macOS 15.2 or newer"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::Backend(BackendError::Hypervisor(message))) => {
            assert_eq!(
                message,
                "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_p_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmePRegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmePRegisterState>, bangbang_hvf::HvfVcpuRunnerError> {
    use bangbang_hvf::{HvfArm64VcpuSmePRegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmePRegisterCapture(
            HvfArm64VcpuSmePRegisterCaptureError::StreamingSveModeDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmePRegisterCapture(
            HvfArm64VcpuSmePRegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME configuration queries require macOS 15.2 or newer",
                    "Hypervisor.framework SME P-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmePRegisterCapture(
            HvfArm64VcpuSmePRegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_p_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_z_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmeZRegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmeZRegisterState>, bangbang_hvf::HvfVcpuRunnerError> {
    use bangbang_hvf::{HvfArm64VcpuSmeZRegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmeZRegisterCapture(
            HvfArm64VcpuSmeZRegisterCaptureError::StreamingSveModeDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmeZRegisterCapture(
            HvfArm64VcpuSmeZRegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME configuration queries require macOS 15.2 or newer",
                    "Hypervisor.framework SME Z-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmeZRegisterCapture(
            HvfArm64VcpuSmeZRegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_z_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_za_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmeZaRegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmeZaRegisterState>, bangbang_hvf::HvfVcpuRunnerError>
{
    use bangbang_hvf::{HvfArm64VcpuSmeZaRegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmeZaRegisterCapture(
            HvfArm64VcpuSmeZaRegisterCaptureError::ZaStorageDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmeZaRegisterCapture(
            HvfArm64VcpuSmeZaRegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME configuration queries require macOS 15.2 or newer",
                    "Hypervisor.framework SME ZA-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmeZaRegisterCapture(
            HvfArm64VcpuSmeZaRegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_za_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_zt0_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmeZt0RegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmeZt0RegisterState>, bangbang_hvf::HvfVcpuRunnerError>
{
    use bangbang_hvf::{HvfArm64VcpuSmeZt0RegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmeZt0RegisterCapture(
            HvfArm64VcpuSmeZt0RegisterCaptureError::ZaStorageDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmeZt0RegisterCapture(
            HvfArm64VcpuSmeZt0RegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME ZT0-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmeZt0RegisterCapture(
            HvfArm64VcpuSmeZt0RegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_zt0_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_configuration_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64SmeConfiguration, bangbang_runtime::BackendError>,
) -> Result<Option<bangbang_hvf::HvfArm64SmeConfiguration>, bangbang_runtime::BackendError> {
    use bangbang_runtime::BackendError;

    match result {
        Ok(configuration) => Ok(Some(configuration)),
        Err(BackendError::Hypervisor(message)) => {
            assert_eq!(
                message,
                "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_GUEST_CODE: [u32; 9] = [
    0xd280_0060, // mov x0, #3
    0xd518_e100, // msr CNTKCTL_EL1, x0
    0xd280_0040, // mov x0, #2
    0xd51b_e220, // msr CNTP_CTL_EL0, x0
    0xd28a_cf00, // mov x0, #0x5678
    0xf2a2_4680, // movk x0, #0x1234, lsl #16
    0xd51b_e240, // msr CNTP_CVAL_EL0, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_SP_EL0: u64 = 0x1000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_SP_EL1: u64 = 0x2000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_ELR_EL1: u64 = 0x3000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_SPSR_EL1: u64 = 0x3c5;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_REGISTER_GUEST_CODE: [u32; 9] = [
    0xd282_0000, // mov x0, #0x1000
    0xd518_4100, // msr SP_EL0, x0
    0xd284_0000, // mov x0, #0x2000
    0x9100_001f, // mov sp, x0
    0xd286_0000, // mov x0, #0x3000
    0xd518_4020, // msr ELR_EL1, x0
    0xd280_78a0, // mov x0, #0x3c5
    0xd518_4000, // msr SPSR_EL1, x0
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_AFSR0_EL1: u64 = 0x1111;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_AFSR1_EL1: u64 = 0x2222;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_ESR_EL1: u64 = 0x9600_0045;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_FAR_EL1: u64 = 0x3333_4444;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_PAR_EL1: u64 = 0x5555_6800;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_VBAR_EL1: u64 = 0x1234_5000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_REGISTER_GUEST_CODE: [u32; 18] = [
    0xd282_2220, // mov x0, #0x1111
    0xd518_5100, // msr AFSR0_EL1, x0
    0xd284_4440, // mov x0, #0x2222
    0xd518_5120, // msr AFSR1_EL1, x0
    0xd280_08a0, // mov x0, #0x45
    0xf2b2_c000, // movk x0, #0x9600, lsl #16
    0xd518_5200, // msr ESR_EL1, x0
    0xd288_8880, // mov x0, #0x4444
    0xf2a6_6660, // movk x0, #0x3333, lsl #16
    0xd518_6000, // msr FAR_EL1, x0
    0xd28d_0000, // mov x0, #0x6800
    0xf2aa_aaa0, // movk x0, #0x5555, lsl #16
    0xd518_7400, // msr PAR_EL1, x0
    0xd28a_0000, // mov x0, #0x5000
    0xf2a2_4680, // movk x0, #0x1234, lsl #16
    0xd518_c000, // msr VBAR_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXECUTION_CONTROL_TEST_ACTLR_EL1: u64 = 2;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXECUTION_CONTROL_TEST_CPACR_EL1: u64 = 0x0030_0000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXECUTION_CONTROL_GUEST_CODE: [u32; 6] = [
    0xd280_0040, // mov x0, #2
    0xd518_1020, // msr ACTLR_EL1, x0
    0xd2a0_0600, // mov x0, #0x300000
    0xd518_1040, // msr CPACR_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_TTBR0_EL1: u64 = 0x1234_5000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_TTBR1_EL1: u64 = 0x5678_9000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_TCR_EL1: u64 = 0x10;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_MAIR_EL1: u64 = 0xff44_0400;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_AMAIR_EL1_WRITE: u64 = 0x1122_3344_5566_7788;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_CONTEXTIDR_EL1: u64 = 0xa5a5_5a5a;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_REGISTER_GUEST_CODE: [u32; 24] = [
    0xd538_1000, // mrs x0, SCTLR_EL1
    0xd518_1000, // msr SCTLR_EL1, x0
    0xd503_3fdf, // isb
    0xd28a_0000, // mov x0, #0x5000
    0xf2a2_4680, // movk x0, #0x1234, lsl #16
    0xd518_2000, // msr TTBR0_EL1, x0
    0xd292_0000, // mov x0, #0x9000
    0xf2aa_cf00, // movk x0, #0x5678, lsl #16
    0xd518_2020, // msr TTBR1_EL1, x0
    0xd280_0200, // mov x0, #0x10
    0xd518_2040, // msr TCR_EL1, x0
    0xd280_8000, // mov x0, #0x400
    0xf2bf_e880, // movk x0, #0xff44, lsl #16
    0xd518_a200, // msr MAIR_EL1, x0
    0xd28e_f100, // mov x0, #0x7788
    0xf2aa_acc0, // movk x0, #0x5566, lsl #16
    0xf2c6_6880, // movk x0, #0x3344, lsl #32
    0xf2e2_2440, // movk x0, #0x1122, lsl #48
    0xd518_a300, // msr AMAIR_EL1, x0
    0xd28b_4b40, // mov x0, #0x5a5a
    0xf2b4_b4a0, // movk x0, #0xa5a5, lsl #16
    0xd518_d020, // msr CONTEXTIDR_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APIA_KEY: u128 = (0x2222_u128 << 64) | 0x1111;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APIB_KEY: u128 = (0x4444_u128 << 64) | 0x3333;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APDA_KEY: u128 = (0x6666_u128 << 64) | 0x5555;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APDB_KEY: u128 = (0x8888_u128 << 64) | 0x7777;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APGA_KEY: u128 = (0xaaaa_u128 << 64) | 0x9999;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_KEY_GUEST_CODE: [u32; 22] = [
    0xd282_2220, // mov x0, #0x1111
    0xd518_2100, // msr APIAKeyLo_EL1, x0
    0xd284_4440, // mov x0, #0x2222
    0xd518_2120, // msr APIAKeyHi_EL1, x0
    0xd286_6660, // mov x0, #0x3333
    0xd518_2140, // msr APIBKeyLo_EL1, x0
    0xd288_8880, // mov x0, #0x4444
    0xd518_2160, // msr APIBKeyHi_EL1, x0
    0xd28a_aaa0, // mov x0, #0x5555
    0xd518_2200, // msr APDAKeyLo_EL1, x0
    0xd28c_ccc0, // mov x0, #0x6666
    0xd518_2220, // msr APDAKeyHi_EL1, x0
    0xd28e_eee0, // mov x0, #0x7777
    0xd518_2240, // msr APDBKeyLo_EL1, x0
    0xd291_1100, // mov x0, #0x8888
    0xd518_2260, // msr APDBKeyHi_EL1, x0
    0xd293_3320, // mov x0, #0x9999
    0xd518_2300, // msr APGAKeyLo_EL1, x0
    0xd295_5540, // mov x0, #0xaaaa
    0xd518_2320, // msr APGAKeyHi_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_TEST_TPIDR_EL0: u64 = 0x1111;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_TEST_TPIDRRO_EL0: u64 = 0x2222;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_TEST_TPIDR_EL1: u64 = 0x3333;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_REGISTER_GUEST_CODE: [u32; 7] = [
    0xd282_2220, // mov x0, #0x1111
    0xd51b_d040, // msr TPIDR_EL0, x0
    0xd284_4440, // mov x0, #0x2222
    0xd51b_d060, // msr TPIDRRO_EL0, x0
    0xd286_6660, // mov x0, #0x3333
    0xd518_d080, // msr TPIDR_EL1, x0
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_Q0: [u8; 16] = [0x12; 16];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_Q31: [u8; 16] = [0x34; 16];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_FPCR: u64 = 0x0100_0000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_FPSR: u64 = 0x1f;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_REGISTER_GUEST_CODE: [u32; 10] = [
    0xd2a0_0600, // mov x0, #0x300000
    0xd518_1040, // msr CPACR_EL1, x0
    0xd503_3fdf, // isb
    0x4f00_e640, // movi v0.16b, #0x12
    0x4f01_e69f, // movi v31.16b, #0x34
    0xd2a0_2000, // mov x0, #0x1000000
    0xd51b_4400, // msr FPCR, x0
    0xd280_03e0, // mov x0, #0x1f
    0xd51b_4420, // msr FPSR, x0
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_TEST_PMR_EL1: u64 = 0xa0;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_TEST_BPR0_EL1: u64 = 3;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_TEST_BPR1_EL1: u64 = 4;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_REGISTER_GUEST_CODE: [u32; 15] = [
    0xd538_cca0, // mrs x0, ICC_SRE_EL1
    0xb240_0000, // orr x0, x0, #1
    0xd518_cca0, // msr ICC_SRE_EL1, x0
    0xd503_3fdf, // isb
    0xd280_1400, // mov x0, #0xa0
    0xd518_4600, // msr ICC_PMR_EL1, x0
    0xd280_0060, // mov x0, #3
    0xd518_c860, // msr ICC_BPR0_EL1, x0
    0xd280_0080, // mov x0, #4
    0xd518_cc60, // msr ICC_BPR1_EL1, x0
    0xd280_0020, // mov x0, #1
    0xd518_ccc0, // msr ICC_IGRPEN0_EL1, x0
    0xd518_cce0, // msr ICC_IGRPEN1_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];

// Bare EL1 setup for one message-only SPI. X0 points at four little-endian
// values: distributor base, redistributor base, INTID, and VBAR. The code
// wakes redistributor 0, programs the SPI as Group-1 edge-triggered and routed
// to affinity 0, enables the GICv3 system-register interface, then publishes
// readiness with HVC #0 and waits for the message.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_MSI_GUEST_CODE: [u32; 69] = [
    0xaa00_03f3, // mov x19, x0
    0xf940_0274, // ldr x20, [x19]                 (GICD)
    0xf940_0675, // ldr x21, [x19, #8]             (GICR)
    0xb940_1276, // ldr w22, [x19, #16]            (INTID)
    0xf940_0e77, // ldr x23, [x19, #24]            (VBAR)
    0xd518_c017, // msr VBAR_EL1, x23
    0xd503_3fdf, // isb
    0x9100_52a1, // add x1, x21, #0x14             (GICR_WAKER)
    0xb940_0022, // ldr w2, [x1]
    0x121e_7842, // bic w2, w2, #2                 (ProcessorSleep)
    0xb900_0022, // str w2, [x1]
    0xb940_0022, // ldr w2, [x1]
    0x3717_ffe2, // tbnz w2, #2, .-4               (ChildrenAsleep)
    0x1200_12c3, // and w3, w22, #31
    0x5280_0024, // mov w4, #1
    0x1ac3_2084, // lsl w4, w4, w3
    0x5305_7ec5, // lsr w5, w22, #5
    0x9102_0286, // add x6, x20, #0x80             (GICD_IGROUPR)
    0x8b05_08c6, // add x6, x6, x5, lsl #2
    0xb940_00c7, // ldr w7, [x6]
    0x2a04_00e7, // orr w7, w7, w4
    0xb900_00c7, // str w7, [x6]
    0x1200_0ec3, // and w3, w22, #15
    0x531f_7863, // lsl w3, w3, #1
    0x1100_0463, // add w3, w3, #1
    0x5280_0024, // mov w4, #1
    0x1ac3_2084, // lsl w4, w4, w3
    0x9130_0286, // add x6, x20, #0xc00            (GICD_ICFGR)
    0x5304_7ec5, // lsr w5, w22, #4
    0x8b05_08c6, // add x6, x6, x5, lsl #2
    0xb940_00c7, // ldr w7, [x6]
    0x2a04_00e7, // orr w7, w7, w4
    0xb900_00c7, // str w7, [x6]
    0x9110_0286, // add x6, x20, #0x400            (GICD_IPRIORITYR)
    0x8b16_00c6, // add x6, x6, x22
    0x5280_1007, // mov w7, #0x80
    0x3900_00c7, // strb w7, [x6]
    0x9140_1a86, // add x6, x20, #0x6000           (GICD_IROUTER)
    0x8b16_0cc6, // add x6, x6, x22, lsl #3
    0xf900_00df, // str xzr, [x6]
    0x1200_12c3, // and w3, w22, #31
    0x5280_0024, // mov w4, #1
    0x1ac3_2084, // lsl w4, w4, w3
    0x5305_7ec5, // lsr w5, w22, #5
    0x9104_0286, // add x6, x20, #0x100            (GICD_ISENABLER)
    0x8b05_08c6, // add x6, x6, x5, lsl #2
    0xb900_00c4, // str w4, [x6]
    0xb940_0287, // ldr w7, [x20]                  (GICD_CTLR)
    0x5280_0248, // mov w8, #0x12                  (ARE_NS | EnableGrp1NS)
    0x2a08_00e7, // orr w7, w7, w8
    0xb900_0287, // str w7, [x20]
    0xd503_3f9f, // dsb sy
    0xb940_0287, // ldr w7, [x20]
    0x37ff_ffe7, // tbnz w7, #31, .-4              (RWP)
    0xd538_cca1, // mrs x1, ICC_SRE_EL1
    0xb240_0021, // orr x1, x1, #1
    0xd518_cca1, // msr ICC_SRE_EL1, x1
    0xd503_3fdf, // isb
    0xd280_1fe1, // mov x1, #0xff
    0xd518_4601, // msr ICC_PMR_EL1, x1
    0xd518_cc7f, // msr ICC_BPR1_EL1, xzr
    0xd280_0021, // mov x1, #1
    0xd518_cce1, // msr ICC_IGRPEN1_EL1, x1
    0xd503_3fdf, // isb
    0xd503_42ff, // msr DAIFClr, #2
    0xb940_0681, // ldr w1, [x20, #4]              (GICD_TYPER evidence)
    0xd400_0002, // hvc #0                         (ready)
    0xd503_207f, // wfi
    0x17ff_ffff, // b .-4
];

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_MSI_IRQ_HANDLER: [u32; 4] = [
    0xd538_cc00, // mrs x0, ICC_IAR1_EL1
    0xd518_cc20, // msr ICC_EOIR1_EL1, x0
    0xd400_0022, // hvc #1
    0x1400_0000, // b .
];

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn test_rtc_mmio_layout() -> bangbang_runtime::rtc::RtcMmioLayout {
    bangbang_runtime::rtc::RtcMmioLayout::new(
        bangbang_runtime::memory::GuestAddress::new(0x4000_1000),
        bangbang_runtime::mmio::MmioRegionId::new(3000),
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn queries_arm64_sme_configuration_before_vm_creation() {
    use bangbang_hvf::HvfBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let first =
        assert_sme_configuration_supported_or_unavailable(HvfBackend::arm64_sme_configuration())
            .expect("first SME configuration query should succeed or report unsupported");
    let second =
        assert_sme_configuration_supported_or_unavailable(HvfBackend::arm64_sme_configuration())
            .expect("second SME configuration query should succeed or report unsupported");

    assert!(
        first.is_some() == second.is_some(),
        "SME configuration availability should remain stable on one host"
    );
    if let (Some(first), Some(second)) = (first, second) {
        let first_max_svl_bytes = first.max_svl_bytes();
        let second_max_svl_bytes = second.max_svl_bytes();
        assert!(
            first_max_svl_bytes == second_max_svl_bytes,
            "maximum guest-usable SME SVL should remain stable on one host"
        );
        assert!(
            first == second,
            "SME configuration should remain stable on one host"
        );
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn queries_arm64_default_vcpu_cache_configuration_before_vm_creation() {
    use bangbang_hvf::{HvfArm64VcpuCacheConfiguration, HvfBackend};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let first = HvfBackend::arm64_vcpu_cache_configuration()
        .expect("first default vCPU cache configuration query should succeed");
    let second = HvfBackend::arm64_vcpu_cache_configuration()
        .expect("second default vCPU cache configuration query should succeed");

    let values = |configuration: HvfArm64VcpuCacheConfiguration| {
        [
            configuration.ctr_el0(),
            configuration.clidr_el1(),
            configuration.dczid_el0(),
        ]
    };
    assert!(
        values(first) == values(second),
        "default vCPU cache feature accessors should remain stable on one host"
    );
    assert!(
        first == second,
        "default vCPU cache configuration should remain stable on one host"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn queries_arm64_default_vcpu_cache_geometry_before_vm_creation() {
    use bangbang_hvf::HvfBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let first = HvfBackend::arm64_vcpu_cache_geometry()
        .expect("first default vCPU cache geometry query should succeed");
    let second = HvfBackend::arm64_vcpu_cache_geometry()
        .expect("second default vCPU cache geometry query should succeed");

    assert!(
        first.data_or_unified_ccsidr_el1() == second.data_or_unified_ccsidr_el1()
            && first.instruction_ccsidr_el1() == second.instruction_ccsidr_el1(),
        "default vCPU CCSIDR accessors should remain stable on one host"
    );
    assert!(
        first == second,
        "default vCPU cache geometry should remain stable on one host"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn creates_and_destroys_hvf_vcpu() {
    use bangbang_hvf::{HvfBackend, HvfRegister, HvfSystemRegister};
    use bangbang_runtime::BackendError;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let mut vcpu = backend.create_vcpu().expect("vCPU should be created");
        assert_eq!(
            vcpu.exit_snapshot(),
            Err(BackendError::InvalidState("vCPU has not exited yet"))
        );
        vcpu.set_register(HvfRegister::X0, 0x1234)
            .expect("vCPU register should be set");
        assert_eq!(
            vcpu.get_register(HvfRegister::X0)
                .expect("vCPU register should be read"),
            0x1234
        );
        let original_vtimer_mask = vcpu
            .get_vtimer_mask()
            .expect("original vCPU vtimer mask should be read");
        let original_vtimer_offset = vcpu
            .get_vtimer_offset()
            .expect("original vCPU vtimer offset should be read");
        let original_vtimer_control = vcpu
            .get_system_register(HvfSystemRegister::CNTV_CTL_EL0)
            .expect("original vCPU vtimer control should be read");
        let original_vtimer_compare_value = vcpu
            .get_system_register(HvfSystemRegister::CNTV_CVAL_EL0)
            .expect("original vCPU vtimer compare value should be read");
        vcpu.set_vtimer_mask(true)
            .expect("vCPU vtimer mask should be set");
        vcpu.set_system_register(HvfSystemRegister::CNTV_CTL_EL0, 0)
            .expect("vCPU vtimer should be disabled");
        vcpu.set_vtimer_offset(VTIMER_TEST_OFFSET)
            .expect("vCPU vtimer offset should be set");
        vcpu.set_system_register(HvfSystemRegister::CNTV_CVAL_EL0, VTIMER_TEST_COMPARE_VALUE)
            .expect("vCPU vtimer compare value should be set");
        assert!(
            vcpu.get_vtimer_mask()
                .expect("vCPU vtimer mask should be read")
        );
        assert_eq!(
            vcpu.get_vtimer_offset()
                .expect("vCPU vtimer offset should be read"),
            VTIMER_TEST_OFFSET
        );
        assert_eq!(
            vcpu.get_system_register(HvfSystemRegister::CNTV_CTL_EL0)
                .expect("vCPU vtimer control should be read")
                & VTIMER_WRITABLE_CONTROL_MASK,
            0
        );
        assert_eq!(
            vcpu.get_system_register(HvfSystemRegister::CNTV_CVAL_EL0)
                .expect("vCPU vtimer compare value should be read"),
            VTIMER_TEST_COMPARE_VALUE
        );
        vcpu.set_vtimer_offset(original_vtimer_offset)
            .expect("original vCPU vtimer offset should be restored");
        vcpu.set_system_register(
            HvfSystemRegister::CNTV_CVAL_EL0,
            original_vtimer_compare_value,
        )
        .expect("original vCPU vtimer compare value should be restored");
        vcpu.set_system_register(
            HvfSystemRegister::CNTV_CTL_EL0,
            original_vtimer_control & VTIMER_WRITABLE_CONTROL_MASK,
        )
        .expect("original vCPU vtimer control should be restored");
        vcpu.set_vtimer_mask(original_vtimer_mask)
            .expect("original vCPU vtimer mask should be restored");
        vcpu.destroy().expect("vCPU should be destroyed");
        vcpu.destroy()
            .expect("destroyed vCPU should remain destroyed");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn configures_hvf_vcpu_arm64_boot_registers() {
    use bangbang_hvf::{
        ARM64_LINUX_BOOT_CPSR, HvfArm64BootRegisters, HvfBackend, HvfRegister, HvfSystemRegister,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::GuestAddress;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let registers = HvfArm64BootRegisters {
        kernel_entry: GuestAddress::new(0x8028_0000),
        fdt_address: GuestAddress::new(0x8fe0_0000),
    };

    backend.create_vm().expect("VM should be created");
    {
        let mut vcpu = backend.create_vcpu().expect("vCPU should be created");
        vcpu.configure_arm64_boot_registers(registers)
            .expect("boot registers should be configured");

        assert_eq!(
            vcpu.get_register(HvfRegister::PC)
                .expect("PC should be read"),
            registers.kernel_entry.raw_value()
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X0)
                .expect("X0 should be read"),
            registers.fdt_address.raw_value()
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X1)
                .expect("X1 should be read"),
            0
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X2)
                .expect("X2 should be read"),
            0
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X3)
                .expect("X3 should be read"),
            0
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::CPSR)
                .expect("CPSR should be read"),
            ARM64_LINUX_BOOT_CPSR
        );
        let _mpidr = vcpu
            .get_system_register(HvfSystemRegister::MPIDR_EL1)
            .expect("MPIDR_EL1 should be read");

        vcpu.destroy().expect("vCPU should be destroyed");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_configured_arm64_general_registers_on_runner_thread() {
    use bangbang_hvf::{ARM64_LINUX_BOOT_CPSR, HvfArm64BootRegisters, HvfBackend};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::GuestAddress;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let registers = HvfArm64BootRegisters {
        kernel_entry: GuestAddress::new(0x8028_0000),
        fdt_address: GuestAddress::new(0x8fe0_0000),
    };

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(registers)
            .expect("boot registers should be configured");

        let state = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be captured");
        assert_eq!(state.general_purpose_registers().len(), 31);
        assert_eq!(
            state.general_purpose_register(0),
            Some(registers.fdt_address.raw_value())
        );
        assert_eq!(state.general_purpose_register(1), Some(0));
        assert_eq!(state.general_purpose_register(2), Some(0));
        assert_eq!(state.general_purpose_register(3), Some(0));
        assert_eq!(state.pc(), registers.kernel_entry.raw_value());
        assert_eq!(state.cpsr(), ARM64_LINUX_BOOT_CPSR);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn measures_real_hvf_vcpu_execution_time_on_owner_thread() {
    use bangbang_hvf::{
        HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit,
        is_hvf_arm64_pvtime_measurement_available,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    const NOP: u32 = 0xd503_201f;
    const HVC_ZERO: u32 = 0xd400_0002;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    assert!(
        is_hvf_arm64_pvtime_measurement_available(),
        "the signed host must export the public macOS 11 execution-time primitive"
    );
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = std::iter::repeat_n(NOP, 128)
        .chain([HVC_ZERO])
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("execution-time guest should fit");

    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    backend
        .create_gic()
        .expect("GIC should be created before the execution-time vCPU");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest boot registers should configure");

        let before = runner
            .pvtime_execution_time_ns()
            .expect("initial owner-thread measurement should succeed");
        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("measurement guest should exit through HVC")
        else {
            panic!("measurement guest should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("measurement guest exit should decode")
                .immediate(),
            0
        );
        let after = runner
            .pvtime_execution_time_ns()
            .expect("post-run owner-thread measurement should succeed");
        let repeated = runner
            .pvtime_execution_time_ns()
            .expect("repeated owner-thread measurement should succeed");
        assert!(
            after > before,
            "guest execution must increase cumulative time"
        );
        assert!(
            repeated >= after,
            "cumulative execution time must not regress"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_arm64_general_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::GuestAddress;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let registers = HvfArm64BootRegisters {
        kernel_entry: GuestAddress::new(0x8028_0000),
        fdt_address: GuestAddress::new(0x8fe0_0000),
    };

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(registers)
            .expect("boot registers should be configured");

        let before = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be captured before restore");
        runner
            .restore_arm64_general_register_state(&before)
            .expect("general-register state should be restored");
        let after = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be recaptured after restore");
        assert!(
            after == before,
            "general-register state should round trip without exposing values"
        );

        runner
            .restore_arm64_general_register_state(&before)
            .expect("repeated general-register restore should succeed");
        let repeated = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be recaptured after repeated restore");
        assert!(
            repeated == before,
            "repeated general-register restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_core_system_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = CORE_SYSTEM_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("core system-register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest register writer should exit through HVC")
        else {
            panic!("guest register writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest register writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_core_system_register_state()
            .expect("core system-register state should be captured");
        assert_eq!(state.sp_el0(), CORE_SYSTEM_TEST_SP_EL0);
        assert_eq!(state.sp_el1(), CORE_SYSTEM_TEST_SP_EL1);
        assert_eq!(state.elr_el1(), CORE_SYSTEM_TEST_ELR_EL1);
        assert_eq!(state.spsr_el1(), CORE_SYSTEM_TEST_SPSR_EL1);

        runner
            .restore_arm64_core_system_register_state(&state)
            .expect("core system-register state should be restored");
        let restored = runner
            .capture_arm64_core_system_register_state()
            .expect("core system-register state should be recaptured after restore");
        assert!(
            restored == state,
            "core system-register state should round trip without exposing values"
        );

        runner
            .restore_arm64_core_system_register_state(&state)
            .expect("repeated core system-register restore should succeed");
        let repeated = runner
            .capture_arm64_core_system_register_state()
            .expect("core system-register state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated core system-register restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_exception_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = EXCEPTION_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("exception-register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest exception-register writer should exit through HVC")
        else {
            panic!("guest exception-register writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest exception-register writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_exception_register_state()
            .expect("exception-register state should be captured");
        // Auxiliary fault-status contents are implementation-defined. Current
        // Apple Silicon exposes AFSR0 as read-as-zero/write-ignored and
        // preserves AFSR1, while another host may expose either behavior for
        // either register.
        assert!(matches!(state.afsr0_el1(), 0 | EXCEPTION_TEST_AFSR0_EL1));
        assert!(matches!(state.afsr1_el1(), 0 | EXCEPTION_TEST_AFSR1_EL1));
        assert_eq!(state.esr_el1(), EXCEPTION_TEST_ESR_EL1);
        assert_eq!(state.far_el1(), EXCEPTION_TEST_FAR_EL1);
        assert_eq!(state.par_el1(), EXCEPTION_TEST_PAR_EL1);
        assert_eq!(state.vbar_el1(), EXCEPTION_TEST_VBAR_EL1);

        runner
            .restore_arm64_exception_register_state(&state)
            .expect("exception-register state should be restored");
        let restored = runner
            .capture_arm64_exception_register_state()
            .expect("exception-register state should be recaptured after restore");
        assert!(
            restored == state,
            "exception-register state should round trip without exposing values"
        );

        runner
            .restore_arm64_exception_register_state(&state)
            .expect("repeated exception-register restore should succeed");
        let repeated = runner
            .capture_arm64_exception_register_state()
            .expect("exception-register state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated exception-register restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_execution_controls_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = EXECUTION_CONTROL_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("execution-control guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest execution-control writer should exit through HVC")
        else {
            panic!("guest execution-control writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest execution-control writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_execution_control_register_state()
            .expect("execution-control state should be captured");
        assert_eq!(state.actlr_el1(), EXECUTION_CONTROL_TEST_ACTLR_EL1);
        assert_eq!(state.cpacr_el1(), EXECUTION_CONTROL_TEST_CPACR_EL1);

        runner
            .restore_arm64_execution_control_register_state(&state)
            .expect("execution-control state should be restored");
        let restored = runner
            .capture_arm64_execution_control_register_state()
            .expect("execution-control state should be recaptured after restore");
        assert!(
            restored == state,
            "execution-control state should round trip without exposing values"
        );

        runner
            .restore_arm64_execution_control_register_state(&state)
            .expect("repeated execution-control restore should succeed");
        let repeated = runner
            .capture_arm64_execution_control_register_state()
            .expect("execution-control state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated execution-control restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_arm64_cache_selection_register_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_cache_selection_register_state()
            .expect("first cache-selection state should be captured");
        let second = runner
            .capture_arm64_cache_selection_register_state()
            .expect("second cache-selection state should be captured");

        // Exercise the raw accessor without assuming an architecturally
        // unknown reset value or interpreting it as cache topology.
        let _captured_values = [first.csselr_el1(), second.csselr_el1()];

        runner
            .restore_arm64_cache_selection_register_state(&first)
            .expect("cache-selection state should be restored");
        let restored = runner
            .capture_arm64_cache_selection_register_state()
            .expect("restored cache-selection state should be captured");
        assert!(
            restored == first,
            "restored cache-selection state should match its idle source"
        );
        runner
            .restore_arm64_cache_selection_register_state(&first)
            .expect("cache-selection state should be restored a second time");
        let restored_again = runner
            .capture_arm64_cache_selection_register_state()
            .expect("twice-restored cache-selection state should be captured");
        assert!(
            restored_again == first,
            "twice-restored cache-selection state should match its idle source"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_all_implemented_arm64_breakpoint_registers_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_breakpoint_register_state()
            .expect("first breakpoint-register state should be captured");
        let second = runner
            .capture_arm64_breakpoint_register_state()
            .expect("second breakpoint-register state should be captured");

        for state in [&first, &second] {
            let count = state.implemented_breakpoint_count();
            assert!((1..=16).contains(&count));
            assert_eq!(state.breakpoint_value_registers().len(), usize::from(count));
            assert_eq!(
                state.breakpoint_control_registers().len(),
                usize::from(count)
            );
            for index in 0..count {
                assert!(state.breakpoint_value_register(index).is_some());
                assert!(state.breakpoint_control_register(index).is_some());
            }
            if count < 16 {
                assert_eq!(state.breakpoint_value_register(count), None);
                assert_eq!(state.breakpoint_control_register(count), None);
            }
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_all_implemented_arm64_watchpoint_registers_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_watchpoint_register_state()
            .expect("first watchpoint-register state should be captured");
        let second = runner
            .capture_arm64_watchpoint_register_state()
            .expect("second watchpoint-register state should be captured");

        for state in [&first, &second] {
            let count = state.implemented_watchpoint_count();
            assert!((1..=16).contains(&count));
            assert_eq!(state.watchpoint_value_registers().len(), usize::from(count));
            assert_eq!(
                state.watchpoint_control_registers().len(),
                usize::from(count)
            );
            for index in 0..count {
                assert!(state.watchpoint_value_register(index).is_some());
                assert!(state.watchpoint_control_register(index).is_some());
            }
            if count < 16 {
                assert_eq!(state.watchpoint_value_register(count), None);
                assert_eq!(state.watchpoint_control_register(count), None);
            }
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_arm64_debug_control_registers_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let original = runner
            .capture_arm64_debug_control_register_state()
            .expect("original debug-control state should be captured");
        runner
            .restore_arm64_debug_control_register_state(&original)
            .expect("first debug-control state restore should succeed");
        let first_recapture = runner
            .capture_arm64_debug_control_register_state()
            .expect("debug-control state should be recaptured after first restore");
        assert_eq!(first_recapture, original);
        runner
            .restore_arm64_debug_control_register_state(&original)
            .expect("second debug-control state restore should succeed");
        let second_recapture = runner
            .capture_arm64_debug_control_register_state()
            .expect("debug-control state should be recaptured after second restore");
        assert_eq!(second_recapture, original);

        // Exercise both accessors without assuming or logging reset values.
        // Reapplying only the captured original does not manufacture active
        // debug controls, touch adjacent debug state, or execute the guest.
        let _captured_values = [
            first_recapture.mdccint_el1(),
            first_recapture.mdscr_el1(),
            second_recapture.mdccint_el1(),
            second_recapture.mdscr_el1(),
        ];

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_arm64_debug_trap_state_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let original = runner
            .capture_arm64_debug_trap_state()
            .expect("original debug-trap state should be captured");
        runner
            .restore_arm64_debug_trap_state(&original)
            .expect("first debug-trap state restore should succeed");
        let first_recapture = runner
            .capture_arm64_debug_trap_state()
            .expect("debug-trap state should be recaptured after first restore");
        assert_eq!(first_recapture, original);
        runner
            .restore_arm64_debug_trap_state(&original)
            .expect("second debug-trap state restore should succeed");
        let second_recapture = runner
            .capture_arm64_debug_trap_state()
            .expect("debug-trap state should be recaptured after second restore");
        assert_eq!(second_recapture, original);

        // Exercise both accessors without assuming or logging default values.
        // Reapplying only the captured original keeps this test free of guest
        // debug activation, guest instructions, and destination-policy claims.
        let _captured_values = [
            first_recapture.trap_debug_exceptions(),
            first_recapture.trap_debug_reg_accesses(),
            second_recapture.trap_debug_exceptions(),
            second_recapture.trap_debug_reg_accesses(),
        ];

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_identification_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuIdentificationRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_identification_register_state()
            .expect("first identification-register state should be captured");
        let second = runner
            .capture_arm64_identification_register_state()
            .expect("second identification-register state should be captured");

        let values = |state: HvfArm64VcpuIdentificationRegisterState| {
            [
                state.midr_el1(),
                state.mpidr_el1(),
                state.id_aa64pfr0_el1(),
                state.id_aa64pfr1_el1(),
                state.id_aa64dfr0_el1(),
                state.id_aa64dfr1_el1(),
                state.id_aa64isar0_el1(),
                state.id_aa64isar1_el1(),
                state.id_aa64mmfr0_el1(),
                state.id_aa64mmfr1_el1(),
                state.id_aa64mmfr2_el1(),
            ]
        };
        assert!(
            values(first) == values(second),
            "identification-register accessors should remain stable within one vCPU lifetime"
        );
        assert!(
            first == second,
            "identification-register state should remain stable within one vCPU lifetime"
        );
        assert!(
            first.mpidr_el1()
                == runner
                    .mpidr_el1()
                    .expect("standalone MPIDR owner-thread read should succeed"),
            "captured MPIDR should match the standalone owner-thread getter"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sve_sme_identification_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSveSmeIdentificationRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_sve_sme_identification_register_state()
            .expect("first SVE/SME identification state should be captured");
        let second = runner
            .capture_arm64_sve_sme_identification_register_state()
            .expect("second SVE/SME identification state should be captured");

        let values = |state: HvfArm64VcpuSveSmeIdentificationRegisterState| {
            [state.id_aa64zfr0_el1(), state.id_aa64smfr0_el1()]
        };
        assert!(
            values(first) == values(second),
            "SVE/SME identification accessors should remain stable within one vCPU lifetime"
        );
        assert!(
            first == second,
            "SVE/SME identification state should remain stable within one vCPU lifetime"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_pstate_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first =
            assert_sme_pstate_capture_supported_or_unavailable(runner.capture_arm64_sme_pstate())
                .expect("first SME PSTATE capture should succeed or report unsupported");
        let second =
            assert_sme_pstate_capture_supported_or_unavailable(runner.capture_arm64_sme_pstate())
                .expect("second SME PSTATE capture should succeed or report unsupported");

        assert_eq!(
            first.is_some(),
            second.is_some(),
            "SME availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            // Exercise both accessors without assuming or logging the flags,
            // entering streaming mode, enabling ZA, or reading SME data.
            let first_values = (
                first.streaming_sve_mode_enabled(),
                first.za_storage_enabled(),
            );
            let second_values = (
                second.streaming_sve_mode_enabled(),
                second.za_storage_enabled(),
            );
            assert!(
                first_values == second_values,
                "SME PSTATE should remain stable on one idle vCPU"
            );
            assert!(
                first == second,
                "SME PSTATE value should remain stable on one idle vCPU"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_p_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmePRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_p_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_p_register_state(),
        )
        .expect("first SME P-register capture should succeed or report unavailable");
        let second = assert_sme_p_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_p_register_state(),
        )
        .expect("second SME P-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME P-register capture availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.maximum_svl_bytes() == second.maximum_svl_bytes(),
                "SME maximum streaming vector length should remain stable"
            );
            assert!(
                first.predicate_width_bytes() == second.predicate_width_bytes(),
                "SME predicate allocation width should remain stable"
            );
            assert!(
                first.p_register(15).is_some() && first.p_register(16).is_none(),
                "SME P-register capture should contain exactly P0 through P15"
            );
            for register in 0..HvfArm64VcpuSmePRegisterState::REGISTER_COUNT {
                let first_register = first
                    .p_register(register)
                    .expect("first capture should contain every P register");
                let second_register = second
                    .p_register(register)
                    .expect("second capture should contain every P register");
                assert!(
                    first_register.len() == first.predicate_width_bytes(),
                    "first capture should retain the exact predicate width"
                );
                assert!(
                    second_register.len() == second.predicate_width_bytes(),
                    "second capture should retain the exact predicate width"
                );
            }
            assert!(
                first == second,
                "SME P-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}").contains("<redacted>"),
                "SME P-register debug output should remain redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_z_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmeZRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_z_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_z_register_state(),
        )
        .expect("first SME Z-register capture should succeed or report unavailable");
        let second = assert_sme_z_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_z_register_state(),
        )
        .expect("second SME Z-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME Z-register capture availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.maximum_svl_bytes() == second.maximum_svl_bytes(),
                "SME maximum streaming vector length should remain stable"
            );
            assert!(
                first.z_register(31).is_some() && first.z_register(32).is_none(),
                "SME Z-register capture should contain exactly Z0 through Z31"
            );
            for register in 0..HvfArm64VcpuSmeZRegisterState::REGISTER_COUNT {
                let first_register = first
                    .z_register(register)
                    .expect("first capture should contain every Z register");
                let second_register = second
                    .z_register(register)
                    .expect("second capture should contain every Z register");
                assert!(
                    first_register.len() == first.maximum_svl_bytes(),
                    "first capture should retain the exact maximum width"
                );
                assert!(
                    second_register.len() == second.maximum_svl_bytes(),
                    "second capture should retain the exact maximum width"
                );
            }
            assert!(
                first == second,
                "SME Z-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}").contains("<redacted>"),
                "SME Z-register debug output should remain redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_za_register_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_za_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_za_register_state(),
        )
        .expect("first SME ZA-register capture should succeed or report unavailable");
        let second = assert_sme_za_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_za_register_state(),
        )
        .expect("second SME ZA-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME ZA-register capture availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.maximum_svl_bytes() == second.maximum_svl_bytes(),
                "SME maximum streaming vector length should remain stable"
            );
            let expected_size = first
                .maximum_svl_bytes()
                .checked_mul(first.maximum_svl_bytes())
                .expect("SME maximum streaming vector length should have a square byte size");
            assert!(
                first.len() == expected_size && first.as_bytes().len() == expected_size,
                "first SME ZA capture should retain the exact maximum-SVL square"
            );
            assert!(
                second.len() == expected_size && second.as_bytes().len() == expected_size,
                "second SME ZA capture should retain the exact maximum-SVL square"
            );
            assert!(
                !first.is_empty() && !second.is_empty(),
                "successful SME ZA captures should contain the complete matrix"
            );
            assert!(
                first == second,
                "SME ZA-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}").contains("<redacted>"),
                "SME ZA-register debug output should remain redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_zt0_register_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmeZt0RegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_zt0_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_zt0_register_state(),
        )
        .expect("first SME ZT0-register capture should succeed or report unavailable");
        let second = assert_sme_zt0_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_zt0_register_state(),
        )
        .expect("second SME ZT0-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME ZT0-register availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.as_bytes().len() == HvfArm64VcpuSmeZt0RegisterState::BYTE_COUNT
                    && second.as_bytes().len() == HvfArm64VcpuSmeZt0RegisterState::BYTE_COUNT,
                "SME ZT0 captures should preserve exactly 64 bytes"
            );
            assert!(
                first == second,
                "SME ZT0-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}")
                    == "HvfArm64VcpuSmeZt0RegisterState { register: \"<redacted>\" }",
                "SME ZT0-register debug output should remain fully redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_system_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmeSystemRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_sme_system_register_state()
            .expect("first SME system-register state should be captured");
        let second = runner
            .capture_arm64_sme_system_register_state()
            .expect("second SME system-register state should be captured");

        let values = |state: HvfArm64VcpuSmeSystemRegisterState| {
            [state.smcr_el1(), state.smpri_el1(), state.tpidr2_el0()]
        };
        assert!(
            values(first) == values(second),
            "SME system-register accessors should remain stable within one idle vCPU lifetime"
        );
        assert!(
            first == second,
            "SME system-register state should remain stable within one idle vCPU lifetime"
        );
        assert!(
            format!("{first:?}").contains("<redacted>"),
            "SME system-register debug output should remain redacted"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_arm64_system_context_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSystemContextRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_system_context_register_state()
            .expect("first system-context register state should be captured");
        let second = runner
            .capture_arm64_system_context_register_state()
            .expect("second system-context register state should be captured");

        let values = |state: HvfArm64VcpuSystemContextRegisterState| {
            [state.scxtnum_el0(), state.scxtnum_el1()]
        };
        assert!(
            values(first) == values(second),
            "system-context register accessors should remain stable within one idle vCPU lifetime"
        );
        assert!(
            first == second,
            "system-context register state should remain stable within one idle vCPU lifetime"
        );
        assert!(
            format!("{first:?}").contains("<redacted>"),
            "system-context register debug output should remain redacted"
        );

        runner
            .restore_arm64_system_context_register_state(&first)
            .expect("system-context register state should be restored");
        let restored = runner
            .capture_arm64_system_context_register_state()
            .expect("restored system-context register state should be captured");
        assert!(
            restored == first,
            "restored system-context register state should match its idle source"
        );
        runner
            .restore_arm64_system_context_register_state(&first)
            .expect("system-context register state should be restored a second time");
        let restored_again = runner
            .capture_arm64_system_context_register_state()
            .expect("twice-restored system-context register state should be captured");
        assert!(
            restored_again == first,
            "twice-restored system-context register state should match its idle source"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_translation_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = TRANSLATION_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("translation-register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest translation-register writer should exit through HVC")
        else {
            panic!("guest translation-register writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest translation-register writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_translation_register_state()
            .expect("translation-register state should be captured");
        assert_eq!(state.sctlr_el1() & 1, 0);
        assert_eq!(state.ttbr0_el1(), TRANSLATION_TEST_TTBR0_EL1);
        assert_eq!(state.ttbr1_el1(), TRANSLATION_TEST_TTBR1_EL1);
        assert_eq!(state.tcr_el1(), TRANSLATION_TEST_TCR_EL1);
        assert_eq!(state.mair_el1(), TRANSLATION_TEST_MAIR_EL1);
        // AMAIR is implementation-defined. Current Apple Silicon exposes it
        // as read-as-zero/write-ignored, while a future host may preserve the
        // architecturally valid guest write.
        assert!(matches!(
            state.amair_el1(),
            0 | TRANSLATION_TEST_AMAIR_EL1_WRITE
        ));
        assert_eq!(state.contextidr_el1(), TRANSLATION_TEST_CONTEXTIDR_EL1);

        runner
            .restore_arm64_translation_register_state(&state)
            .expect("translation-register state should be restored");
        let restored = runner
            .capture_arm64_translation_register_state()
            .expect("translation-register state should be recaptured after restore");
        assert!(
            restored == state,
            "translation-register state should round trip without exposing values"
        );

        runner
            .restore_arm64_translation_register_state(&state)
            .expect("repeated translation-register restore should succeed");
        let repeated = runner
            .capture_arm64_translation_register_state()
            .expect("translation-register state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated translation-register restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_pointer_authentication_keys_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = POINTER_AUTHENTICATION_KEY_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("pointer-authentication key guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest pointer-authentication key writer should exit through HVC")
        else {
            panic!("guest pointer-authentication key writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest pointer-authentication key writer should exit through HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_pointer_authentication_key_state()
            .expect("pointer-authentication key state should be captured");
        assert!(
            format!("{state:?}")
                == "HvfArm64VcpuPointerAuthenticationKeyState { keys: \"<redacted>\" }",
            "pointer-authentication key Debug output should be fully redacted"
        );
        assert!(
            state.apia_key() == POINTER_AUTHENTICATION_TEST_APIA_KEY,
            "APIA should match the non-secret test key"
        );
        assert!(
            state.apib_key() == POINTER_AUTHENTICATION_TEST_APIB_KEY,
            "APIB should match the non-secret test key"
        );
        assert!(
            state.apda_key() == POINTER_AUTHENTICATION_TEST_APDA_KEY,
            "APDA should match the non-secret test key"
        );
        assert!(
            state.apdb_key() == POINTER_AUTHENTICATION_TEST_APDB_KEY,
            "APDB should match the non-secret test key"
        );
        assert!(
            state.apga_key() == POINTER_AUTHENTICATION_TEST_APGA_KEY,
            "APGA should match the non-secret test key"
        );

        runner
            .restore_arm64_pointer_authentication_key_state(&state)
            .expect("pointer-authentication key state should be restored");
        let restored = runner
            .capture_arm64_pointer_authentication_key_state()
            .expect("pointer-authentication key state should be recaptured after restore");
        assert!(
            restored == state,
            "pointer-authentication key state should round trip without exposing values"
        );

        runner
            .restore_arm64_pointer_authentication_key_state(&state)
            .expect("repeated pointer-authentication key restore should succeed");
        let repeated = runner
            .capture_arm64_pointer_authentication_key_state()
            .expect("pointer-authentication key state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated pointer-authentication key restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_thread_context_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = THREAD_CONTEXT_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("thread-context register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest thread-context writer should exit through HVC")
        else {
            panic!("guest thread-context writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest thread-context writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_thread_context_register_state()
            .expect("thread-context register state should be captured");
        assert_eq!(state.tpidr_el0(), THREAD_CONTEXT_TEST_TPIDR_EL0);
        assert_eq!(state.tpidrro_el0(), THREAD_CONTEXT_TEST_TPIDRRO_EL0);
        assert_eq!(state.tpidr_el1(), THREAD_CONTEXT_TEST_TPIDR_EL1);

        runner
            .restore_arm64_thread_context_register_state(&state)
            .expect("thread-context register state should be restored");
        let restored = runner
            .capture_arm64_thread_context_register_state()
            .expect("thread-context register state should be recaptured after restore");
        assert!(
            restored == state,
            "thread-context register state should round trip without exposing values"
        );

        runner
            .restore_arm64_thread_context_register_state(&state)
            .expect("repeated thread-context register restore should succeed");
        let repeated = runner
            .capture_arm64_thread_context_register_state()
            .expect("thread-context register state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated thread-context register restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_simd_fp_state_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = SIMD_FP_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("SIMD/FP guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest SIMD/FP writer should exit through HVC")
        else {
            panic!("guest SIMD/FP writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest SIMD/FP writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_simd_fp_state()
            .expect("SIMD/FP state should be captured");
        assert_eq!(state.q_register(0), Some(SIMD_FP_TEST_Q0));
        assert_eq!(state.q_register(31), Some(SIMD_FP_TEST_Q31));
        assert_eq!(state.fpcr(), SIMD_FP_TEST_FPCR);
        assert_eq!(state.fpsr(), SIMD_FP_TEST_FPSR);

        runner
            .restore_arm64_simd_fp_state(&state)
            .expect("SIMD/FP state should be restored");
        let restored = runner
            .capture_arm64_simd_fp_state()
            .expect("SIMD/FP state should be recaptured after restore");
        assert!(
            restored == state,
            "SIMD/FP state should round trip without exposing values"
        );

        runner
            .restore_arm64_simd_fp_state(&state)
            .expect("repeated SIMD/FP restore should succeed");
        let repeated = runner
            .capture_arm64_simd_fp_state()
            .expect("SIMD/FP state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated SIMD/FP restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn creates_hvf_gic_before_vcpu() {
    use bangbang_hvf::{HvfBackend, HvfGicMetadata};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    let metadata = *backend.create_gic().expect("GIC should be created");
    assert_eq!(metadata.msi, None);
    assert_eq!(HvfGicMetadata::FDT_COMPATIBILITY, "arm,gic-v3");
    assert!(metadata.distributor.size > 0);
    assert!(metadata.redistributor.region.size > 0);
    {
        let mut vcpu = backend
            .create_vcpu()
            .expect("vCPU should be created after GIC");
        vcpu.destroy().expect("vCPU should be destroyed");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn delivers_hvf_gic_msi_to_the_allocated_guest_intid() {
    use std::num::NonZeroU32;
    use std::sync::mpsc;
    use std::time::Duration;

    use bangbang_hvf::{
        HvfArm64BootRegisters, HvfBackend, HvfGicMsiConfiguration, HvfMemoryPermissions,
        HvfVcpuExit,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::fdt::ARM64_GICV2M_SPI_END_EXCLUSIVE;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    let metadata = *backend
        .create_gic_with_msi(HvfGicMsiConfiguration::new(
            NonZeroU32::new(1).expect("test MSI count should be nonzero"),
        ))
        .expect("MSI-enabled GIC should be created");
    let signaler = backend
        .gic_msi_signaler()
        .expect("MSI-enabled GIC should retain its sender")
        .clone();
    let interrupt = signaler
        .allocator()
        .allocate()
        .expect("one MSI should allocate");
    assert_eq!(
        interrupt.raw_value(),
        ARM64_GICV2M_SPI_END_EXCLUSIVE - 1,
        "the host terminal SPI should remain outside the GICv2m allocation"
    );
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let vector_base = guest_entry
        .checked_add(0x800)
        .expect("guest vector address should fit");
    let irq_handler = vector_base
        .checked_add(0x280)
        .expect("current-EL SPx IRQ vector should fit");
    let config_address = guest_entry
        .checked_add(0x1000)
        .expect("guest MSI config address should fit");
    let guest_code = GIC_MSI_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    let irq_code = GIC_MSI_IRQ_HANDLER
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("MSI guest setup code should be written");
    memory
        .write_slice(&irq_code, irq_handler)
        .expect("MSI guest IRQ handler should be written");

    let mut guest_config = Vec::with_capacity(32);
    guest_config.extend_from_slice(&metadata.distributor.base.to_le_bytes());
    guest_config.extend_from_slice(&metadata.redistributor.region.base.to_le_bytes());
    guest_config.extend_from_slice(&interrupt.raw_value().to_le_bytes());
    guest_config.extend_from_slice(&0_u32.to_le_bytes());
    guest_config.extend_from_slice(&vector_base.raw_value().to_le_bytes());
    memory
        .write_slice(&guest_config, config_address)
        .expect("MSI guest configuration should be written");

    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: config_address,
            })
            .expect("MSI guest boot registers should be configured");

        let HvfVcpuExit::Exception(ready) = runner
            .run_once()
            .expect("MSI guest should publish readiness through HVC")
        else {
            panic!("MSI guest readiness should produce an exception exit");
        };
        assert_eq!(
            ready
                .decode_hvc()
                .expect("MSI guest readiness should decode as HVC")
                .immediate(),
            0
        );
        let ready_registers = runner
            .capture_arm64_general_register_state()
            .expect("MSI readiness registers should be captured");
        assert_eq!(
            ready_registers
                .general_purpose_register(1)
                .expect("X1 should contain GICD_TYPER")
                & (1 << 17),
            0,
            "the validated GICv2m path requires a distributor without LPIs",
        );

        signaler
            .send(&interrupt)
            .expect("real Hypervisor.framework MSI should be sent");
        let cancel = runner.run_cancel_handle();
        let delivered = std::thread::scope(|scope| {
            let (sender, receiver) = mpsc::sync_channel(1);
            let runner_ref = &runner;
            scope.spawn(move || {
                let _ = sender.send(runner_ref.run_once());
            });

            match receiver.recv_timeout(Duration::from_secs(5)) {
                Ok(result) => result.expect("MSI guest run should succeed"),
                Err(error) => {
                    cancel
                        .cancel()
                        .expect("timed-out MSI guest run should cancel");
                    let _ = receiver.recv_timeout(Duration::from_secs(5));
                    panic!("MSI guest did not observe an IRQ before the deadline: {error}");
                }
            }
        });
        let HvfVcpuExit::Exception(delivered) = delivered else {
            panic!("delivered MSI should produce an exception exit");
        };
        assert_eq!(
            delivered
                .decode_hvc()
                .expect("MSI IRQ handler exit should decode as HVC")
                .immediate(),
            1
        );
        let registers = runner
            .capture_arm64_general_register_state()
            .expect("MSI IRQ result registers should be captured");
        assert_eq!(
            registers.general_purpose_register(0),
            Some(u64::from(interrupt.raw_value()))
        );

        runner
            .shutdown()
            .expect("MSI guest runner should shut down");
    }
    drop(signaler);
    backend
        .destroy_vm()
        .expect("MSI guest VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_hvf_gic_device_and_icc_state_before_run() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend.create_gic().expect("GIC should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");

        let state = runner
            .capture_gic_device_state()
            .expect("opaque GIC device state should be captured");
        assert!(!state.is_empty());
        assert_eq!(state.as_bytes().len(), state.len());
        let icc_state = runner
            .capture_arm64_gic_icc_register_state()
            .expect("GIC ICC register state should be captured before run");
        runner
            .restore_gic_device_state(&state)
            .expect("opaque GIC device state should be restored before run");
        for _ in 0..2 {
            runner
                .restore_arm64_gic_icc_register_state(&icc_state)
                .expect("GIC ICC register state should be restored before run");
            let restored_icc_state = runner
                .capture_arm64_gic_icc_register_state()
                .expect("restored GIC ICC register state should be recaptured");
            assert!(
                restored_icc_state == icc_state,
                "restored GIC ICC register state should match the original"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_gic_icc_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = GIC_ICC_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("GIC ICC register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    backend.create_gic().expect("GIC should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest GIC ICC writer should exit through HVC")
        else {
            panic!("guest GIC ICC writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest GIC ICC writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_gic_icc_register_state()
            .expect("GIC ICC register state should be captured");
        assert_eq!(state.pmr_el1(), GIC_ICC_TEST_PMR_EL1);
        assert_eq!(state.bpr0_el1(), GIC_ICC_TEST_BPR0_EL1);
        assert_eq!(state.bpr1_el1(), GIC_ICC_TEST_BPR1_EL1);
        assert_eq!(state.sre_el1() & 1, 1);
        assert_eq!(state.igrpen0_el1(), 1);
        assert_eq!(state.igrpen1_el1(), 1);
        let _host_defined_values = (
            state.ap0r0_el1(),
            state.ap1r0_el1(),
            state.rpr_el1(),
            state.ctlr_el1(),
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn rejects_hvf_gic_after_vcpu_creation() {
    use bangbang_hvf::{HvfBackend, HvfGicError};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let mut vcpu = backend.create_vcpu().expect("vCPU should be created");
        vcpu.destroy().expect("vCPU should be destroyed");
    }
    assert_eq!(
        backend
            .create_gic()
            .expect_err("GIC creation after vCPU creation should fail"),
        HvfGicError::InvalidState("GIC must be created before creating vCPUs")
    );
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn cancels_runner_before_first_run() {
    use bangbang_hvf::{HvfBackend, HvfVcpuExit};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner.cancel().expect("runner should accept cancellation");
        assert_eq!(
            runner.run_once().expect("runner should return an exit"),
            HvfVcpuExit::Canceled
        );
        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn owns_and_cleans_ordered_two_vcpu_topology() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    backend
        .create_gic()
        .expect("GIC should be created before the vCPU topology");
    {
        let topology = backend
            .start_vcpu_topology(2)
            .expect("host should support a two-vCPU topology");
        assert_eq!(topology.mpidrs(), [0, 1]);
        assert_eq!(topology.len(), 2);

        topology
            .cancel()
            .expect("every topology member should accept cancellation");

        topology
            .shutdown()
            .expect("every topology member should shut down");
        topology
            .shutdown()
            .expect("topology shutdown should be idempotent");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn concurrently_runs_and_batch_cancels_two_vcpus() {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use bangbang_hvf::{
        HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuRunControlReason,
        HvfVcpuRunEvent, HvfVcpuRunMemberOutcome, HvfVcpuRunStepOutcome,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};
    use bangbang_runtime::mmio::MmioDispatcher;

    const SECOND_ENTRY_OFFSET: u64 = 0x100;
    const FLAGS_OFFSET: u64 = 0x2000;
    const PEER_FLAG_OFFSET: u32 = 8;
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
    const MOV_W1_ONE: u32 = 0x5280_0021;
    const STR_W1_X0: u32 = 0xb900_0001;
    const DMB_ISH: u32 = 0xd503_3bbf;
    const ADD_X2_X0_PEER: u32 = 0x9100_2002;
    const SUB_X2_X0_PEER: u32 = 0xd100_2002;
    const LDR_W3_X2: u32 = 0xb940_0043;
    const CBZ_W3_PREVIOUS: u32 = 0x34ff_ffe3;
    const SPIN_FOREVER: u32 = 0x1400_0000;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");

    for iteration in 0..2 {
        let mut backend = HvfBackend::new();
        let layout =
            aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
                .expect("guest memory layout should be valid");
        let mut memory =
            GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
        let first_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
        let second_entry = first_entry
            .checked_add(SECOND_ENTRY_OFFSET)
            .expect("second guest entry should fit");
        let first_flag = first_entry
            .checked_add(FLAGS_OFFSET)
            .expect("first handshake flag should fit");
        let second_flag = first_flag
            .checked_add(u64::from(PEER_FLAG_OFFSET))
            .expect("second handshake flag should fit");
        let first_code = [
            MOV_W1_ONE,
            STR_W1_X0,
            DMB_ISH,
            ADD_X2_X0_PEER,
            LDR_W3_X2,
            CBZ_W3_PREVIOUS,
            SPIN_FOREVER,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
        let second_code = [
            MOV_W1_ONE,
            STR_W1_X0,
            DMB_ISH,
            SUB_X2_X0_PEER,
            LDR_W3_X2,
            CBZ_W3_PREVIOUS,
            SPIN_FOREVER,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
        memory
            .write_slice(&first_code, first_entry)
            .expect("first guest handshake should be written");
        memory
            .write_slice(&second_code, second_entry)
            .expect("second guest handshake should be written");
        memory
            .write_slice(&[0; 16], first_flag)
            .expect("guest handshake flags should be zeroed");
        let dram_region = memory
            .regions()
            .first()
            .expect("guest DRAM should contain one region");
        assert_eq!(dram_region.range().start(), first_entry);
        let first_flag_host = dram_region
            .host_address()
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(FLAGS_OFFSET as usize)
            .cast::<u32>();
        let second_flag_host = first_flag_host.wrapping_add(2);

        backend.create_vm().expect("VM should be created");
        backend
            .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
            .expect("guest handshake memory should be mapped");
        backend
            .create_gic()
            .expect("GIC should be created before the vCPU topology");
        {
            let topology = backend
                .start_vcpu_topology(2)
                .expect("host should support a two-vCPU topology");
            let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
            let mut coordinator = topology
                .into_run_coordinator(dispatcher, &[0, 1])
                .expect("two-vCPU coordinator should start");
            coordinator
                .configure_arm64_boot_registers(
                    0,
                    HvfArm64BootRegisters {
                        kernel_entry: first_entry,
                        fdt_address: first_flag,
                    },
                )
                .expect("first guest entry should be configured");
            coordinator
                .configure_arm64_boot_registers(
                    1,
                    HvfArm64BootRegisters {
                        kernel_entry: second_entry,
                        fdt_address: second_flag,
                    },
                )
                .expect("second guest entry should be configured");
            assert_eq!(
                coordinator.dispatch_online(),
                Ok(2),
                "iteration {iteration} should submit both vCPUs before collection"
            );

            let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
            loop {
                // SAFETY: both aligned pointers remain inside the mapped DRAM
                // region owned by `backend`; volatile reads observe guest writes
                // while both vCPU owner threads are running.
                let flags = unsafe {
                    (
                        std::ptr::read_volatile(first_flag_host),
                        std::ptr::read_volatile(second_flag_host),
                    )
                };
                if flags == (1, 1) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "iteration {iteration} timed out waiting for both guest handshake flags; observed {flags:?}"
                );
                std::thread::yield_now();
            }

            let waiter = coordinator
                .control()
                .request_stop()
                .expect("one active-only batch stop should start");
            let event = coordinator
                .receive_event()
                .expect("both canceled generations should drain");
            let HvfVcpuRunEvent::Barrier(report) = event else {
                panic!("iteration {iteration} should complete a stop barrier");
            };
            assert_eq!(report.reason(), HvfVcpuRunControlReason::Stop);
            assert_eq!(
                report
                    .acknowledgements()
                    .iter()
                    .map(|result| result.index())
                    .collect::<Vec<_>>(),
                vec![0, 1]
            );
            assert!(report.acknowledgements().iter().all(|result| matches!(
                result.result(),
                Ok(HvfVcpuRunMemberOutcome::Handled(
                    HvfVcpuRunStepOutcome::Canceled
                ))
            )));
            assert_eq!(waiter.wait(), Ok(report));

            coordinator
                .shutdown()
                .expect("coordinator should shut down every owner");
            coordinator
                .shutdown()
                .expect("coordinator shutdown should be idempotent");
        }
        backend
            .destroy_vm()
            .expect("VM teardown should unmap guest memory after owner shutdown");
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn tracks_concurrent_guest_writes_with_exact_retry_and_bounded_cancellation() {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use bangbang_hvf::{
        HvfArm64BootRegisters, HvfBackend, HvfDirtyWriteTrackerStopError, HvfMemoryPermissions,
        HvfVcpuRunControlReason, HvfVcpuRunEvent, HvfVcpuRunMemberOutcome, HvfVcpuRunStepOutcome,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::fdt::{Arm64FdtRegion, Arm64FdtVmGenIdDevice};
    use bangbang_runtime::interrupt::GuestInterruptLine;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};
    use bangbang_runtime::mmio::MmioDispatcher;
    use bangbang_runtime::startup::{
        ARM64_BOOT_VMGENID_SIZE, Arm64BootVmGenIdDevice, replace_arm64_boot_vmgenid,
    };

    const SECOND_ENTRY_OFFSET: u64 = 0x100;
    const TARGET_START_PAGE: u64 = 2;
    const VCPU0_VALUE: u16 = 0x11;
    const VCPU1_VALUE: u16 = 0x22;
    const VCPU0_SECOND_VALUE: u16 = 0x33;
    const VCPU1_SECOND_VALUE: u16 = 0x44;
    const MOV_X3_X0: u32 = 0xaa00_03e3;
    const STR_W1_X3: u32 = 0xb900_0061;
    const STR_W1_X2: u32 = 0xb900_0041;
    const DMB_ISH: u32 = 0xd503_3bbf;
    const HVC_ZERO: u32 = 0xd400_0002;
    const SPIN_FOREVER: u32 = 0x1400_0000;
    const MAX_MEMBER_EVENTS: usize = 16;
    const DIRTY_PROGRESS_TIMEOUT: Duration = Duration::from_secs(5);

    fn mov_w1(value: u16) -> u32 {
        0x5280_0001 | (u32::from(value) << 5)
    }

    fn add_x2_x3_page_offset(host_page_size: u64, pages: u64) -> u32 {
        const ADD_X2_X3_SHIFT_12: u32 = 0x9140_0062;
        const ARM64_IMMEDIATE_PAGE_SIZE: u64 = 0x1000;

        assert!(host_page_size.is_multiple_of(ARM64_IMMEDIATE_PAGE_SIZE));
        let immediate = host_page_size
            .checked_div(ARM64_IMMEDIATE_PAGE_SIZE)
            .and_then(|units| units.checked_mul(pages))
            .and_then(|units| u32::try_from(units).ok())
            .expect("guest page offset should fit the ADD immediate");
        assert!(immediate <= 0xfff);
        ADD_X2_X3_SHIFT_12 | (immediate << 10)
    }

    fn guest_code(
        host_page_size: u64,
        first_value: u16,
        second_value: u16,
        first_page: u64,
    ) -> Vec<u8> {
        [
            MOV_X3_X0,
            mov_w1(first_value),
            STR_W1_X3,
            add_x2_x3_page_offset(host_page_size, first_page),
            STR_W1_X2,
            add_x2_x3_page_offset(host_page_size, first_page + 1),
            STR_W1_X2,
            DMB_ISH,
            HVC_ZERO,
            mov_w1(second_value),
            STR_W1_X3,
            add_x2_x3_page_offset(host_page_size, first_page),
            STR_W1_X2,
            add_x2_x3_page_offset(host_page_size, first_page + 1),
            STR_W1_X2,
            DMB_ISH,
            HVC_ZERO,
            SPIN_FOREVER,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
    }

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let page_size = host_page_size().expect("host page size should be valid");
    let layout = aarch64::dram_layout(page_size * 8)
        .expect("dirty-write guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("dirty-write guest memory allocation should succeed");
    let first_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let second_entry = first_entry
        .checked_add(SECOND_ENTRY_OFFSET)
        .expect("second guest entry should fit");
    let shared_page = first_entry
        .checked_add(page_size * TARGET_START_PAGE)
        .expect("shared dirty page should fit");
    let vcpu0_pages = [
        shared_page
            .checked_add(page_size)
            .expect("first vCPU0 page should fit"),
        shared_page
            .checked_add(page_size * 2)
            .expect("second vCPU0 page should fit"),
    ];
    let vcpu1_pages = [
        shared_page
            .checked_add(page_size * 3)
            .expect("first vCPU1 page should fit"),
        shared_page
            .checked_add(page_size * 4)
            .expect("second vCPU1 page should fit"),
    ];
    let device_page = shared_page
        .checked_add(page_size * 5)
        .expect("current-device dirty page should fit");
    memory
        .write_slice(
            &guest_code(page_size, VCPU0_VALUE, VCPU0_SECOND_VALUE, 1),
            first_entry,
        )
        .expect("first dirty-write guest code should be written");
    memory
        .write_slice(
            &guest_code(page_size, VCPU1_VALUE, VCPU1_SECOND_VALUE, 3),
            second_entry,
        )
        .expect("second dirty-write guest code should be written");
    for page in [
        shared_page,
        vcpu0_pages[0],
        vcpu0_pages[1],
        vcpu1_pages[0],
        vcpu1_pages[1],
    ] {
        memory
            .write_slice(&0_u32.to_le_bytes(), page)
            .expect("dirty-write target should be zeroed");
    }
    let userspace_tracker = memory
        .enable_dirty_tracking()
        .expect("shared dirty epoch should start before current-device activity");
    let vmgenid_range = bangbang_runtime::memory::GuestMemoryRange::new(
        device_page,
        ARM64_BOOT_VMGENID_SIZE as u64,
    )
    .expect("current-device VMGenID range should validate");
    let mut vmgenid = Arm64BootVmGenIdDevice {
        range: vmgenid_range,
        generation_id: [0; ARM64_BOOT_VMGENID_SIZE],
        fdt_device: Arm64FdtVmGenIdDevice {
            region: Arm64FdtRegion {
                base: device_page.raw_value(),
                size: ARM64_BOOT_VMGENID_SIZE as u64,
            },
            interrupt_line: GuestInterruptLine::new(1)
                .expect("current-device interrupt line should validate"),
        },
    };
    replace_arm64_boot_vmgenid(&mut memory, &mut vmgenid)
        .expect("current VMGenID device should write through tracked guest memory");
    assert_eq!(
        userspace_tracker
            .dirty_pages()
            .expect("current-device dirty page should query"),
        vec![device_page]
    );
    let dram_region = memory
        .regions()
        .first()
        .expect("dirty-write guest DRAM should contain one region");
    let target_host_pointer = |address: GuestAddress| {
        let offset = address
            .raw_value()
            .checked_sub(dram_region.range().start().raw_value())
            .and_then(|offset| usize::try_from(offset).ok())
            .expect("dirty-write target offset should fit this host");
        dram_region
            .host_address()
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(offset)
            .cast::<u32>()
    };
    let shared_host = target_host_pointer(shared_page);
    let vcpu0_hosts = vcpu0_pages.map(target_host_pointer);
    let vcpu1_hosts = vcpu1_pages.map(target_host_pointer);

    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("dirty-write guest memory should be mapped");
    let tracker = backend
        .start_dirty_write_tracking()
        .expect("dirty-write tracking should start before vCPU ownership");
    backend
        .create_gic()
        .expect("GIC should be created before the tracked vCPU topology");
    {
        let topology = backend
            .start_vcpu_topology(2)
            .expect("host should support a tracked two-vCPU topology");
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let mut coordinator = topology
            .into_run_coordinator(dispatcher, &[0, 1])
            .expect("tracked two-vCPU coordinator should start");
        for (index, entry) in [first_entry, second_entry].into_iter().enumerate() {
            coordinator
                .configure_arm64_boot_registers(
                    index,
                    HvfArm64BootRegisters {
                        kernel_entry: entry,
                        fdt_address: shared_page,
                    },
                )
                .expect("tracked guest entry should be configured");
        }
        assert_eq!(coordinator.dispatch_online(), Ok(2));
        let watchdog_control = coordinator.control();
        let (progress_sender, progress_receiver) = mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if progress_receiver
                .recv_timeout(DIRTY_PROGRESS_TIMEOUT)
                .is_err()
            {
                let _ = watchdog_control.request_stop();
            }
        });

        let expected_pages = [
            shared_page,
            vcpu0_pages[0],
            vcpu0_pages[1],
            vcpu1_pages[0],
            vcpu1_pages[1],
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let expected_vcpu0_pages = vcpu0_pages.into_iter().collect::<BTreeSet<_>>();
        let expected_vcpu1_pages = vcpu1_pages.into_iter().collect::<BTreeSet<_>>();
        for epoch_index in 0..2u64 {
            let mut first_write_pages = BTreeSet::new();
            let mut vcpu0_first_write_pages = BTreeSet::new();
            let mut vcpu1_first_write_pages = BTreeSet::new();
            let mut stale_shared_faults = 0usize;
            let mut reached_hvc = [false; 2];

            for _ in 0..MAX_MEMBER_EVENTS {
                let event = coordinator
                    .receive_event()
                    .expect("tracked member event should be received");
                let HvfVcpuRunEvent::Member(result) = event else {
                    panic!("tracked guest should not terminate before cancellation: {event:?}");
                };
                match result.result() {
                    Ok(HvfVcpuRunMemberOutcome::Handled(HvfVcpuRunStepOutcome::DirtyWrite {
                        page,
                        first_write,
                    })) => {
                        assert!(expected_pages.contains(page));
                        if *first_write {
                            assert!(first_write_pages.insert(*page));
                            match result.index() {
                                0 => {
                                    vcpu0_first_write_pages.insert(*page);
                                }
                                1 => {
                                    vcpu1_first_write_pages.insert(*page);
                                }
                                index => panic!("unexpected tracked member index {index}"),
                            }
                        } else {
                            assert_eq!(*page, shared_page);
                            stale_shared_faults += 1;
                            assert!(stale_shared_faults <= 1);
                        }
                        assert_eq!(
                            coordinator.dispatch_online(),
                            Ok(1),
                            "a dirty exit should retry exactly the completed member"
                        );
                    }
                    Ok(HvfVcpuRunMemberOutcome::Handled(HvfVcpuRunStepOutcome::Hvc { .. })) => {
                        let index = result.index();
                        reached_hvc[index] = true;
                        coordinator
                            .set_online(index, false)
                            .expect("an idle epoch-complete member should go offline");
                    }
                    outcome => panic!("unexpected tracked member outcome: {outcome:?}"),
                }
                if reached_hvc == [true, true] {
                    break;
                }
            }

            assert_eq!(reached_hvc, [true, true]);
            assert_eq!(first_write_pages, expected_pages);
            assert!(expected_vcpu0_pages.is_subset(&vcpu0_first_write_pages));
            assert!(expected_vcpu1_pages.is_subset(&vcpu1_first_write_pages));
            let mut expected_epoch_pages = expected_pages.clone();
            if epoch_index == 0 {
                expected_epoch_pages.insert(device_page);
            }
            assert_eq!(
                tracker
                    .dirty_pages()
                    .expect("active tracker query should succeed")
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                expected_epoch_pages
            );

            // SAFETY: these aligned pointers remain inside the live mapped
            // DRAM region owned by `backend`; both HVC exits follow a DMB.
            let (shared_value, vcpu0_values, vcpu1_values) = unsafe {
                (
                    std::ptr::read_volatile(shared_host),
                    vcpu0_hosts.map(|pointer| std::ptr::read_volatile(pointer)),
                    vcpu1_hosts.map(|pointer| std::ptr::read_volatile(pointer)),
                )
            };
            let (vcpu0_value, vcpu1_value) = if epoch_index == 0 {
                (VCPU0_VALUE, VCPU1_VALUE)
            } else {
                (VCPU0_SECOND_VALUE, VCPU1_SECOND_VALUE)
            };
            assert!([u32::from(vcpu0_value), u32::from(vcpu1_value)].contains(&shared_value));
            assert_eq!(vcpu0_values, [u32::from(vcpu0_value); 2]);
            assert_eq!(vcpu1_values, [u32::from(vcpu1_value); 2]);

            assert_eq!(tracker.reset_epoch_quiesced(), Ok(epoch_index + 1));
            assert!(
                tracker
                    .dirty_pages()
                    .expect("advanced epoch should be clean")
                    .is_empty()
            );
            for index in 0..2 {
                coordinator
                    .set_online(index, true)
                    .expect("an idle epoch-complete member should return online");
            }
            if epoch_index == 0 {
                assert_eq!(
                    coordinator.dispatch_online(),
                    Ok(2),
                    "both idle owners should enter the second protected epoch"
                );
            }
        }

        progress_sender
            .send(())
            .expect("dirty progress watchdog should be released");
        watchdog
            .join()
            .expect("dirty progress watchdog should join");
        assert_eq!(
            tracker.stop(),
            Err(HvfDirtyWriteTrackerStopError::OwnersActive { count: 2 })
        );
        assert_eq!(
            coordinator.dispatch_online(),
            Ok(2),
            "both owners should resume into the bounded cancellation target"
        );

        let waiter = coordinator
            .control()
            .request_stop()
            .expect("tracked active runs should accept aggregate cancellation");
        let event = coordinator
            .receive_event()
            .expect("tracked cancellation barrier should drain");
        let HvfVcpuRunEvent::Barrier(report) = event else {
            panic!("tracked cancellation should complete a barrier: {event:?}");
        };
        assert_eq!(report.reason(), HvfVcpuRunControlReason::Stop);
        assert!(report.acknowledgements().iter().all(|result| matches!(
            result.result(),
            Ok(HvfVcpuRunMemberOutcome::Handled(
                HvfVcpuRunStepOutcome::Canceled
            ))
        )));
        assert_eq!(waiter.wait(), Ok(report));
        assert_eq!(
            tracker.stop(),
            Err(HvfDirtyWriteTrackerStopError::OwnersActive { count: 2 })
        );

        coordinator
            .shutdown()
            .expect("tracked coordinator should shut down every owner");
        tracker
            .stop()
            .expect("owner-free tracker should restore remaining clean ranges");
    }
    backend
        .stop_dirty_write_tracking()
        .expect("backend tracker retention should clear idempotently");
    backend
        .destroy_vm()
        .expect("tracked VM should unmap after owner shutdown");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_physical_timer_tval_on_idle_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend
        .create_gic()
        .expect("GIC should be created before the physical-timer vCPU");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_physical_timer_state()
            .expect("first idle physical-timer state should be captured");
        let second = runner
            .capture_arm64_physical_timer_state()
            .expect("second idle physical-timer state should be captured");

        let _first_tval = first.cntp_tval_el0();
        let _second_tval = second.cntp_tval_el0();

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_physical_timer_state_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = PHYSICAL_TIMER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("physical-timer guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    backend
        .create_gic()
        .expect("GIC should be created before the physical-timer vCPU");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest physical-timer writer should exit through HVC")
        else {
            panic!("guest physical-timer writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest physical-timer writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_physical_timer_state()
            .expect("physical-timer state should be captured");
        assert_eq!(state.cntkctl_el1(), PHYSICAL_TIMER_TEST_CNTKCTL_EL1);
        assert_eq!(
            state.cntp_ctl_el0() & PHYSICAL_TIMER_WRITABLE_CONTROL_MASK,
            PHYSICAL_TIMER_TEST_CNTP_CTL_EL0
        );
        assert_eq!(
            state.cntp_ctl_el0() & !PHYSICAL_TIMER_DEFINED_CONTROL_MASK,
            0
        );
        assert!(matches!(
            state.cntp_ctl_el0() & PHYSICAL_TIMER_ISTATUS_MASK,
            0 | PHYSICAL_TIMER_ISTATUS_MASK
        ));
        assert_eq!(state.cntp_cval_el0(), PHYSICAL_TIMER_TEST_CNTP_CVAL_EL0);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_runner_arm64_virtual_timer_state() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let original = runner
            .capture_arm64_virtual_timer_state()
            .expect("original runner vtimer state should be captured");

        runner
            .set_vtimer_mask(true)
            .expect("runner vtimer mask should be set");
        runner
            .set_vtimer_control(0)
            .expect("runner vtimer should be disabled");
        runner
            .set_vtimer_offset(VTIMER_TEST_OFFSET)
            .expect("runner vtimer offset should be set");
        runner
            .set_vtimer_compare_value(VTIMER_TEST_COMPARE_VALUE)
            .expect("runner vtimer compare value should be set");

        let captured = runner
            .capture_arm64_virtual_timer_state()
            .expect("runner vtimer state should be captured");
        assert!(captured.masked());
        assert_eq!(captured.offset(), VTIMER_TEST_OFFSET);
        assert_eq!(captured.control() & VTIMER_WRITABLE_CONTROL_MASK, 0);
        assert_eq!(captured.compare_value(), VTIMER_TEST_COMPARE_VALUE);

        runner
            .set_vtimer_offset(original.offset())
            .expect("original runner vtimer offset should be restored");
        runner
            .set_vtimer_compare_value(original.compare_value())
            .expect("original runner vtimer compare value should be restored");
        runner
            .set_vtimer_control(original.control() & VTIMER_WRITABLE_CONTROL_MASK)
            .expect("original runner vtimer control should be restored");
        runner
            .set_vtimer_mask(original.masked())
            .expect("original runner vtimer mask should be restored");

        let restored = runner
            .capture_arm64_virtual_timer_state()
            .expect("restored runner vtimer state should be captured");
        assert_eq!(restored.masked(), original.masked());
        assert_eq!(restored.offset(), original.offset());
        assert_eq!(
            restored.control() & VTIMER_WRITABLE_CONTROL_MASK,
            original.control() & VTIMER_WRITABLE_CONTROL_MASK
        );
        assert_eq!(restored.compare_value(), original.compare_value());

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn waits_for_retained_runner_virtual_timer_and_drains_control() {
    use std::time::{Duration, Instant};

    use bangbang_hvf::{HvfBackend, HvfVcpuRetainedVtimerWaitOutcome, HvfVcpuRunner};
    use bangbang_runtime::VmBackend;

    fn wait_for_admission(runner: &HvfVcpuRunner<'_>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if runner
                .retained_vtimer_wait_active()
                .expect("retained-wait activity should be observable")
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "retained wait did not publish its admission"
            );
            std::thread::yield_now();
        }
    }

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    let metadata = *backend.create_gic().expect("GIC should be created");
    let virtual_timer_intid = metadata.timer_interrupts.el1_virtual_timer_intid;
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let original = runner
            .capture_arm64_virtual_timer_state()
            .expect("original runner vtimer state should be captured");

        for exit_masked in [false, true] {
            runner
                .set_vtimer_control(0)
                .expect("vtimer should be disabled before programming");
            runner
                .set_vtimer_mask(exit_masked)
                .expect("vtimer exit mask should be selected");
            let deadline = mach_counter_sample().wrapping_add(
                mach_ticks_for(Duration::from_millis(100))
                    .expect("test Mach deadline should fit u64"),
            );
            runner
                .set_vtimer_compare_value(deadline.wrapping_sub(original.offset()))
                .expect("future vtimer comparator should be programmed");
            runner
                .set_vtimer_control(1)
                .expect("future vtimer should be enabled and guest-unmasked");

            assert_eq!(
                runner.wait_for_retained_vtimer(virtual_timer_intid),
                Ok(HvfVcpuRetainedVtimerWaitOutcome::TimerPending)
            );
            let completed_at = mach_counter_sample();
            assert!(
                completed_at.wrapping_sub(deadline) < (1_u64 << 63),
                "retained wait returned before its real Mach deadline"
            );
            runner
                .clear_gic_ppi_pending(virtual_timer_intid)
                .expect("published timer PPI should clear");
        }

        runner
            .set_vtimer_control(0)
            .expect("vtimer should be disabled before programming a due comparator");
        runner
            .set_vtimer_mask(false)
            .expect("due vtimer exits should be unmasked");
        let due = mach_counter_sample();
        runner
            .set_vtimer_compare_value(due.wrapping_sub(original.offset()))
            .expect("due vtimer comparator should be programmed");
        runner
            .set_vtimer_control(1)
            .expect("due vtimer should be enabled and guest-unmasked");
        assert_eq!(
            runner.wait_for_retained_vtimer(virtual_timer_intid),
            Ok(HvfVcpuRetainedVtimerWaitOutcome::TimerPending)
        );
        runner
            .clear_gic_ppi_pending(virtual_timer_intid)
            .expect("due timer PPI should clear");

        for control in [0, 0b11] {
            runner
                .set_vtimer_control(control)
                .expect("indefinite retained timer control should be programmed");
            let cancel = runner.run_cancel_handle();
            std::thread::scope(|scope| {
                let wait = scope.spawn(|| runner.wait_for_retained_vtimer(virtual_timer_intid));
                wait_for_admission(&runner);
                cancel.cancel().expect("retained wait should cancel");
                assert_eq!(
                    wait.join().expect("wait caller should not panic"),
                    Ok(HvfVcpuRetainedVtimerWaitOutcome::Canceled)
                );
            });
        }

        runner
            .set_vtimer_control(0)
            .expect("shutdown retained timer should be disabled");
        std::thread::scope(|scope| {
            let wait = scope.spawn(|| runner.wait_for_retained_vtimer(virtual_timer_intid));
            wait_for_admission(&runner);
            runner
                .shutdown()
                .expect("shutdown should drain retained owner wait");
            assert_eq!(
                wait.join().expect("wait caller should not panic"),
                Ok(HvfVcpuRetainedVtimerWaitOutcome::Canceled)
            );
        });
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_normalized_arm64_timers_across_fresh_hvf_vms() {
    use bangbang_hvf::{HvfArm64SnapshotTimerState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("source VM should be created");
    backend
        .create_gic()
        .expect("source GIC should be created before its vCPU");
    let source = {
        let runner = backend
            .start_vcpu_runner()
            .expect("source vCPU runner should start");
        let state = runner
            .capture_arm64_snapshot_timer_state()
            .expect("source normalized timer state should be captured");
        runner.shutdown().expect("source runner should shut down");
        state
    };
    backend.destroy_vm().expect("source VM should be destroyed");

    backend
        .create_vm()
        .expect("fresh destination VM should be created");
    backend
        .create_gic()
        .expect("destination GIC should be created before its vCPU");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("destination vCPU runner should start");
        runner
            .restore_arm64_snapshot_timer_state(source)
            .expect("source timer state should restore on the fresh unrun vCPU");
        let recaptured = runner
            .capture_arm64_snapshot_timer_state()
            .expect("destination timer state should be recaptured");
        assert_normalized_timer_restore_equivalent(source, recaptured);

        let armed = HvfArm64SnapshotTimerState::try_new(
            true,
            3,
            recaptured.virtual_count(),
            0b11,
            recaptured.virtual_count().wrapping_add(10_000_000),
            0b11,
            10_000_000,
        )
        .expect("armed normalized timer state should be valid");
        runner
            .restore_arm64_snapshot_timer_state(armed)
            .expect("armed masked timer state should restore before first run");
        let recaptured_armed = runner
            .capture_arm64_snapshot_timer_state()
            .expect("armed timer state should be recaptured");
        assert_normalized_timer_restore_equivalent(armed, recaptured_armed);

        runner
            .shutdown()
            .expect("destination runner should shut down");
    }
    backend
        .destroy_vm()
        .expect("destination VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_runner_arm64_pending_interrupt_state() {
    use bangbang_hvf::{HvfBackend, HvfInterruptType};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");

        runner
            .set_pending_interrupt(HvfInterruptType::Irq, true)
            .expect("runner IRQ pending level should be set");
        runner
            .set_pending_interrupt(HvfInterruptType::Fiq, false)
            .expect("runner FIQ pending level should be cleared");
        let irq_only = runner
            .capture_arm64_pending_interrupt_state()
            .expect("IRQ-only pending state should be captured");
        assert!(irq_only.irq_pending());
        assert!(!irq_only.fiq_pending());

        runner
            .set_pending_interrupt(HvfInterruptType::Irq, false)
            .expect("runner IRQ pending level should be cleared");
        runner
            .set_pending_interrupt(HvfInterruptType::Fiq, true)
            .expect("runner FIQ pending level should be set");
        let fiq_only = runner
            .capture_arm64_pending_interrupt_state()
            .expect("FIQ-only pending state should be captured");
        assert!(!fiq_only.irq_pending());
        assert!(fiq_only.fiq_pending());

        runner
            .restore_arm64_pending_interrupt_state(&irq_only)
            .expect("IRQ-only pending state should be restored");
        let restored = runner
            .capture_arm64_pending_interrupt_state()
            .expect("restored pending-interrupt state should be captured");
        assert!(
            restored == irq_only,
            "restored pending-interrupt state should match its source"
        );
        runner
            .restore_arm64_pending_interrupt_state(&irq_only)
            .expect("IRQ-only pending state should be restored a second time");
        let restored_again = runner
            .capture_arm64_pending_interrupt_state()
            .expect("twice-restored pending-interrupt state should be captured");
        assert!(
            restored_again == irq_only,
            "twice-restored pending-interrupt state should match its source"
        );

        runner
            .set_pending_interrupt(HvfInterruptType::Irq, false)
            .expect("runner IRQ pending level should remain cleared");
        runner
            .set_pending_interrupt(HvfInterruptType::Fiq, false)
            .expect("runner FIQ pending level should be cleared");
        let cleared = runner
            .capture_arm64_pending_interrupt_state()
            .expect("cleared pending state should be captured");
        assert!(!cleared.irq_pending());
        assert!(!cleared.fiq_pending());

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn sets_and_clears_runner_gic_ppi_pending() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    let metadata = *backend.create_gic().expect("GIC should be created");
    let virtual_timer_intid = metadata.timer_interrupts.el1_virtual_timer_intid;
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .set_gic_ppi_pending(virtual_timer_intid)
            .expect("runner GIC PPI pending bit should be set");
        runner
            .clear_gic_ppi_pending(virtual_timer_intid)
            .expect("runner GIC PPI pending bit should be cleared");
        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn maps_guest_memory_and_unmaps_before_destroying_vm() {
    use bangbang_hvf::{HvfBackend, HvfMemoryPermissions};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let memory = GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    backend
        .destroy_vm()
        .expect("VM destruction should unmap guest memory first");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn maps_shared_guest_memory_and_exposes_guest_writes_through_its_descriptor() {
    use std::fs::File;
    use std::os::fd::AsFd;
    use std::os::unix::fs::FileExt;
    use std::sync::{Arc, Mutex};

    use bangbang_hvf::{
        HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuRunStepOutcome,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryBacking, GuestMemoryRange, aarch64,
    };
    use bangbang_runtime::mmio::MmioDispatcher;

    const MOV_W1_TEST_VALUE: u32 = 0x5280_0b41;
    const STR_W1_X0: u32 = 0xb900_0001;
    const DMB_ISH: u32 = 0xd503_3bbf;
    const HVC_ZERO: u32 = 0xd400_0002;
    const TEST_VALUE: u32 = 0x5a;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let page_size = host_page_size().expect("host page size should be valid");
    let layout =
        aarch64::dram_layout(page_size * 2).expect("shared guest memory layout should be valid");
    let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
        .expect("shared guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_target = guest_entry
        .checked_add(page_size)
        .expect("guest write target should fit");
    let guest_code = [MOV_W1_TEST_VALUE, STR_W1_X0, DMB_ISH, HVC_ZERO]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("shared guest code should be written");
    let export = memory.regions()[0]
        .try_clone_shared_backing()
        .expect("shared descriptor should clone")
        .expect("shared guest memory should expose a descriptor");
    let export_file = File::from(
        export
            .as_fd()
            .try_clone_to_owned()
            .expect("shared descriptor clone should be independent"),
    );

    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("shared guest memory should be mapped");
    let dynamic_range = GuestMemoryRange::new(
        guest_entry
            .checked_add(page_size * 2)
            .expect("dynamic shared range should fit"),
        page_size,
    )
    .expect("dynamic shared range should validate");
    backend
        .map_dynamic_guest_memory_region(dynamic_range, HvfMemoryPermissions::GUEST_RAM)
        .expect("dynamic shared guest memory should map");
    let tracker = backend
        .start_dirty_write_tracking()
        .expect("shared guest memory should be write-protected");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("shared-memory vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_target,
            })
            .expect("shared-memory guest registers should configure");
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        assert_eq!(
            runner
                .run_once_and_handle_mmio(Arc::clone(&dispatcher))
                .expect("first shared-memory write exit should be handled"),
            HvfVcpuRunStepOutcome::DirtyWrite {
                page: guest_target,
                first_write: true,
            }
        );
        assert!(matches!(
            runner
                .run_once_and_handle_mmio(dispatcher)
                .expect("retried shared-memory guest should reach HVC"),
            HvfVcpuRunStepOutcome::Hvc { exit, .. } if exit.immediate() == 0
        ));
        runner
            .shutdown()
            .expect("shared-memory vCPU runner should shut down");
    }
    assert_eq!(
        tracker
            .dirty_pages()
            .expect("shared-memory dirty pages should query"),
        vec![guest_target]
    );
    tracker
        .stop()
        .expect("owner-free shared-memory tracker should restore write access");
    backend
        .stop_dirty_write_tracking()
        .expect("shared-memory tracker retention should clear");

    let mut descriptor_value = [0_u8; std::mem::size_of::<u32>()];
    export_file
        .read_exact_at(&mut descriptor_value, page_size)
        .expect("shared descriptor should observe the guest write");
    assert_eq!(u32::from_le_bytes(descriptor_value), TEST_VALUE);

    backend
        .unmap_dynamic_guest_memory_region(dynamic_range)
        .expect("dynamic shared guest memory should unmap");
    backend
        .unmap_guest_memory()
        .expect("shared guest memory should unmap");
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn prepares_internal_hvf_arm64_boot_session() {
    use bangbang_hvf::{ARM64_LINUX_BOOT_CPSR, HvfArm64BootSessionConfig, HvfBackend};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::{PmemConfigInput, PmemMmioLayout, VIRTIO_PMEM_ALIGNMENT};
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("session-kernel", &image).expect("temp kernel should be created");
    let writable_pmem = TempFile::new_len("session-writable-pmem", VIRTIO_PMEM_ALIGNMENT)
        .expect("temp writable pmem should be created");
    let readonly_pmem = TempFile::new_len("session-readonly-pmem", VIRTIO_PMEM_ALIGNMENT)
        .expect("temp readonly pmem should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    controller
        .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
            "pmem0",
            path_text(writable_pmem.path()),
        )))
        .expect("writable pmem config should be stored");
    controller
        .handle_action(VmmAction::PutPmem(
            PmemConfigInput::new("pmem1", path_text(readonly_pmem.path())).with_read_only(true),
        ))
        .expect("readonly pmem config should be stored");
    let mut backend = HvfBackend::new();
    let pmem_mmio_layout =
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500));
    let rtc_mmio_layout = test_rtc_mmio_layout();
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        pmem_mmio_layout,
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        rtc_mmio_layout,
    );

    let mut session = backend
        .prepare_arm64_boot_session(&controller, config.clone())
        .expect("internal HVF arm64 boot session should prepare");

    let mmio_dispatcher = session.mmio_dispatcher();
    let mmio_regions = mmio_dispatcher
        .try_lock()
        .expect("session MMIO dispatcher should lock")
        .regions()
        .to_vec();
    assert_eq!(mmio_regions.len(), 3);
    let first_pmem_region = mmio_regions
        .iter()
        .find(|region| region.id() == pmem_mmio_layout.base_region_id())
        .expect("first pmem MMIO region should be registered");
    assert_eq!(
        first_pmem_region.range().start(),
        pmem_mmio_layout.base_address()
    );
    assert_eq!(
        first_pmem_region.range().size(),
        bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE
    );
    let second_pmem_region_id =
        MmioRegionId::new(pmem_mmio_layout.base_region_id().raw_value() + 1);
    let second_pmem_region = mmio_regions
        .iter()
        .find(|region| region.id() == second_pmem_region_id)
        .expect("second pmem MMIO region should be registered");
    assert_eq!(second_pmem_region.id(), second_pmem_region_id);
    assert_eq!(
        second_pmem_region.range().start(),
        pmem_mmio_layout
            .base_address()
            .checked_add(pmem_mmio_layout.address_stride())
            .expect("second pmem MMIO address should fit")
    );
    assert_eq!(
        second_pmem_region.range().size(),
        bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE
    );
    let rtc_region = mmio_regions
        .iter()
        .find(|region| region.id() == rtc_mmio_layout.region_id())
        .expect("RTC MMIO region should be registered");
    assert_eq!(rtc_region.range().start(), rtc_mmio_layout.base());
    assert_eq!(
        rtc_region.range().size(),
        bangbang_runtime::rtc::RTC_MMIO_DEVICE_WINDOW_SIZE
    );
    assert!(session.block_interrupt_lines().is_empty());
    assert_eq!(session.pmem_interrupt_lines().len(), 2);
    assert_eq!(session.runtime_resources().pmem_devices.len(), 2);
    assert!(
        !session.runtime_resources().pmem_devices[0]
            .mapping()
            .is_read_only()
    );
    assert!(
        session.runtime_resources().pmem_devices[1]
            .mapping()
            .is_read_only()
    );
    assert!(
        !session.runtime_resources().pmem_devices[0]
            .guest_range()
            .overlaps(session.runtime_resources().layout.ranges()[0])
    );
    assert!(
        !session.runtime_resources().pmem_devices[0]
            .guest_range()
            .overlaps(session.runtime_resources().pmem_devices[1].guest_range())
    );
    assert_eq!(
        session
            .guest_memory()
            .expect("session should expose mapped guest memory")
            .total_size(),
        session.runtime_resources().layout.total_size()
    );
    let boot_origin = session
        .runtime_resources()
        .boot_origin
        .as_ref()
        .expect("ordinary session should retain boot-origin metadata");
    let boot_registers = session
        .boot_registers()
        .expect("ordinary session should retain boot registers");
    let mut fdt_magic = [0; 4];
    session
        .guest_memory()
        .expect("session should expose mapped guest memory")
        .read_slice(&mut fdt_magic, boot_origin.fdt.address)
        .expect("mapped guest memory should contain the written FDT");
    assert_eq!(u32::from_be_bytes(fdt_magic), 0xd00d_feed);
    assert_eq!(
        boot_registers.kernel_entry,
        boot_origin.loaded_boot_source.kernel.entry_address
    );
    assert_eq!(boot_registers.fdt_address, boot_origin.fdt.address);
    let register_state = session
        .capture_arm64_general_register_state()
        .expect("internal session should capture general-register state");
    assert_eq!(
        register_state.general_purpose_register(0),
        Some(boot_registers.fdt_address.raw_value())
    );
    assert_eq!(register_state.pc(), boot_registers.kernel_entry.raw_value());
    assert_eq!(register_state.cpsr(), ARM64_LINUX_BOOT_CPSR);
    session
        .restore_arm64_general_register_state(&register_state)
        .expect("internal session should restore general-register state");
    let core_system_register_state = session
        .capture_arm64_core_system_register_state()
        .expect("internal session should capture core system-register state");
    session
        .restore_arm64_core_system_register_state(&core_system_register_state)
        .expect("internal session should restore core system-register state");
    let exception_register_state = session
        .capture_arm64_exception_register_state()
        .expect("internal session should capture exception-register state");
    session
        .restore_arm64_exception_register_state(&exception_register_state)
        .expect("internal session should restore exception-register state");
    let execution_control_state = session
        .capture_arm64_execution_control_register_state()
        .expect("internal session should capture execution-control state");
    session
        .restore_arm64_execution_control_register_state(&execution_control_state)
        .expect("internal session should restore execution-control state");
    let cache_selection_state = session
        .capture_arm64_cache_selection_register_state()
        .expect("internal session should capture cache-selection state");
    session
        .restore_arm64_cache_selection_register_state(&cache_selection_state)
        .expect("internal session should restore cache-selection state");
    session
        .capture_arm64_breakpoint_register_state()
        .expect("internal session should capture breakpoint-register state");
    session
        .capture_arm64_watchpoint_register_state()
        .expect("internal session should capture watchpoint-register state");
    let debug_control_state = session
        .capture_arm64_debug_control_register_state()
        .expect("internal session should capture debug-control state");
    session
        .restore_arm64_debug_control_register_state(&debug_control_state)
        .expect("internal session should restore debug-control state");
    let debug_trap_state = session
        .capture_arm64_debug_trap_state()
        .expect("internal session should capture debug-trap state");
    session
        .restore_arm64_debug_trap_state(&debug_trap_state)
        .expect("internal session should restore debug-trap state");
    session
        .capture_arm64_identification_register_state()
        .expect("internal session should capture identification-register state");
    session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("internal session should capture SVE/SME identification state");
    let _sme_pstate =
        assert_sme_pstate_capture_supported_or_unavailable(session.capture_arm64_sme_pstate())
            .expect("internal session SME PSTATE capture should succeed or report unsupported");
    let _sme_p_registers = assert_sme_p_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_p_register_state(),
    )
    .expect("internal session SME P-register capture should succeed or report unavailable");
    let _sme_z_registers = assert_sme_z_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_z_register_state(),
    )
    .expect("internal session SME Z-register capture should succeed or report unavailable");
    let _sme_za_register = assert_sme_za_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_za_register_state(),
    )
    .expect("internal session SME ZA-register capture should succeed or report unavailable");
    let _sme_zt0_register = assert_sme_zt0_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_zt0_register_state(),
    )
    .expect("internal session SME ZT0-register capture should succeed or report unavailable");
    session
        .capture_arm64_sme_system_register_state()
        .expect("internal session should capture SME system-register state");
    let system_context_state = session
        .capture_arm64_system_context_register_state()
        .expect("internal session should capture system-context register state");
    session
        .restore_arm64_system_context_register_state(&system_context_state)
        .expect("internal session should restore system-context register state");
    let translation_state = session
        .capture_arm64_translation_register_state()
        .expect("internal session should capture translation-register state");
    session
        .restore_arm64_translation_register_state(&translation_state)
        .expect("internal session should restore translation-register state");
    let pointer_authentication_key_state = session
        .capture_arm64_pointer_authentication_key_state()
        .expect("internal session should capture pointer-authentication key state");
    session
        .restore_arm64_pointer_authentication_key_state(&pointer_authentication_key_state)
        .expect("internal session should restore pointer-authentication key state");
    let thread_context_state = session
        .capture_arm64_thread_context_register_state()
        .expect("internal session should capture thread-context register state");
    session
        .restore_arm64_thread_context_register_state(&thread_context_state)
        .expect("internal session should restore thread-context register state");
    let simd_fp_state = session
        .capture_arm64_simd_fp_state()
        .expect("internal session should capture SIMD/FP state");
    session
        .restore_arm64_simd_fp_state(&simd_fp_state)
        .expect("internal session should restore SIMD/FP state");
    session
        .capture_arm64_physical_timer_state()
        .expect("internal session should capture physical-timer state");
    session
        .capture_arm64_virtual_timer_state()
        .expect("internal session should capture virtual-timer state");
    let snapshot_timer_state = session
        .capture_arm64_snapshot_timer_state()
        .expect("internal session should capture normalized timer state");
    let pending_interrupt_state = session
        .capture_arm64_pending_interrupt_state()
        .expect("internal session should capture pending-interrupt state");
    session
        .restore_arm64_pending_interrupt_state(&pending_interrupt_state)
        .expect("internal session should restore pending-interrupt state");
    let gic_device_state = session
        .capture_gic_device_state()
        .expect("internal session should capture GIC device state");
    assert!(!gic_device_state.is_empty());
    let gic_icc_register_state = session
        .capture_arm64_gic_icc_register_state()
        .expect("internal session should capture GIC ICC register state");
    session
        .restore_gic_device_state(&gic_device_state)
        .expect("internal session should restore GIC device state before run");
    session
        .restore_arm64_gic_icc_register_state(&gic_icc_register_state)
        .expect("internal session should restore GIC ICC register state before run");
    let restored_gic_icc_register_state = session
        .capture_arm64_gic_icc_register_state()
        .expect("internal session should capture GIC ICC register state");
    assert!(
        restored_gic_icc_register_state == gic_icc_register_state,
        "internal session should preserve original GIC ICC register state"
    );
    session
        .restore_arm64_snapshot_timer_state(snapshot_timer_state)
        .expect("internal session should restore normalized timers after GIC state");
    assert_normalized_timer_restore_equivalent(
        snapshot_timer_state,
        session
            .capture_arm64_snapshot_timer_state()
            .expect("internal session should recapture normalized timers"),
    );
    let old_vmgenid = session.runtime_resources().vmgenid_device;
    session
        .replace_vmgenid_for_snapshot_restore()
        .expect("internal session should replace VMGenID and inject its SPI");
    let new_vmgenid = session.runtime_resources().vmgenid_device;
    assert_ne!(new_vmgenid.generation_id, old_vmgenid.generation_id);
    assert_eq!(new_vmgenid.range, old_vmgenid.range);
    assert_eq!(new_vmgenid.fdt_device, old_vmgenid.fdt_device);
    let mut guest_vmgenid = [0; bangbang_runtime::startup::ARM64_BOOT_VMGENID_SIZE];
    session
        .guest_memory()
        .expect("internal session should expose VMGenID guest memory")
        .read_slice(&mut guest_vmgenid, new_vmgenid.range.start())
        .expect("internal session replacement VMGenID should read");
    assert_eq!(guest_vmgenid, new_vmgenid.generation_id);
    let run_cancel_handle = session.run_cancel_handle();
    drop(run_cancel_handle);
    let run_loop_control = session.run_loop_control();
    let run_loop_stop_token = run_loop_control.stop_token();
    run_loop_control
        .request_stop()
        .expect("internal HVF boot-session run-loop stop should request vCPU cancellation");
    assert!(run_loop_stop_token.is_stop_requested());
    session
        .shutdown()
        .expect("internal HVF arm64 boot session should shut down");
    drop(session);

    let mut second_session = backend
        .prepare_arm64_boot_session(&controller, config)
        .expect("second internal HVF arm64 boot session should prepare after shutdown");
    assert_eq!(
        second_session
            .guest_memory_mut()
            .expect("second session should expose mutable mapped guest memory")
            .total_size(),
        second_session.runtime_resources().layout.total_size()
    );
    second_session
        .shutdown()
        .expect("second internal HVF arm64 boot session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn capture_ready_storage_traverses_signed_mmio_and_pci_owners() {
    use std::time::Instant;

    use bangbang_hvf::{HvfArm64BootSessionConfig, OwnedHvfArm64BootSession};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::{
        BlockCaptureIoEngine, BlockMmioLayout, DriveCacheType, DriveConfigInput, DriveIoEngine,
        PreparedBlockDevice,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::{
        PmemConfig, PmemConfigInput, PmemFileBacking, PmemMmioLayout, VIRTIO_PMEM_ALIGNMENT,
    };
    use bangbang_runtime::storage_capture::{
        CaptureReadyStorageConfigs, StorageDeviceOrigin, StorageTransportState,
    };
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");

    let mmio_kernel = TempFile::new("capture-ready-mmio-kernel", &image)
        .expect("MMIO capture kernel should create");
    let mmio_root = TempFile::new_len("capture-ready-mmio-root", 4096)
        .expect("MMIO Sync backing should create");
    let mmio_async = TempFile::new_len("capture-ready-mmio-async", 4096)
        .expect("MMIO Async backing should create");
    let mmio_pmem = TempFile::new_len("capture-ready-mmio-pmem", VIRTIO_PMEM_ALIGNMENT)
        .expect("MMIO pmem backing should create");
    let mut mmio_controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    mmio_controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            mmio_kernel.path(),
        )))
        .expect("MMIO capture boot source should configure");
    mmio_controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", mmio_root.path(), true)
                .with_is_read_only(true)
                .with_io_engine(DriveIoEngine::Sync),
        ))
        .expect("MMIO Sync root should configure");
    mmio_controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("async", "async", mmio_async.path(), false)
                .with_is_read_only(false)
                .with_cache_type(DriveCacheType::Writeback)
                .with_io_engine(DriveIoEngine::Async),
        ))
        .expect("MMIO Async data drive should configure");
    mmio_controller
        .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
            "pmem0",
            path_text(mmio_pmem.path()),
        )))
        .expect("MMIO pmem should configure");
    let mmio_session_config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut mmio_session = OwnedHvfArm64BootSession::new(&mmio_controller, mmio_session_config)
        .expect("signed MMIO storage session should prepare");
    let mmio_configs = CaptureReadyStorageConfigs::new(
        mmio_controller.drive_configs().to_vec(),
        mmio_controller.pmem_configs().to_vec(),
    );
    let mmio_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO retry publishers should quiesce");
    let mmio_first = mmio_session
        .capture_ready_storage_state_at(&mmio_configs, &mmio_guard, Instant::now())
        .expect("signed MMIO storage should become capture-ready");
    let mmio_second = mmio_session
        .capture_ready_storage_state_at(&mmio_configs, &mmio_guard, Instant::now())
        .expect("MMIO Async admission should reopen for a second capture");

    assert_eq!(mmio_first.block_devices().len(), 2);
    assert_eq!(mmio_first.pmem_devices().len(), 1);
    for (captured, configured) in mmio_first
        .block_devices()
        .iter()
        .zip(mmio_controller.drive_configs())
    {
        assert_eq!(captured.config(), configured);
        assert!(matches!(
            captured.transport(),
            StorageTransportState::Mmio(_)
        ));
    }
    assert_eq!(
        mmio_first.block_devices()[0].device().io_engine(),
        BlockCaptureIoEngine::Sync
    );
    let BlockCaptureIoEngine::Async(mmio_async_first) =
        mmio_first.block_devices()[1].device().io_engine()
    else {
        panic!("second MMIO drive should retain Async continuation state");
    };
    let BlockCaptureIoEngine::Async(mmio_async_second) =
        mmio_second.block_devices()[1].device().io_engine()
    else {
        panic!("second MMIO capture should retain Async continuation state");
    };
    assert_eq!(
        mmio_async_second.generation(),
        mmio_async_first.generation()
    );
    assert!(mmio_async_first.admission_stopped());
    assert_eq!(mmio_async_first.owned_operations(), 0);
    assert_eq!(mmio_async_first.parked_host_completions(), 0);
    assert_eq!(mmio_async_first.final_completions(), 0);
    assert_eq!(
        mmio_first.pmem_devices()[0].config(),
        &mmio_controller.pmem_configs()[0]
    );
    assert!(matches!(
        mmio_first.pmem_devices()[0].transport(),
        StorageTransportState::Mmio(_)
    ));
    assert!(
        mmio_first.pmem_devices()[0]
            .mapping()
            .same_mapping(mmio_second.pmem_devices()[0].mapping())
    );
    let mmio_debug = format!(
        "{:?} {:?}",
        mmio_first.block_devices(),
        mmio_first.pmem_devices()
    );
    for private_path in [
        path_text(mmio_root.path()),
        path_text(mmio_async.path()),
        path_text(mmio_pmem.path()),
    ] {
        assert!(!mmio_debug.contains(&private_path));
    }
    drop(mmio_guard);
    mmio_session
        .shutdown()
        .expect("signed MMIO storage session should shut down");

    let pci_kernel = TempFile::new("capture-ready-pci-kernel", &image)
        .expect("PCI capture kernel should create");
    let pci_root = TempFile::new_len("capture-ready-pci-root", 4096)
        .expect("startup PCI Sync backing should create");
    let pci_startup_pmem =
        TempFile::new_len("capture-ready-pci-startup-pmem", VIRTIO_PMEM_ALIGNMENT)
            .expect("startup PCI pmem backing should create");
    let pci_dynamic_async = TempFile::new_len("capture-ready-pci-dynamic-async", 4096)
        .expect("runtime PCI Async backing should create");
    let pci_dynamic_pmem =
        TempFile::new_len("capture-ready-pci-dynamic-pmem", VIRTIO_PMEM_ALIGNMENT)
            .expect("runtime PCI pmem backing should create");
    let mut pci_controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    pci_controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            pci_kernel.path(),
        )))
        .expect("PCI capture boot source should configure");
    pci_controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", pci_root.path(), true)
                .with_is_read_only(true)
                .with_io_engine(DriveIoEngine::Sync),
        ))
        .expect("startup PCI Sync root should configure");
    pci_controller
        .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
            "startup_pmem",
            path_text(pci_startup_pmem.path()),
        )))
        .expect("startup PCI pmem should configure");
    let pci_session_config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    )
    .with_pci_enabled();
    let mut pci_session = OwnedHvfArm64BootSession::new(&pci_controller, pci_session_config)
        .expect("signed startup PCI storage session should prepare");

    let dynamic_drive_input =
        DriveConfigInput::new("hotdata", "hotdata", pci_dynamic_async.path(), false)
            .with_is_read_only(false)
            .with_cache_type(DriveCacheType::Writeback)
            .with_io_engine(DriveIoEngine::Async);
    let dynamic_drive = dynamic_drive_input
        .clone()
        .validate()
        .expect("runtime PCI Async config should validate");
    pci_controller
        .handle_action(VmmAction::PutDrive(dynamic_drive_input))
        .expect("runtime PCI Async config should join current inventory");
    pci_session
        .insert_runtime_block_device(
            PreparedBlockDevice::from_config_with_backing(&dynamic_drive, None)
                .expect("runtime PCI Async device should prepare"),
        )
        .expect("runtime PCI Async device should publish");

    let dynamic_pmem_input = PmemConfigInput::new("hotpmem", path_text(pci_dynamic_pmem.path()));
    let dynamic_pmem = PmemConfig::try_from(dynamic_pmem_input.clone())
        .expect("runtime PCI pmem config should validate");
    pci_controller
        .handle_action(VmmAction::PutPmem(dynamic_pmem_input))
        .expect("runtime PCI pmem config should join current inventory");
    pci_session
        .insert_runtime_pmem_device(
            &dynamic_pmem,
            PmemFileBacking::open(&dynamic_pmem).expect("runtime PCI pmem backing should open"),
        )
        .expect("runtime PCI pmem device should publish");

    let pci_configs = CaptureReadyStorageConfigs::new(
        pci_controller.drive_configs().to_vec(),
        pci_controller.pmem_configs().to_vec(),
    );
    let pci_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI retry publishers should quiesce");
    let pci_first = pci_session
        .capture_ready_storage_state_at(&pci_configs, &pci_guard, Instant::now())
        .expect("signed startup/runtime PCI storage should become capture-ready");
    let pci_second = pci_session
        .capture_ready_storage_state_at(&pci_configs, &pci_guard, Instant::now())
        .expect("runtime PCI Async admission should reopen for a second capture");

    assert_eq!(pci_first.block_devices().len(), 2);
    assert_eq!(pci_first.pmem_devices().len(), 2);
    for (captured, configured) in pci_first
        .block_devices()
        .iter()
        .zip(pci_controller.drive_configs())
    {
        assert_eq!(captured.config(), configured);
    }
    for (captured, configured) in pci_first
        .pmem_devices()
        .iter()
        .zip(pci_controller.pmem_configs())
    {
        assert_eq!(captured.config(), configured);
    }
    let block_origins = pci_first
        .block_devices()
        .iter()
        .map(|device| match device.transport() {
            StorageTransportState::Pci(transport) => transport.origin(),
            StorageTransportState::Mmio(_) => panic!("PCI block should not capture as MMIO"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        block_origins,
        vec![StorageDeviceOrigin::Startup, StorageDeviceOrigin::Runtime]
    );
    let pmem_origins = pci_first
        .pmem_devices()
        .iter()
        .map(|device| match device.transport() {
            StorageTransportState::Pci(transport) => transport.origin(),
            StorageTransportState::Mmio(_) => panic!("PCI pmem should not capture as MMIO"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pmem_origins,
        vec![StorageDeviceOrigin::Startup, StorageDeviceOrigin::Runtime]
    );
    let BlockCaptureIoEngine::Async(pci_async_first) =
        pci_first.block_devices()[1].device().io_engine()
    else {
        panic!("runtime PCI drive should retain Async continuation state");
    };
    let BlockCaptureIoEngine::Async(pci_async_second) =
        pci_second.block_devices()[1].device().io_engine()
    else {
        panic!("second runtime PCI capture should retain Async continuation state");
    };
    assert_eq!(pci_async_second.generation(), pci_async_first.generation());
    assert!(pci_async_first.admission_stopped());
    assert_eq!(pci_async_first.owned_operations(), 0);
    assert_eq!(pci_async_first.parked_host_completions(), 0);
    assert_eq!(pci_async_first.final_completions(), 0);
    for (first, second) in pci_first
        .pmem_devices()
        .iter()
        .zip(pci_second.pmem_devices())
    {
        assert!(first.mapping().same_mapping(second.mapping()));
    }
    let pci_debug = format!(
        "{:?} {:?}",
        pci_first.block_devices(),
        pci_first.pmem_devices()
    );
    for private_path in [
        path_text(pci_root.path()),
        path_text(pci_startup_pmem.path()),
        path_text(pci_dynamic_async.path()),
        path_text(pci_dynamic_pmem.path()),
    ] {
        assert!(!pci_debug.contains(&private_path));
    }
    drop(pci_guard);
    pci_session
        .shutdown()
        .expect("signed PCI storage session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn capture_ready_balloon_traverses_signed_mmio_and_pci_owners() {
    use bangbang_hvf::{
        HvfArm64BootBalloonCaptureError, HvfArm64BootBalloonDeviceConfig,
        HvfArm64BootBalloonTransportState, HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::balloon::{BalloonConfigInput, BalloonMmioLayout, available_features};
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("capture-ready-balloon-kernel", &image)
        .expect("balloon capture kernel should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("balloon capture boot source should configure");
    controller
        .handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(8, true)
                .with_stats_polling_interval_s(1)
                .with_free_page_hinting(true)
                .with_free_page_reporting(true),
        ))
        .expect("balloon capture device should configure");
    let balloon_config = controller
        .balloon_config()
        .expect("balloon config should exist");
    let balloon_device = HvfArm64BootBalloonDeviceConfig::new(BalloonMmioLayout::new(
        GuestAddress::new(0x4000_8000),
        MmioRegionId::new(4000),
    ));
    let base_session_config = || {
        HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            test_rtc_mmio_layout(),
        )
        .with_balloon_device(balloon_device)
    };

    let mut mmio_session = OwnedHvfArm64BootSession::new(&controller, base_session_config())
        .expect("signed MMIO balloon session should prepare");
    let mmio_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO auxiliary publishers should quiesce");
    let mmio_first = mmio_session
        .capture_ready_balloon_state(Some(balloon_config), &mmio_guard)
        .expect("signed MMIO balloon should become capture-ready")
        .expect("configured MMIO balloon should be captured");
    let mmio_second = mmio_session
        .capture_ready_balloon_state(Some(balloon_config), &mmio_guard)
        .expect("signed MMIO balloon should support repeated detached capture")
        .expect("configured MMIO balloon should remain captured");
    assert_eq!(mmio_first.config(), balloon_config);
    let HvfArm64BootBalloonTransportState::Mmio { state, .. } = mmio_first.transport() else {
        panic!("MMIO balloon should retain MMIO ownership");
    };
    assert_eq!(
        state.device().available_features(),
        available_features(balloon_config)
    );
    assert!(state.device().active_queues().is_none());
    assert_eq!(mmio_first, mmio_second);
    assert!(!format!("{mmio_first:?}").contains("40008000"));
    assert!(matches!(
        mmio_session.capture_ready_balloon_state(None, &mmio_guard),
        Err(HvfArm64BootBalloonCaptureError::OwnershipMismatch {
            configured: false,
            mmio_owner: true,
            pci_owner: false,
        })
    ));
    drop(mmio_guard);
    mmio_session
        .shutdown()
        .expect("signed MMIO balloon session should shut down");

    let mut pci_session =
        OwnedHvfArm64BootSession::new(&controller, base_session_config().with_pci_enabled())
            .expect("signed PCI balloon session should prepare");
    let pci_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI auxiliary publishers should quiesce");
    let pci = pci_session
        .capture_ready_balloon_state(Some(balloon_config), &pci_guard)
        .expect("signed PCI balloon should become capture-ready")
        .expect("configured PCI balloon should be captured");
    let HvfArm64BootBalloonTransportState::Pci {
        sbdf,
        bar_range,
        state,
    } = pci.transport()
    else {
        panic!("PCI balloon should retain PCI ownership");
    };
    assert!(sbdf.device() > 0);
    assert_eq!(
        bar_range.size(),
        bangbang_runtime::virtio_pci::VIRTIO_PCI_CAPABILITY_BAR_SIZE
    );
    assert_eq!(
        state.device().available_features(),
        available_features(balloon_config)
    );
    assert!(state.device().active_queues().is_none());
    assert!(!format!("{pci:?}").contains("40008000"));
    drop(pci_guard);
    pci_session
        .shutdown()
        .expect("signed PCI balloon session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_QUEUE_SIZE: u16 = 8;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_DESCRIPTOR_TABLE: bangbang_runtime::memory::GuestAddress =
    bangbang_runtime::memory::GuestAddress::new(0x8040_0000);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_AVAILABLE_RING: bangbang_runtime::memory::GuestAddress =
    bangbang_runtime::memory::GuestAddress::new(0x8041_0000);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_USED_RING: bangbang_runtime::memory::GuestAddress =
    bangbang_runtime::memory::GuestAddress::new(0x8042_0000);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_FIRST_DATA: bangbang_runtime::memory::GuestAddress =
    bangbang_runtime::memory::GuestAddress::new(0x8043_0000);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_SECOND_DATA: bangbang_runtime::memory::GuestAddress =
    bangbang_runtime::memory::GuestAddress::new(0x8044_0000);

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_entropy_capture_mmio(
    dispatcher: &mut bangbang_runtime::mmio::MmioDispatcher,
    address: bangbang_runtime::memory::GuestAddress,
    data: &[u8],
) {
    use bangbang_runtime::mmio::{MmioAccessBytes, MmioDispatchOutcome, MmioOperation};

    let access = dispatcher
        .lookup(
            address,
            u64::try_from(data.len()).expect("entropy MMIO write length should fit u64"),
        )
        .expect("entropy MMIO write should resolve");
    let outcome = dispatcher
        .dispatch(
            MmioOperation::write(
                access,
                MmioAccessBytes::new(data).expect("entropy MMIO bytes should validate"),
            )
            .expect("entropy MMIO operation should validate"),
        )
        .expect("entropy MMIO write should dispatch");
    assert!(matches!(outcome, MmioDispatchOutcome::Write));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_entropy_capture_queue(memory: &mut bangbang_runtime::memory::GuestMemory) {
    use bangbang_runtime::virtio_queue::{VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE};

    for (index, data_address) in [ENTROPY_CAPTURE_FIRST_DATA, ENTROPY_CAPTURE_SECOND_DATA]
        .into_iter()
        .enumerate()
    {
        let descriptor_address = ENTROPY_CAPTURE_DESCRIPTOR_TABLE
            .checked_add(
                u64::try_from(index).expect("descriptor index should fit u64")
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE)
                        .expect("descriptor size should fit u64"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&data_address.raw_value().to_le_bytes(), descriptor_address)
            .expect("entropy descriptor address should write");
        memory
            .write_slice(
                &4_u32.to_le_bytes(),
                descriptor_address.checked_add(8).unwrap(),
            )
            .expect("entropy descriptor length should write");
        memory
            .write_slice(
                &VIRTQUEUE_DESC_F_WRITE.to_le_bytes(),
                descriptor_address.checked_add(12).unwrap(),
            )
            .expect("entropy descriptor flags should write");
        memory
            .write_slice(
                &0_u16.to_le_bytes(),
                descriptor_address.checked_add(14).unwrap(),
            )
            .expect("entropy descriptor next index should write");
    }
    memory
        .write_slice(&0_u16.to_le_bytes(), ENTROPY_CAPTURE_AVAILABLE_RING)
        .expect("entropy available flags should write");
    memory
        .write_slice(
            &2_u16.to_le_bytes(),
            ENTROPY_CAPTURE_AVAILABLE_RING.checked_add(2).unwrap(),
        )
        .expect("entropy available index should write");
    memory
        .write_slice(
            &0_u16.to_le_bytes(),
            ENTROPY_CAPTURE_AVAILABLE_RING.checked_add(4).unwrap(),
        )
        .expect("first entropy available head should write");
    memory
        .write_slice(
            &1_u16.to_le_bytes(),
            ENTROPY_CAPTURE_AVAILABLE_RING.checked_add(6).unwrap(),
        )
        .expect("second entropy available head should write");
    memory
        .write_slice(&[0; 4], ENTROPY_CAPTURE_USED_RING)
        .expect("entropy used ring header should reset");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn activate_and_notify_entropy_capture_queue(
    session: &mut bangbang_hvf::OwnedHvfArm64BootSession,
    transport_base: bangbang_runtime::memory::GuestAddress,
    pci: bool,
) -> std::time::Duration {
    use bangbang_runtime::entropy::VirtioRngOsEntropySource;
    use bangbang_runtime::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK, VirtioMmioRegister,
    };
    use bangbang_runtime::virtio_pci::VIRTIO_PCI_NOTIFICATION_OFFSET;

    let dispatcher = session.mmio_dispatcher();
    write_entropy_capture_queue(
        session
            .guest_memory_mut()
            .expect("signed entropy guest memory should remain mapped"),
    );
    let mut dispatcher = dispatcher
        .lock()
        .expect("signed entropy MMIO dispatcher should not be poisoned");
    let write =
        |dispatcher: &mut bangbang_runtime::mmio::MmioDispatcher, offset: u64, data: &[u8]| {
            write_entropy_capture_mmio(
                dispatcher,
                transport_base
                    .checked_add(offset)
                    .expect("entropy transport address should not overflow"),
                data,
            );
        };
    let features_ok = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    let driver_ok = features_ok | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    if pci {
        write(
            &mut dispatcher,
            0x14,
            &[u8::try_from(VIRTIO_DEVICE_STATUS_ACKNOWLEDGE).unwrap()],
        );
        write(
            &mut dispatcher,
            0x14,
            &[
                u8::try_from(VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER)
                    .unwrap(),
            ],
        );
        write(&mut dispatcher, 0x08, &1_u32.to_le_bytes());
        write(&mut dispatcher, 0x0c, &1_u32.to_le_bytes());
        write(&mut dispatcher, 0x14, &[u8::try_from(features_ok).unwrap()]);
        write(&mut dispatcher, 0x16, &0_u16.to_le_bytes());
        write(
            &mut dispatcher,
            0x18,
            &ENTROPY_CAPTURE_QUEUE_SIZE.to_le_bytes(),
        );
        write(
            &mut dispatcher,
            0x20,
            &u32::try_from(ENTROPY_CAPTURE_DESCRIPTOR_TABLE.raw_value())
                .unwrap()
                .to_le_bytes(),
        );
        write(
            &mut dispatcher,
            0x28,
            &u32::try_from(ENTROPY_CAPTURE_AVAILABLE_RING.raw_value())
                .unwrap()
                .to_le_bytes(),
        );
        write(
            &mut dispatcher,
            0x30,
            &u32::try_from(ENTROPY_CAPTURE_USED_RING.raw_value())
                .unwrap()
                .to_le_bytes(),
        );
        write(&mut dispatcher, 0x1c, &1_u16.to_le_bytes());
        write(&mut dispatcher, 0x14, &[u8::try_from(driver_ok).unwrap()]);
        write(
            &mut dispatcher,
            VIRTIO_PCI_NOTIFICATION_OFFSET,
            &0_u16.to_le_bytes(),
        );
    } else {
        for status in [
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        ] {
            write(
                &mut dispatcher,
                VirtioMmioRegister::Status.offset(),
                &status.to_le_bytes(),
            );
        }
        write(
            &mut dispatcher,
            VirtioMmioRegister::DriverFeaturesSel.offset(),
            &1_u32.to_le_bytes(),
        );
        write(
            &mut dispatcher,
            VirtioMmioRegister::DriverFeatures.offset(),
            &1_u32.to_le_bytes(),
        );
        write(
            &mut dispatcher,
            VirtioMmioRegister::Status.offset(),
            &features_ok.to_le_bytes(),
        );
        for (register, value) in [
            (
                VirtioMmioRegister::QueueNum,
                u32::from(ENTROPY_CAPTURE_QUEUE_SIZE),
            ),
            (
                VirtioMmioRegister::QueueDescLow,
                u32::try_from(ENTROPY_CAPTURE_DESCRIPTOR_TABLE.raw_value()).unwrap(),
            ),
            (
                VirtioMmioRegister::QueueDriverLow,
                u32::try_from(ENTROPY_CAPTURE_AVAILABLE_RING.raw_value()).unwrap(),
            ),
            (
                VirtioMmioRegister::QueueDeviceLow,
                u32::try_from(ENTROPY_CAPTURE_USED_RING.raw_value()).unwrap(),
            ),
            (VirtioMmioRegister::QueueReady, 1),
        ] {
            write(&mut dispatcher, register.offset(), &value.to_le_bytes());
        }
        write(
            &mut dispatcher,
            VirtioMmioRegister::Status.offset(),
            &driver_ok.to_le_bytes(),
        );
        write(
            &mut dispatcher,
            VirtioMmioRegister::QueueNotify.offset(),
            &0_u32.to_le_bytes(),
        );
    }
    drop(dispatcher);

    let mut source = VirtioRngOsEntropySource::new();
    session
        .dispatch_entropy_queue_notifications_and_schedule_retry_wakeup(&mut source)
        .expect("signed entropy owner should dispatch and schedule retry")
        .rate_limiter_retry_after()
        .expect("second entropy descriptor should be throttled")
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn capture_ready_entropy_traverses_signed_mmio_and_pci_owners() {
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootEntropyCaptureError, HvfArm64BootEntropyDeviceConfig,
        HvfArm64BootEntropyTransportState, HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::entropy::{
        EntropyConfigInput, EntropyMmioLayout, EntropyRateLimiterConfig, EntropyTokenBucketConfig,
        VirtioRngRetryCaptureState,
    };
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::virtio_pci::VIRTIO_PCI_CAPABILITY_BAR_SIZE;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("capture-ready-entropy-kernel", &image)
        .expect("entropy capture kernel should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("entropy capture boot source should configure");
    let rate_limiter = EntropyRateLimiterConfig::new(
        Some(EntropyTokenBucketConfig::new(4, None, 60_000)),
        Some(EntropyTokenBucketConfig::new(1, None, 60_000)),
    );
    controller
        .handle_action(VmmAction::PutEntropy(
            EntropyConfigInput::new().with_rate_limiter(rate_limiter),
        ))
        .expect("entropy capture device should configure");
    let entropy_config = controller
        .entropy_config()
        .expect("entropy capture config should exist");
    let entropy_device = HvfArm64BootEntropyDeviceConfig::new(EntropyMmioLayout::new(
        GuestAddress::new(0x4000_7000),
        MmioRegionId::new(3001),
    ));
    let base_session_config = || {
        HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            test_rtc_mmio_layout(),
        )
        .with_entropy_device(entropy_device)
    };

    let mut mmio_session = OwnedHvfArm64BootSession::new(&controller, base_session_config())
        .expect("signed MMIO entropy session should prepare");
    let mmio_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO entropy retry publisher should quiesce");
    let now = Instant::now();
    let mmio_first = mmio_session
        .capture_ready_entropy_state_at(Some(entropy_config), &mmio_guard, now)
        .expect("signed MMIO entropy should become capture-ready")
        .expect("configured MMIO entropy should be captured");
    let mmio_second = mmio_session
        .capture_ready_entropy_state_at(Some(entropy_config), &mmio_guard, now)
        .expect("signed MMIO entropy should support repeated detached capture")
        .expect("configured MMIO entropy should remain captured");
    assert_eq!(mmio_first, mmio_second);
    assert_eq!(mmio_first.config(), entropy_config);
    assert_eq!(mmio_first.retry(), VirtioRngRetryCaptureState::None);
    let HvfArm64BootEntropyTransportState::Mmio {
        region,
        interrupt_line,
        state,
    } = mmio_first.transport()
    else {
        panic!("MMIO entropy should retain MMIO ownership");
    };
    let mmio_base = region.range().start();
    assert_eq!(mmio_base, GuestAddress::new(0x4000_7000));
    assert!(interrupt_line.raw_value() > 0);
    assert_eq!(state.device().config(), entropy_config);
    assert!(state.device().active_queue().is_none());
    assert!(state.device().rate_limiter().bandwidth().is_some());
    assert!(state.device().rate_limiter().ops().is_some());
    assert!(!state.transport().is_device_activated());
    assert!(!format!("{mmio_first:?}").contains("40007000"));
    assert!(matches!(
        mmio_session.capture_ready_entropy_state_at(None, &mmio_guard, now),
        Err(HvfArm64BootEntropyCaptureError::OwnershipMismatch {
            configured: false,
            mmio_owner: true,
            pci_owner: false,
        })
    ));
    drop(mmio_guard);

    let mmio_retry_after =
        activate_and_notify_entropy_capture_queue(&mut mmio_session, mmio_base, false);
    assert!(mmio_retry_after > std::time::Duration::ZERO);
    assert!(mmio_retry_after <= std::time::Duration::from_secs(60));
    let mmio_pending_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO pending entropy retry publisher should quiesce");
    let mmio_pending_now = Instant::now();
    let mmio_pending = mmio_session
        .capture_ready_entropy_state_at(Some(entropy_config), &mmio_pending_guard, mmio_pending_now)
        .expect("signed MMIO pending entropy should become capture-ready")
        .expect("configured MMIO pending entropy should be captured");
    let mmio_pending_again = mmio_session
        .capture_ready_entropy_state_at(Some(entropy_config), &mmio_pending_guard, mmio_pending_now)
        .expect("signed MMIO pending entropy capture should repeat")
        .expect("configured MMIO pending entropy should remain captured");
    assert_eq!(mmio_pending, mmio_pending_again);
    assert!(matches!(
        mmio_pending.retry(),
        VirtioRngRetryCaptureState::After { remaining_nanos }
            if remaining_nanos > 0 && remaining_nanos <= 60_000_000_000
    ));
    let mmio_pending_device = mmio_pending
        .transport()
        .mmio_state()
        .expect("pending MMIO capture should retain MMIO transport")
        .device();
    assert!(mmio_pending_device.has_pending_rate_limited_queue());
    assert_eq!(
        mmio_pending_device
            .active_queue()
            .map(|queue| (queue.next_available(), queue.next_used())),
        Some((1, 1))
    );
    drop(mmio_pending_guard);
    mmio_session
        .shutdown()
        .expect("signed MMIO entropy session should shut down");

    let mut pci_session =
        OwnedHvfArm64BootSession::new(&controller, base_session_config().with_pci_enabled())
            .expect("signed PCI entropy session should prepare");
    let pci_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI entropy retry publisher should quiesce");
    let pci = pci_session
        .capture_ready_entropy_state_at(Some(entropy_config), &pci_guard, Instant::now())
        .expect("signed PCI entropy should become capture-ready")
        .expect("configured PCI entropy should be captured");
    assert_eq!(pci.config(), entropy_config);
    assert_eq!(pci.retry(), VirtioRngRetryCaptureState::None);
    let HvfArm64BootEntropyTransportState::Pci {
        sbdf,
        bar_range,
        state,
    } = pci.transport()
    else {
        panic!("PCI entropy should retain PCI ownership");
    };
    let pci_bar_base = bar_range.start();
    assert!(sbdf.device() > 0);
    assert_eq!(bar_range.size(), VIRTIO_PCI_CAPABILITY_BAR_SIZE);
    assert_eq!(state.device().config(), entropy_config);
    assert!(state.device().active_queue().is_none());
    assert!(state.device().rate_limiter().bandwidth().is_some());
    assert!(state.device().rate_limiter().ops().is_some());
    assert!(!state.transport().is_device_activated());
    assert!(!format!("{pci:?}").contains("40007000"));
    drop(pci_guard);

    let pci_retry_after =
        activate_and_notify_entropy_capture_queue(&mut pci_session, pci_bar_base, true);
    assert!(pci_retry_after > std::time::Duration::ZERO);
    assert!(pci_retry_after <= std::time::Duration::from_secs(60));
    let pci_pending_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI pending entropy retry publisher should quiesce");
    let pci_pending_now = Instant::now();
    let pci_pending = pci_session
        .capture_ready_entropy_state_at(Some(entropy_config), &pci_pending_guard, pci_pending_now)
        .expect("signed PCI pending entropy should become capture-ready")
        .expect("configured PCI pending entropy should be captured");
    let pci_pending_again = pci_session
        .capture_ready_entropy_state_at(Some(entropy_config), &pci_pending_guard, pci_pending_now)
        .expect("signed PCI pending entropy capture should repeat")
        .expect("configured PCI pending entropy should remain captured");
    assert_eq!(pci_pending, pci_pending_again);
    assert!(matches!(
        pci_pending.retry(),
        VirtioRngRetryCaptureState::After { remaining_nanos }
            if remaining_nanos > 0 && remaining_nanos <= 60_000_000_000
    ));
    let pci_pending_device = pci_pending
        .transport()
        .pci_state()
        .expect("pending PCI capture should retain PCI transport")
        .device();
    assert!(pci_pending_device.has_pending_rate_limited_queue());
    assert_eq!(
        pci_pending_device
            .active_queue()
            .map(|queue| (queue.next_available(), queue.next_used())),
        Some((1, 1))
    );
    drop(pci_pending_guard);
    pci_session
        .shutdown()
        .expect("signed PCI entropy session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn capture_ready_memory_hotplug_traverses_signed_mmio_and_pci_owners() {
    use bangbang_hvf::{
        HvfArm64BootMemoryHotplugCaptureError, HvfArm64BootMemoryHotplugDeviceConfig,
        HvfArm64BootMemoryHotplugTransportState, HvfArm64BootSessionConfig,
        OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::memory_hotplug::{MemoryHotplugConfigInput, VirtioMemMmioLayout};
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::virtio_pci::VIRTIO_PCI_CAPABILITY_BAR_SIZE;
    use bangbang_runtime::vsock::VsockMmioLayout;

    const MIB: u64 = 1024 * 1024;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("capture-ready-memory-hotplug-kernel", &image)
        .expect("memory-hotplug capture kernel should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("memory-hotplug capture boot source should configure");
    controller
        .handle_action(VmmAction::PutMemoryHotplug(MemoryHotplugConfigInput::new(
            128, 2, 128,
        )))
        .expect("memory-hotplug capture device should configure");
    let memory_hotplug_config = controller
        .memory_hotplug_config()
        .expect("memory-hotplug config should exist");
    let memory_hotplug_device = HvfArm64BootMemoryHotplugDeviceConfig::new(
        VirtioMemMmioLayout::new(GuestAddress::new(0x4000_8000), MmioRegionId::new(4001)),
    );
    let base_session_config = || {
        HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            test_rtc_mmio_layout(),
        )
        .with_memory_hotplug_device(memory_hotplug_device)
    };

    let mut mmio_session = OwnedHvfArm64BootSession::new(&controller, base_session_config())
        .expect("signed MMIO memory-hotplug session should prepare");
    let mmio_metrics = mmio_session
        .shared_memory_hotplug_device_metrics()
        .expect("MMIO memory-hotplug metrics should be retained");
    let mmio_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO auxiliary publishers should quiesce");
    let mmio_first = mmio_session
        .capture_ready_memory_hotplug_state(Some(memory_hotplug_config), &mmio_guard)
        .expect("signed MMIO memory-hotplug should become capture-ready")
        .expect("configured MMIO memory-hotplug should be captured");
    let mmio_second = mmio_session
        .capture_ready_memory_hotplug_state(Some(memory_hotplug_config), &mmio_guard)
        .expect("signed MMIO memory-hotplug should support repeated detached capture")
        .expect("configured MMIO memory-hotplug should remain captured");
    assert_eq!(mmio_first.config(), memory_hotplug_config);
    let HvfArm64BootMemoryHotplugTransportState::Mmio { state, .. } = mmio_first.transport() else {
        panic!("MMIO memory-hotplug should retain MMIO ownership");
    };
    assert!(state.device().active_queue().is_none());
    assert!(!state.transport().is_device_activated());
    assert_eq!(mmio_first.mapping().active_ranges(), []);
    assert_eq!(mmio_first.mapping().active_bytes(), 0);
    assert_eq!(mmio_first.mapping().offline_bytes(), 128 * MIB);
    assert_eq!(mmio_first.mapping().reservation().range().size(), 128 * MIB);
    assert_eq!(
        mmio_first.mapping().mapping_identity(),
        mmio_first.mapping().reservation().mapping_identity()
    );
    assert_eq!(mmio_first, mmio_second);
    assert!(!format!("{mmio_first:?}").contains("40008000"));
    assert!(matches!(
        mmio_session.capture_ready_memory_hotplug_state(None, &mmio_guard),
        Err(HvfArm64BootMemoryHotplugCaptureError::OwnershipMismatch {
            configured: false,
            mmio_owner: true,
            pci_owner: false,
        })
    ));
    drop(mmio_guard);
    mmio_session
        .shutdown()
        .expect("signed MMIO memory-hotplug session should shut down");
    assert_eq!(mmio_metrics.snapshot().teardown_count(), 1);
    assert_eq!(mmio_metrics.snapshot().teardown_fails(), 0);

    let mut pci_session =
        OwnedHvfArm64BootSession::new(&controller, base_session_config().with_pci_enabled())
            .expect("signed PCI memory-hotplug session should prepare");
    let pci_metrics = pci_session
        .shared_memory_hotplug_device_metrics()
        .expect("PCI memory-hotplug metrics should be retained");
    let pci_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI auxiliary publishers should quiesce");
    let pci = pci_session
        .capture_ready_memory_hotplug_state(Some(memory_hotplug_config), &pci_guard)
        .expect("signed PCI memory-hotplug should become capture-ready")
        .expect("configured PCI memory-hotplug should be captured");
    let HvfArm64BootMemoryHotplugTransportState::Pci {
        sbdf,
        bar_range,
        state,
    } = pci.transport()
    else {
        panic!("PCI memory-hotplug should retain PCI ownership");
    };
    assert!(sbdf.device() > 0);
    assert_eq!(bar_range.size(), VIRTIO_PCI_CAPABILITY_BAR_SIZE);
    assert!(state.device().active_queue().is_none());
    assert!(!state.transport().is_device_activated());
    assert_eq!(pci.mapping().active_ranges(), []);
    assert_eq!(pci.mapping().active_bytes(), 0);
    assert_eq!(pci.mapping().offline_bytes(), 128 * MIB);
    assert_eq!(pci.mapping().reservation().range().size(), 128 * MIB);
    assert_eq!(
        pci.mapping().mapping_identity(),
        pci.mapping().reservation().mapping_identity()
    );
    assert!(!format!("{pci:?}").contains("40008000"));
    drop(pci_guard);
    pci_session
        .shutdown()
        .expect("signed PCI memory-hotplug session should shut down");
    assert_eq!(pci_metrics.snapshot().teardown_count(), 1);
    assert_eq!(pci_metrics.snapshot().teardown_fails(), 0);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn guest_write_to_writable_pmem_is_visible_before_any_pmem_flush() {
    use std::os::unix::fs::FileExt;

    use bangbang_hvf::{HvfArm64BootSessionConfig, HvfBackend, HvfVcpuRunStepOutcome};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::{PmemConfigInput, PmemMmioLayout, VIRTIO_PMEM_ALIGNMENT};
    use bangbang_runtime::vsock::VsockMmioLayout;

    const WRITE_OFFSET: u64 = 4096;
    const WRITE_VALUE: u32 = 0x5a6b_7c8d;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("direct-pmem-kernel", &image)
        .expect("temp direct-pmem kernel should be created");
    let pmem = TempFile::new_len("direct-pmem-backing", VIRTIO_PMEM_ALIGNMENT)
        .expect("temp direct-pmem backing should be created");
    let observer =
        std::fs::File::open(pmem.path()).expect("independent direct-pmem observer should open");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("direct-pmem boot source should configure");
    controller
        .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
            "pmem0",
            path_text(pmem.path()),
        )))
        .expect("direct-pmem device should configure");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut backend = HvfBackend::new();
    let mut session = backend
        .prepare_arm64_boot_session(&controller, config)
        .expect("direct-pmem session should prepare");
    let boot_registers = session
        .boot_registers()
        .expect("direct-pmem session should retain boot registers");
    let target = session.runtime_resources().pmem_devices[0]
        .guest_range()
        .start()
        .checked_add(WRITE_OFFSET)
        .expect("direct-pmem guest target should fit");
    let program = arm64_store_u32_and_hvc_program(target.raw_value(), WRITE_VALUE);
    session
        .guest_memory_mut()
        .expect("direct-pmem session should expose ordinary guest memory")
        .write_slice(&program, boot_registers.kernel_entry)
        .expect("direct-pmem guest program should replace the test kernel entry");

    let mut before = [0_u8; std::mem::size_of::<u32>()];
    observer
        .read_exact_at(&mut before, WRITE_OFFSET)
        .expect("direct-pmem observer should read before the guest write");
    assert_eq!(before, [0; std::mem::size_of::<u32>()]);

    assert!(matches!(
        session
            .run_once_and_handle_mmio()
            .expect("direct-pmem guest should reach HVC without a mapping exit"),
        HvfVcpuRunStepOutcome::Hvc { exit, .. } if exit.immediate() == 0
    ));

    let mut observed = [0_u8; std::mem::size_of::<u32>()];
    observer
        .read_exact_at(&mut observed, WRITE_OFFSET)
        .expect("independent observer should read the live file mapping");
    assert_eq!(
        u32::from_le_bytes(observed),
        WRITE_VALUE,
        "guest writes must be visible through the backing descriptor before a virtio-pmem or teardown flush"
    );

    session
        .shutdown()
        .expect("direct-pmem session should shut down after the pre-flush observation");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn direct_pmem_mapping_has_bounded_process_memory_growth() {
    use bangbang_hvf::{HvfArm64BootSessionConfig, HvfBackend};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::{PmemConfig, PmemConfigInput, PmemFileBacking, PmemMmioLayout};
    use bangbang_runtime::vsock::VsockMmioLayout;

    const PMEM_LEN: u64 = 64 * 1024 * 1024;
    const VIRTUAL_SIZE_SLACK: u64 = 16 * 1024 * 1024;
    const RESIDENT_SIZE_SLACK: u64 = 32 * 1024 * 1024;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("bounded-pmem-kernel", &image)
        .expect("bounded-pmem kernel should be created");
    let pmem = TempFile::new_len("bounded-direct-pmem", PMEM_LEN)
        .expect("bounded direct-pmem backing should be created");
    let pmem_config = PmemConfig::try_from(PmemConfigInput::new("pmem0", path_text(pmem.path())))
        .expect("bounded direct-pmem config should validate");
    let backing =
        PmemFileBacking::open(&pmem_config).expect("bounded direct-pmem backing should open");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("direct-pmem boot source should configure");
    let session_config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    )
    .with_pci_enabled();
    let mut backend = HvfBackend::new();
    let mut session = backend
        .prepare_arm64_boot_session(&controller, session_config)
        .expect("direct-pmem session should prepare");
    let before = process_memory_usage().expect("pre-insert process memory usage should read");
    session
        .insert_runtime_pmem_device(&pmem_config, backing)
        .expect("bounded direct-pmem device should map and publish");
    let after = process_memory_usage().expect("post-insert process memory usage should read");
    let growth = after.saturating_growth_from(before);

    assert!(
        growth.virtual_size <= PMEM_LEN + VIRTUAL_SIZE_SLACK,
        "one {PMEM_LEN}-byte direct pmem insertion must not add a second full-size virtual mapping; before {before:?}, after {after:?}, growth {growth:?}"
    );
    assert!(
        growth.resident_size <= PMEM_LEN + RESIDENT_SIZE_SLACK,
        "direct pmem insertion must keep resident growth within one backing plus generous framework slack; before {before:?}, after {after:?}, growth {growth:?}"
    );

    session
        .remove_runtime_pmem_device("pmem0")
        .expect("bounded direct-pmem device should flush and unmap");
    session
        .shutdown()
        .expect("bounded direct-pmem session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn guest_write_to_read_only_pmem_faults_without_mutating_backing() {
    use std::os::unix::fs::FileExt;

    use bangbang_hvf::{
        HvfArm64BootSessionConfig, HvfBackend, HvfVcpuExitResolveError, HvfVcpuRunnerError,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::{PmemConfigInput, PmemMmioLayout, VIRTIO_PMEM_ALIGNMENT};
    use bangbang_runtime::vsock::VsockMmioLayout;

    const WRITE_OFFSET: u64 = 4096;
    const WRITE_VALUE: u32 = 0xa5b6_c7d8;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("read-only-pmem-kernel", &image)
        .expect("temp read-only-pmem kernel should be created");
    let pmem = TempFile::new_len("read-only-pmem-backing", VIRTIO_PMEM_ALIGNMENT)
        .expect("temp read-only-pmem backing should be created");
    let observer =
        std::fs::File::open(pmem.path()).expect("independent read-only-pmem observer should open");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("read-only-pmem boot source should configure");
    controller
        .handle_action(VmmAction::PutPmem(
            PmemConfigInput::new("pmem0", path_text(pmem.path())).with_read_only(true),
        ))
        .expect("read-only-pmem device should configure");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut backend = HvfBackend::new();
    let mut session = backend
        .prepare_arm64_boot_session(&controller, config)
        .expect("read-only-pmem session should prepare");
    let boot_registers = session
        .boot_registers()
        .expect("read-only-pmem session should retain boot registers");
    let target = session.runtime_resources().pmem_devices[0]
        .guest_range()
        .start()
        .checked_add(WRITE_OFFSET)
        .expect("read-only-pmem guest target should fit");
    let program = arm64_store_u32_and_hvc_program(target.raw_value(), WRITE_VALUE);
    session
        .guest_memory_mut()
        .expect("read-only-pmem session should expose ordinary guest memory")
        .write_slice(&program, boot_registers.kernel_entry)
        .expect("read-only-pmem guest program should replace the test kernel entry");

    let err = session
        .run_once_and_handle_mmio()
        .expect_err("guest write to read-only pmem should fault before HVC");
    assert!(
        matches!(
            err,
            HvfVcpuRunnerError::VcpuExitResolve(HvfVcpuExitResolveError::MmioResolve { .. })
        ),
        "read-only pmem write should surface as an unowned write fault, got {err:?}"
    );

    let mut observed = [0_u8; std::mem::size_of::<u32>()];
    observer
        .read_exact_at(&mut observed, WRITE_OFFSET)
        .expect("independent observer should read after the rejected guest write");
    assert_eq!(
        observed,
        [0; std::mem::size_of::<u32>()],
        "a guest write fault must not mutate the read-only pmem backing"
    );

    session
        .shutdown()
        .expect("read-only-pmem session should shut down after the fault proof");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn prepares_owned_hvf_arm64_boot_session() {
    use bangbang_hvf::{
        ARM64_LINUX_BOOT_CPSR, HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel =
        TempFile::new("owned-session-kernel", &image).expect("temp kernel should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    controller
        .handle_action(VmmAction::PutMachineConfig(
            MachineConfigInput::new(1, 128).with_track_dirty_pages(true),
        ))
        .expect("tracked normal-boot machine config should be stored");
    let rtc_mmio_layout = test_rtc_mmio_layout();
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        rtc_mmio_layout,
    );

    let mut session = OwnedHvfArm64BootSession::new(&controller, config.clone())
        .expect("owned HVF arm64 boot session should prepare");

    let mmio_dispatcher = session.mmio_dispatcher();
    let mmio_regions = mmio_dispatcher
        .try_lock()
        .expect("owned session MMIO dispatcher should lock")
        .regions()
        .to_vec();
    assert_eq!(mmio_regions.len(), 1);
    assert_eq!(mmio_regions[0].id(), rtc_mmio_layout.region_id());
    assert_eq!(mmio_regions[0].range().start(), rtc_mmio_layout.base());
    assert_eq!(
        mmio_regions[0].range().size(),
        bangbang_runtime::rtc::RTC_MMIO_DEVICE_WINDOW_SIZE
    );
    assert!(session.block_interrupt_lines().is_empty());
    assert_eq!(
        session
            .guest_memory()
            .expect("owned session should expose mapped guest memory")
            .total_size(),
        session.runtime_resources().layout.total_size()
    );
    let dirty_tracker = session
        .guest_memory()
        .expect("tracked owned session should expose guest memory")
        .dirty_tracker()
        .expect("normal tracked startup should retain one dirty epoch");
    assert!(
        !dirty_tracker
            .dirty_pages()
            .expect("normal boot dirty pages should query")
            .is_empty(),
        "kernel, FDT, and device boot population must enter the initial epoch"
    );
    assert_eq!(session.reset_dirty_epoch_quiesced(), Ok(Some(1)));
    assert!(
        dirty_tracker
            .dirty_pages()
            .expect("reset normal-boot epoch should query")
            .is_empty()
    );
    let boot_origin = session
        .runtime_resources()
        .boot_origin
        .as_ref()
        .expect("ordinary session should retain boot-origin metadata");
    let boot_registers = session
        .boot_registers()
        .expect("ordinary session should retain boot registers");
    let mut fdt_magic = [0; 4];
    session
        .guest_memory()
        .expect("owned session should expose mapped guest memory")
        .read_slice(&mut fdt_magic, boot_origin.fdt.address)
        .expect("mapped guest memory should contain the written FDT");
    assert_eq!(u32::from_be_bytes(fdt_magic), 0xd00d_feed);
    assert_eq!(
        boot_registers.kernel_entry,
        boot_origin.loaded_boot_source.kernel.entry_address
    );
    assert_eq!(boot_registers.fdt_address, boot_origin.fdt.address);
    let register_state = session
        .capture_arm64_general_register_state()
        .expect("owned session should capture general-register state");
    assert_eq!(
        register_state.general_purpose_register(0),
        Some(boot_registers.fdt_address.raw_value())
    );
    assert_eq!(register_state.pc(), boot_registers.kernel_entry.raw_value());
    assert_eq!(register_state.cpsr(), ARM64_LINUX_BOOT_CPSR);
    session
        .restore_arm64_general_register_state(&register_state)
        .expect("owned session should restore general-register state");
    let core_system_register_state = session
        .capture_arm64_core_system_register_state()
        .expect("owned session should capture core system-register state");
    session
        .restore_arm64_core_system_register_state(&core_system_register_state)
        .expect("owned session should restore core system-register state");
    let exception_register_state = session
        .capture_arm64_exception_register_state()
        .expect("owned session should capture exception-register state");
    session
        .restore_arm64_exception_register_state(&exception_register_state)
        .expect("owned session should restore exception-register state");
    let execution_control_state = session
        .capture_arm64_execution_control_register_state()
        .expect("owned session should capture execution-control state");
    session
        .restore_arm64_execution_control_register_state(&execution_control_state)
        .expect("owned session should restore execution-control state");
    let cache_selection_state = session
        .capture_arm64_cache_selection_register_state()
        .expect("owned session should capture cache-selection state");
    session
        .restore_arm64_cache_selection_register_state(&cache_selection_state)
        .expect("owned session should restore cache-selection state");
    session
        .capture_arm64_breakpoint_register_state()
        .expect("owned session should capture breakpoint-register state");
    session
        .capture_arm64_watchpoint_register_state()
        .expect("owned session should capture watchpoint-register state");
    let debug_control_state = session
        .capture_arm64_debug_control_register_state()
        .expect("owned session should capture debug-control state");
    session
        .restore_arm64_debug_control_register_state(&debug_control_state)
        .expect("owned session should restore debug-control state");
    let debug_trap_state = session
        .capture_arm64_debug_trap_state()
        .expect("owned session should capture debug-trap state");
    session
        .restore_arm64_debug_trap_state(&debug_trap_state)
        .expect("owned session should restore debug-trap state");
    session
        .capture_arm64_identification_register_state()
        .expect("owned session should capture identification-register state");
    session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("owned session should capture SVE/SME identification state");
    let _sme_pstate =
        assert_sme_pstate_capture_supported_or_unavailable(session.capture_arm64_sme_pstate())
            .expect("owned session SME PSTATE capture should succeed or report unsupported");
    let _sme_p_registers = assert_sme_p_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_p_register_state(),
    )
    .expect("owned session SME P-register capture should succeed or report unavailable");
    let _sme_z_registers = assert_sme_z_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_z_register_state(),
    )
    .expect("owned session SME Z-register capture should succeed or report unavailable");
    let _sme_za_register = assert_sme_za_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_za_register_state(),
    )
    .expect("owned session SME ZA-register capture should succeed or report unavailable");
    let _sme_zt0_register = assert_sme_zt0_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_zt0_register_state(),
    )
    .expect("owned session SME ZT0-register capture should succeed or report unavailable");
    session
        .capture_arm64_sme_system_register_state()
        .expect("owned session should capture SME system-register state");
    let system_context_state = session
        .capture_arm64_system_context_register_state()
        .expect("owned session should capture system-context register state");
    session
        .restore_arm64_system_context_register_state(&system_context_state)
        .expect("owned session should restore system-context register state");
    let translation_state = session
        .capture_arm64_translation_register_state()
        .expect("owned session should capture translation-register state");
    session
        .restore_arm64_translation_register_state(&translation_state)
        .expect("owned session should restore translation-register state");
    let pointer_authentication_key_state = session
        .capture_arm64_pointer_authentication_key_state()
        .expect("owned session should capture pointer-authentication key state");
    session
        .restore_arm64_pointer_authentication_key_state(&pointer_authentication_key_state)
        .expect("owned session should restore pointer-authentication key state");
    let thread_context_state = session
        .capture_arm64_thread_context_register_state()
        .expect("owned session should capture thread-context register state");
    session
        .restore_arm64_thread_context_register_state(&thread_context_state)
        .expect("owned session should restore thread-context register state");
    let simd_fp_state = session
        .capture_arm64_simd_fp_state()
        .expect("owned session should capture SIMD/FP state");
    session
        .restore_arm64_simd_fp_state(&simd_fp_state)
        .expect("owned session should restore SIMD/FP state");
    session
        .capture_arm64_physical_timer_state()
        .expect("owned session should capture physical-timer state");
    session
        .capture_arm64_virtual_timer_state()
        .expect("owned session should capture virtual-timer state");
    let snapshot_timer_state = session
        .capture_arm64_snapshot_timer_state()
        .expect("owned session should capture normalized timer state");
    let pending_interrupt_state = session
        .capture_arm64_pending_interrupt_state()
        .expect("owned session should capture pending-interrupt state");
    session
        .restore_arm64_pending_interrupt_state(&pending_interrupt_state)
        .expect("owned session should restore pending-interrupt state");
    let gic_device_state = session
        .capture_gic_device_state()
        .expect("owned session should capture GIC device state");
    assert!(!gic_device_state.is_empty());
    let gic_icc_register_state = session
        .capture_arm64_gic_icc_register_state()
        .expect("owned session should capture GIC ICC register state");
    session
        .restore_gic_device_state(&gic_device_state)
        .expect("owned session should restore GIC device state before run");
    session
        .restore_arm64_gic_icc_register_state(&gic_icc_register_state)
        .expect("owned session should restore GIC ICC register state before run");
    let restored_gic_icc_register_state = session
        .capture_arm64_gic_icc_register_state()
        .expect("owned session should capture GIC ICC register state");
    assert!(
        restored_gic_icc_register_state == gic_icc_register_state,
        "owned session should preserve original GIC ICC register state"
    );
    session
        .restore_arm64_snapshot_timer_state(snapshot_timer_state)
        .expect("owned session should restore normalized timers after GIC state");
    assert_normalized_timer_restore_equivalent(
        snapshot_timer_state,
        session
            .capture_arm64_snapshot_timer_state()
            .expect("owned session should recapture normalized timers"),
    );
    let old_vmgenid = session.runtime_resources().vmgenid_device;
    session
        .replace_vmgenid_for_snapshot_restore()
        .expect("owned session should replace VMGenID and inject its SPI");
    let new_vmgenid = session.runtime_resources().vmgenid_device;
    assert_ne!(new_vmgenid.generation_id, old_vmgenid.generation_id);
    assert_eq!(new_vmgenid.range, old_vmgenid.range);
    assert_eq!(new_vmgenid.fdt_device, old_vmgenid.fdt_device);
    let mut guest_vmgenid = [0; bangbang_runtime::startup::ARM64_BOOT_VMGENID_SIZE];
    session
        .guest_memory()
        .expect("owned session should expose VMGenID guest memory")
        .read_slice(&mut guest_vmgenid, new_vmgenid.range.start())
        .expect("owned session replacement VMGenID should read");
    assert_eq!(guest_vmgenid, new_vmgenid.generation_id);
    let page_size = host_page_size().expect("host page size should remain available");
    let vmgenid_page = GuestAddress::new(new_vmgenid.range.start().raw_value() & !(page_size - 1));
    assert_eq!(
        dirty_tracker
            .dirty_pages()
            .expect("VMGenID device dirty page should query"),
        vec![vmgenid_page]
    );
    assert_eq!(session.reset_dirty_epoch_quiesced(), Ok(Some(2)));
    let run_cancel_handle = session.run_cancel_handle();
    drop(run_cancel_handle);
    let run_loop_control = session.run_loop_control();
    let run_loop_stop_token = run_loop_control.stop_token();
    run_loop_control
        .request_stop()
        .expect("owned HVF boot-session run-loop stop should request vCPU cancellation");
    assert!(run_loop_stop_token.is_stop_requested());
    session
        .shutdown()
        .expect("owned HVF arm64 boot session should shut down");
    session
        .shutdown()
        .expect("repeated owned HVF arm64 boot session shutdown should be idempotent");
    drop(session);

    let mut second_session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("second owned HVF arm64 boot session should prepare after shutdown");
    assert_eq!(
        second_session
            .guest_memory_mut()
            .expect("second owned session should expose mutable mapped guest memory")
            .total_size(),
        second_session.runtime_resources().layout.total_size()
    );
    second_session
        .shutdown()
        .expect("second owned HVF arm64 boot session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn psci_cpu_suspend_retains_context_until_two_virtual_timer_wakeups() {
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use bangbang_hvf::{
        HvfArm64BootRunLoopOutcome, HvfArm64BootSessionConfig, HvfVcpuRunStepOutcome,
        OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    const SECONDARY_OFFSET: u64 = 0x1000;
    const FLAGS_OFFSET: u64 = 0x4000;
    const FLAGS_SIZE: usize = 0x48;
    const CPU_ON_RESULT: usize = 0x00;
    const AFFINITY_RESULT: usize = 0x08;
    const PRE_SUSPEND_1: usize = 0x10;
    const POST_SUSPEND_1: usize = 0x14;
    const SUSPEND_RESULT_1: usize = 0x18;
    const SENTINEL_1: usize = 0x20;
    const PRE_SUSPEND_2: usize = 0x28;
    const POST_SUSPEND_2: usize = 0x2c;
    const SUSPEND_RESULT_2: usize = 0x30;
    const SENTINEL_2: usize = 0x38;
    const PEER_OBSERVATION: usize = 0x40;
    const PSCI_CPU_SUSPEND_64: u64 = 0xc400_0001;
    const PSCI_VERSION: u64 = 0x8400_0000;
    const SENTINEL: u64 = 0x5a5a;

    // CPU0 starts CPU1, waits for CPU1's pre-suspend publication, observes
    // AFFINITY_INFO, and emits PSCI_VERSION as an event-driven host checkpoint.
    let primary_code = [
        0x1002_0013, // adr x19, flags (+0x4000)
        0xd280_0060, // mov x0, #3
        0xf2b8_8000, // movk x0, #0xc400, lsl #16 (CPU_ON64)
        0xd280_0021, // mov x1, #1
        0x1000_7f82, // adr x2, secondary (+0x1000)
        0x1001_ff63, // adr x3, flags (+0x4000)
        0xd400_0002, // hvc #0
        0xf900_0260, // str x0, [x19]
        0xb940_1264, // ldr w4, [x19, #0x10]
        0x34ff_ffe4, // cbz w4, previous instruction
        0xd280_0080, // mov x0, #4
        0xf2b8_8000, // movk x0, #0xc400, lsl #16 (AFFINITY_INFO64)
        0xd280_0021, // mov x1, #1
        0xd280_0002, // mov x2, #0
        0xd400_0002, // hvc #0
        0xf900_0660, // str x0, [x19, #8]
        0x5280_0024, // mov w4, #1
        0xb900_4264, // str w4, [x19, #0x40]
        0xd280_0000, // mov x0, #0
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (PSCI_VERSION)
        0xd400_0002, // hvc #0
        0x1400_0000, // b .
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect::<Vec<_>>();

    // CPU1 uses one counter-frequency interval per retained wait, preserves
    // x20 across both calls, and terminates the guest only after both returns.
    let secondary_code = [
        0xaa00_03f3, // mov x19, x0
        0xd28b_4b54, // mov x20, #0x5a5a
        0xd53b_e044, // mrs x4, CNTVCT_EL0
        0xd53b_e005, // mrs x5, CNTFRQ_EL0
        0x8b05_0084, // add x4, x4, x5
        0xd51b_e344, // msr CNTV_CVAL_EL0, x4
        0xd280_0024, // mov x4, #1
        0xd51b_e324, // msr CNTV_CTL_EL0, x4
        0xd503_3fdf, // isb
        0x5280_0026, // mov w6, #1
        0xb900_1266, // str w6, [x19, #0x10]
        0xd280_0020, // mov x0, #1
        0xf2b8_8000, // movk x0, #0xc400, lsl #16 (CPU_SUSPEND64)
        0xd295_5541, // mov x1, #0xaaaa (ignored)
        0xd282_4682, // mov x2, #0x1234 (ignored)
        0xd297_dde3, // mov x3, #0xbeef (ignored)
        0xd400_0002, // hvc #0
        0xf900_0e60, // str x0, [x19, #0x18]
        0xf900_1274, // str x20, [x19, #0x20]
        0xb900_1666, // str w6, [x19, #0x14]
        0xd53b_e044, // mrs x4, CNTVCT_EL0
        0xd53b_e005, // mrs x5, CNTFRQ_EL0
        0x8b05_0084, // add x4, x4, x5
        0xd51b_e344, // msr CNTV_CVAL_EL0, x4
        0xd280_0024, // mov x4, #1
        0xd51b_e324, // msr CNTV_CTL_EL0, x4
        0xd503_3fdf, // isb
        0xb900_2a66, // str w6, [x19, #0x28]
        0xd280_0020, // mov x0, #1
        0xf2b8_8000, // movk x0, #0xc400, lsl #16 (CPU_SUSPEND64)
        0xd297_7761, // mov x1, #0xbbbb (ignored)
        0xd28a_cf02, // mov x2, #0x5678 (ignored)
        0xd299_5fc3, // mov x3, #0xcafe (ignored)
        0xd400_0002, // hvc #0
        0xf900_1a60, // str x0, [x19, #0x30]
        0xf900_1e74, // str x20, [x19, #0x38]
        0xb900_2e66, // str w6, [x19, #0x2c]
        0xd280_0100, // mov x0, #8
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (SYSTEM_OFF)
        0xd400_0002, // hvc #0
        0x1400_0000, // b .
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect::<Vec<_>>();

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel =
        TempFile::new("psci-cpu-suspend-kernel", &image).expect("temp kernel should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    controller
        .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 16)))
        .expect("two-vCPU machine should configure");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("two-vCPU CPU_SUSPEND session should prepare");
    let primary_entry = GuestAddress::new(
        session
            .capture_arm64_general_register_state()
            .expect("primary entry registers should capture")
            .pc(),
    );
    let secondary_entry = primary_entry
        .checked_add(SECONDARY_OFFSET)
        .expect("secondary entry should fit");
    let flags = primary_entry
        .checked_add(FLAGS_OFFSET)
        .expect("shared flags should fit");
    {
        let memory = session
            .guest_memory_mut()
            .expect("guest memory should be mutable before execution");
        memory
            .write_slice(&primary_code, primary_entry)
            .expect("primary guest code should fit");
        memory
            .write_slice(&secondary_code, secondary_entry)
            .expect("secondary guest code should fit");
        memory
            .write_slice(&[0; FLAGS_SIZE], flags)
            .expect("shared guest flags should fit");
    }
    let flags_host = {
        let memory = session
            .guest_memory()
            .expect("mapped guest memory should remain available");
        let region = memory
            .regions()
            .iter()
            .find(|region| region.range().contains(flags))
            .expect("shared flags should belong to mapped DRAM");
        let offset = flags
            .raw_value()
            .checked_sub(region.range().start().raw_value())
            .and_then(|offset| usize::try_from(offset).ok())
            .expect("shared flag host offset should fit");
        region.host_address().as_ptr().cast::<u8>() as usize + offset
    };
    let read_u32 = |offset: usize| {
        // SAFETY: each aligned address remains inside the mapped shared flag
        // area for the session lifetime; volatile reads observe guest stores.
        unsafe { std::ptr::read_volatile((flags_host + offset) as *const u32) }
    };
    let read_u64 = |offset: usize| {
        // SAFETY: each aligned address remains inside the mapped shared flag
        // area for the session lifetime; volatile reads observe guest stores.
        unsafe { std::ptr::read_volatile((flags_host + offset) as *const u64) }
    };

    let control = session.run_loop_control();
    let stop_token = control.stop_token();
    let watchdog_done = Arc::new(AtomicBool::new(false));
    let watchdog_done_for_thread = Arc::clone(&watchdog_done);
    let watchdog = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !watchdog_done_for_thread.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        if !watchdog_done_for_thread.load(Ordering::Acquire) {
            let _ = control.request_stop();
        }
    });

    let one_step = NonZeroUsize::new(1).expect("one is nonzero");
    let mut observed = Vec::new();
    let mut peer_checkpoint_seen = false;
    let mut suspend_entries = 0;
    let mut suspend_completions = 0;
    let mut terminal = None;
    for _ in 0..16 {
        let outcome = session
            .run_loop_with_observer(&stop_token, one_step, |step| observed.push(*step))
            .expect("bounded CPU_SUSPEND run-loop step should succeed");
        let step = *observed
            .last()
            .expect("each non-stopped run-loop call should observe one step");
        match step {
            HvfVcpuRunStepOutcome::CpuSuspend {
                function_id: PSCI_CPU_SUSPEND_64,
                ..
            } => {
                suspend_entries += 1;
                if suspend_entries == 1 {
                    assert_eq!(read_u32(PRE_SUSPEND_1), 1);
                    assert_eq!(read_u32(POST_SUSPEND_1), 0);
                } else if suspend_entries == 2 {
                    assert_eq!(read_u32(POST_SUSPEND_1), 1);
                    assert_eq!(read_u64(SUSPEND_RESULT_1), 0);
                    assert_eq!(read_u64(SENTINEL_1), SENTINEL);
                    assert_eq!(read_u32(PRE_SUSPEND_2), 1);
                    assert_eq!(read_u32(POST_SUSPEND_2), 0);
                }
            }
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_CPU_SUSPEND_64,
                return_value: 0,
                ..
            } => suspend_completions += 1,
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_VERSION,
                return_value: 0x0001_0000,
                ..
            } => {
                peer_checkpoint_seen = true;
                assert_eq!(read_u64(CPU_ON_RESULT), 0);
                assert_eq!(read_u64(AFFINITY_RESULT), 0);
                assert_eq!(read_u32(PEER_OBSERVATION), 1);
                assert_eq!(read_u32(POST_SUSPEND_1), 0);
            }
            _ => {}
        }
        if matches!(outcome, HvfArm64BootRunLoopOutcome::GuestShutdown { .. }) {
            terminal = Some(outcome);
            break;
        }
        assert!(matches!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        ));
    }

    watchdog_done.store(true, Ordering::Release);
    watchdog.join().expect("CPU_SUSPEND watchdog should join");
    assert!(
        peer_checkpoint_seen,
        "CPU0 should publish its ON-affinity checkpoint"
    );
    assert_eq!(suspend_entries, 2);
    assert_eq!(suspend_completions, 2);
    assert!(matches!(
        terminal,
        Some(HvfArm64BootRunLoopOutcome::GuestShutdown { .. })
    ));
    assert_eq!(read_u32(POST_SUSPEND_2), 1);
    assert_eq!(read_u64(SUSPEND_RESULT_2), 0);
    assert_eq!(read_u64(SENTINEL_2), SENTINEL);
    session
        .shutdown()
        .expect("CPU_SUSPEND session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn psci_1_0_and_smccc_1_1_discovery_match_the_advertised_guest_contract() {
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use bangbang_hvf::{
        HvfArm64BootRunLoopOutcome, HvfArm64BootSessionConfig, HvfVcpuRunStepOutcome,
        OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    const RESULTS_OFFSET: u64 = 0x1000;
    const QUERIES_OFFSET: u64 = 0x2000;
    const NOT_SUPPORTED: u64 = 0x0000_0000_ffff_ffff;
    const FEATURE_QUERIES: [(u32, u64); 36] = [
        (0x8400_0000, 0),             // PSCI_VERSION
        (0x8400_0001, 0),             // CPU_SUSPEND32
        (0xc400_0001, 0),             // CPU_SUSPEND64
        (0x8400_0002, 0),             // CPU_OFF
        (0x8400_0003, 0),             // CPU_ON32
        (0xc400_0003, 0),             // CPU_ON64
        (0x8400_0004, 0),             // AFFINITY_INFO32
        (0xc400_0004, 0),             // AFFINITY_INFO64
        (0x8400_0006, 0),             // MIGRATE_INFO_TYPE
        (0x8400_0008, 0),             // SYSTEM_OFF
        (0x8400_0009, 0),             // SYSTEM_RESET
        (0x8400_000a, 0),             // PSCI_FEATURES
        (0x8000_0000, 0),             // SMCCC_VERSION
        (0x8000_0001, NOT_SUPPORTED), // SMCCC_ARCH_FEATURES is not a PSCI query
        (0x8400_0005, NOT_SUPPORTED), // MIGRATE32
        (0xc400_0005, NOT_SUPPORTED), // MIGRATE64
        (0x8400_0007, NOT_SUPPORTED), // MIGRATE_INFO_UP_CPU32
        (0xc400_0007, NOT_SUPPORTED), // MIGRATE_INFO_UP_CPU64
        (0x8400_000b, NOT_SUPPORTED), // CPU_FREEZE
        (0x8400_000c, NOT_SUPPORTED), // CPU_DEFAULT_SUSPEND32
        (0xc400_000c, NOT_SUPPORTED), // CPU_DEFAULT_SUSPEND64
        (0x8400_000d, NOT_SUPPORTED), // NODE_HW_STATE32
        (0xc400_000d, NOT_SUPPORTED), // NODE_HW_STATE64
        (0x8400_000e, NOT_SUPPORTED), // SYSTEM_SUSPEND32
        (0xc400_000e, NOT_SUPPORTED), // SYSTEM_SUSPEND64
        (0x8400_000f, NOT_SUPPORTED), // PSCI_SET_SUSPEND_MODE
        (0x8400_0010, NOT_SUPPORTED), // PSCI_STAT_RESIDENCY32
        (0xc400_0010, NOT_SUPPORTED), // PSCI_STAT_RESIDENCY64
        (0x8400_0011, NOT_SUPPORTED), // PSCI_STAT_COUNT32
        (0xc400_0011, NOT_SUPPORTED), // PSCI_STAT_COUNT64
        (0x8400_0012, NOT_SUPPORTED), // SYSTEM_RESET2_32
        (0xc400_0012, NOT_SUPPORTED), // SYSTEM_RESET2_64
        (0x8400_0013, NOT_SUPPORTED), // MEM_PROTECT
        (0x8400_0014, NOT_SUPPORTED), // MEM_PROTECT_CHECK_RANGE32
        (0xc400_0014, NOT_SUPPORTED), // MEM_PROTECT_CHECK_RANGE64
        (0xdead_beef, NOT_SUPPORTED), // unknown
    ];
    const EXTRA_RESULT_COUNT: usize = 10;
    const RESULT_COUNT: usize = FEATURE_QUERIES.len() + EXTRA_RESULT_COUNT;
    const RESULTS_SIZE: usize = RESULT_COUNT * size_of::<u64>();
    const PSCI_VERSION: u64 = 0x8400_0000;
    const PSCI_FEATURES: u64 = 0x8400_000a;
    const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
    const ARM_SMCCC_VERSION: u64 = 0x8000_0000;
    const ARM_SMCCC_ARCH_FEATURES: u64 = 0x8000_0001;
    const ARM_SMCCC_PV_TIME_FEATURES_64: u64 = 0xc500_0020;
    const ARM_SMCCC_PV_TIME_ST_64: u64 = 0xc500_0021;

    // Loop over the host-supplied PSCI_FEATURES table, then query PSCI and
    // SMCCC versions plus the mandatory minimum SMCCC_ARCH_FEATURES boundary.
    let guest_code = [
        0x1000_8013, // adr x19, results (+0x1000)
        0x1000_fff4, // adr x20, queries (+0x2000)
        0x5280_0495, // mov w21, #36
        0xb840_4681, // ldr w1, [x20], #4
        0xd280_0140, // mov x0, #0xa
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (PSCI_FEATURES)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0x7100_06b5, // subs w21, w21, #1
        0x54ff_ff41, // b.ne feature loop
        0xd280_0000, // mov x0, #0
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (PSCI_VERSION)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0000, // mov x0, #0
        0xf2b0_0000, // movk x0, #0x8000, lsl #16 (SMCCC_VERSION)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0001, // mov x1, #0
        0xf2b0_0001, // movk x1, #0x8000, lsl #16 (SMCCC_VERSION query)
        0xd280_0020, // mov x0, #1
        0xf2b0_0000, // movk x0, #0x8000, lsl #16 (SMCCC_ARCH_FEATURES)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0021, // mov x1, #1
        0xf2b0_0001, // movk x1, #0x8000, lsl #16 (self query)
        0xd280_0020, // mov x0, #1
        0xf2b0_0000, // movk x0, #0x8000, lsl #16 (SMCCC_ARCH_FEATURES)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd290_0001, // mov x1, #0x8000
        0xf2b0_0001, // movk x1, #0x8000, lsl #16 (WORKAROUND_1 query)
        0xd280_0020, // mov x0, #1
        0xf2b0_0000, // movk x0, #0x8000, lsl #16 (SMCCC_ARCH_FEATURES)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0401, // mov x1, #0x20
        0xf2b8_a001, // movk x1, #0xc500, lsl #16 (PV_TIME_FEATURES query)
        0xd280_0020, // mov x0, #1
        0xf2b0_0000, // movk x0, #0x8000, lsl #16 (SMCCC_ARCH_FEATURES)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0421, // mov x1, #0x21
        0xf2b8_a001, // movk x1, #0xc500, lsl #16 (PV_TIME_ST query)
        0xd280_0400, // mov x0, #0x20
        0xf2b8_a000, // movk x0, #0xc500, lsl #16 (PV_TIME_FEATURES64)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0420, // mov x0, #0x21
        0xf2b8_a000, // movk x0, #0xc500, lsl #16 (PV_TIME_ST64)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0400, // mov x0, #0x20
        0xf2b0_a000, // movk x0, #0x8500, lsl #16 (PV_TIME_FEATURES32)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0420, // mov x0, #0x21
        0xf2b0_a000, // movk x0, #0x8500, lsl #16 (PV_TIME_ST32)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0100, // mov x0, #8
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (SYSTEM_OFF)
        0xd400_0002, // hvc #0
        0x1400_0000, // b .
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect::<Vec<_>>();
    let query_bytes = FEATURE_QUERIES
        .into_iter()
        .flat_map(|(function_id, _)| function_id.to_le_bytes())
        .collect::<Vec<_>>();

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("psci-discovery-kernel", &image)
        .expect("temporary PSCI discovery kernel should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    controller
        .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(1, 16)))
        .expect("one-vCPU discovery machine should configure");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("PSCI discovery session should prepare");
    let entry = GuestAddress::new(
        session
            .capture_arm64_general_register_state()
            .expect("discovery entry registers should capture")
            .pc(),
    );
    let results = entry
        .checked_add(RESULTS_OFFSET)
        .expect("discovery results should fit");
    let queries = entry
        .checked_add(QUERIES_OFFSET)
        .expect("discovery queries should fit");
    {
        let memory = session
            .guest_memory_mut()
            .expect("discovery guest memory should be mutable before execution");
        memory
            .write_slice(&guest_code, entry)
            .expect("discovery guest code should fit");
        memory
            .write_slice(&query_bytes, queries)
            .expect("discovery query table should fit");
        memory
            .write_slice(&[0; RESULTS_SIZE], results)
            .expect("discovery result table should fit");
    }

    let control = session.run_loop_control();
    let stop_token = control.stop_token();
    let watchdog_done = Arc::new(AtomicBool::new(false));
    let watchdog_done_for_thread = Arc::clone(&watchdog_done);
    let watchdog = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !watchdog_done_for_thread.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        if !watchdog_done_for_thread.load(Ordering::Acquire) {
            let _ = control.request_stop();
        }
    });

    let mut observed = Vec::new();
    let outcome = session
        .run_loop_with_observer(
            &stop_token,
            NonZeroUsize::new(64).expect("step limit should be nonzero"),
            |step| observed.push(*step),
        )
        .expect("bounded PSCI discovery guest should run");
    watchdog_done.store(true, Ordering::Release);
    watchdog
        .join()
        .expect("PSCI discovery watchdog should join");
    assert!(matches!(
        outcome,
        HvfArm64BootRunLoopOutcome::GuestShutdown { .. }
    ));
    assert_eq!(
        observed
            .iter()
            .filter(|step| matches!(
                step,
                HvfVcpuRunStepOutcome::Hvc {
                    function_id: PSCI_FEATURES,
                    ..
                }
            ))
            .count(),
        FEATURE_QUERIES.len()
    );
    assert!(observed.iter().any(|step| matches!(
        step,
        HvfVcpuRunStepOutcome::Hvc {
            function_id: PSCI_VERSION,
            return_value: 0x0001_0000,
            ..
        }
    )));
    assert!(observed.iter().any(|step| matches!(
        step,
        HvfVcpuRunStepOutcome::Hvc {
            function_id: ARM_SMCCC_VERSION,
            return_value: 0x0001_0001,
            ..
        }
    )));
    assert_eq!(
        observed
            .iter()
            .filter(|step| matches!(
                step,
                HvfVcpuRunStepOutcome::Hvc {
                    function_id: ARM_SMCCC_ARCH_FEATURES,
                    ..
                }
            ))
            .count(),
        4
    );
    assert!(observed.iter().any(|step| matches!(
        step,
        HvfVcpuRunStepOutcome::Hvc {
            function_id: ARM_SMCCC_PV_TIME_FEATURES_64,
            return_value: u64::MAX,
            ..
        }
    )));
    assert!(observed.iter().any(|step| matches!(
        step,
        HvfVcpuRunStepOutcome::Hvc {
            function_id: ARM_SMCCC_PV_TIME_ST_64,
            return_value: u64::MAX,
            ..
        }
    )));
    assert!(matches!(
        observed.last(),
        Some(HvfVcpuRunStepOutcome::GuestShutdown {
            function_id: PSCI_SYSTEM_OFF,
            ..
        })
    ));

    let mut result_bytes = [0; RESULTS_SIZE];
    session
        .guest_memory()
        .expect("discovery guest memory should remain mapped")
        .read_slice(&mut result_bytes, results)
        .expect("discovery results should read after terminal exit");
    let actual = result_bytes
        .chunks_exact(size_of::<u64>())
        .map(|bytes| u64::from_le_bytes(bytes.try_into().expect("result chunk should be u64")))
        .collect::<Vec<_>>();
    let mut expected = FEATURE_QUERIES
        .iter()
        .map(|(_, result)| *result)
        .collect::<Vec<_>>();
    expected.extend([
        0x0001_0000,
        0x0001_0001,
        0,
        0,
        NOT_SUPPORTED,
        NOT_SUPPORTED,
        u64::MAX,
        u64::MAX,
        u64::MAX,
        u64::MAX,
    ]);
    assert_eq!(actual, expected);

    session
        .shutdown()
        .expect("PSCI discovery session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_native_v1_composite_and_keeps_source_session_usable() {
    use std::io::Cursor;
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig, HvfSnapshotV1Bundle,
        HvfVcpuRunStepOutcome, OwnedHvfArm64BootSession, PreparedHvfSnapshotV1Load,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::{BlockMmioLayout, DriveConfigInput};
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::serial::{SharedSerialOutput, SharedSerialOutputBuffer};
    use bangbang_runtime::snapshot_artifact::{
        SnapshotCommitDurability, load_snapshot_artifacts, publish_snapshot_artifacts_with,
    };
    use bangbang_runtime::snapshot_commit::SnapshotCommitKind;
    use bangbang_runtime::snapshot_device::{
        decode_snapshot_v1_device_state, encode_snapshot_v1_device_state,
    };
    use bangbang_runtime::snapshot_memory::write_snapshot_memory_image;
    use bangbang_runtime::startup::prepare_snapshot_v1_device_profile;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel =
        TempFile::new("snapshot-device-kernel", &image).expect("temp kernel should be created");
    let root = TempFile::new_len("snapshot-device-root", 4096)
        .expect("temp root backing should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", root.path(), true).with_is_read_only(true),
        ))
        .expect("read-only root config should be stored");
    controller
        .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(1, 16)))
        .expect("compact snapshot test machine should configure");

    let serial_buffer = SharedSerialOutputBuffer::default();
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        bangbang_runtime::rtc::RtcMmioLayout::new(
            GuestAddress::new(0x4000_1000),
            MmioRegionId::new(10),
        ),
    )
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        MmioRegionId::new(20),
        GuestAddress::new(0x4000_2000),
        SharedSerialOutput::from(serial_buffer),
    ));
    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("owned snapshot device session should prepare");

    // Write deterministic guest code at the configured entry. The first exit
    // stores a non-default serial scratch register, the second is a PSCI HVC,
    // and the final HVC remains after both captures to prove source resumption.
    let source_entry = GuestAddress::new(
        session
            .capture_arm64_general_register_state()
            .expect("source entry registers should capture")
            .pc(),
    );
    let guest_code = [
        0xd282_4685, // mov x5, #0x1234
        0xd2a8_0001, // mov x1, #0x40000000
        0xf284_00e1, // movk x1, #0x2007
        0xd280_0b42, // mov x2, #0x5a
        0x3900_0022, // strb w2, [x1]
        0xd2b0_8000, // mov x0, #0x84000000 (PSCI_VERSION)
        0xd400_0002, // hvc #0
        0xd28a_cf06, // mov x6, #0x5678
        0xd2b0_8000, // mov x0, #0x84000000 (PSCI_VERSION)
        0xd400_0002, // hvc #0
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect::<Vec<_>>();
    session
        .guest_memory_mut()
        .expect("source guest memory should be mutable before execution")
        .write_slice(&guest_code, source_entry)
        .expect("source guest code should fit at the configured entry");
    assert!(matches!(
        session.run_once_and_handle_mmio(),
        Ok(HvfVcpuRunStepOutcome::Mmio { .. })
    ));
    assert!(matches!(
        session.run_once_and_handle_mmio(),
        Ok(HvfVcpuRunStepOutcome::Hvc {
            function_id: 0x8400_0000,
            return_value: 0x0001_0000,
            ..
        })
    ));

    let artifact_pair = TempSnapshotArtifacts::new("native-v1-composite")
        .expect("snapshot artifact directory should create");
    let artifact_paths = artifact_pair.paths();
    let guard = session
        .quiesce_limiter_retry_wakeups()
        .expect("snapshot device retry work should quiesce");
    let publication = publish_snapshot_artifacts_with(&artifact_paths, |mut writer| {
        let state = session
            .capture_snapshot_v1_state_at(
                &controller.drive_configs()[0],
                controller.serial_config(),
                &guard,
                Instant::now(),
            )
            .expect("complete inactive native-v1 state should capture");
        let binding = write_snapshot_memory_image(
            session
                .guest_memory()
                .expect("source guest memory should remain mapped"),
            &mut writer,
        )
        .expect("source guest memory should stream while quiesced");
        let bundle = HvfSnapshotV1Bundle::try_new(binding, state)
            .expect("complete state and memory should form one bundle");
        drop(writer);
        Ok::<_, std::convert::Infallible>(bundle.into_commit_record())
    })
    .expect("production publisher should commit complete native-v1 capture");
    // Keep all block, PMEM, network, and entropy retry schedulers quiesced
    // through validation, durability barriers, and the no-clobber commit.
    drop(guard);
    assert_eq!(publication.durability(), SnapshotCommitDurability::Durable);
    assert_eq!(publication.record().kind(), SnapshotCommitKind::Composite);
    artifact_pair
        .assert_committed_without_staging()
        .expect("committed artifact directory should contain no staging entries");

    let artifacts = load_snapshot_artifacts(&artifact_paths)
        .expect("production-published artifact pair should validate and load");
    assert_eq!(artifacts.record(), publication.record());
    let bundle = HvfSnapshotV1Bundle::try_from_commit_record(artifacts.record().clone())
        .expect("published composite commit should decode");
    let loaded_memory = artifacts.memory();
    assert_eq!(
        loaded_memory.total_size(),
        session.runtime_resources().layout.total_size()
    );

    let encoded = encode_snapshot_v1_device_state(bundle.state().device())
        .expect("captured snapshot device state should encode");
    let decoded = decode_snapshot_v1_device_state(&encoded)
        .expect("captured snapshot device state should decode");
    assert_eq!(decoded.serial_state().scratch(), 0x5a);
    let mut source_generation_id = [0; 16];
    loaded_memory
        .read_slice(&mut source_generation_id, decoded.vmgenid().range().start())
        .expect("captured VMGenID bytes should read");

    let prepared = prepare_snapshot_v1_device_profile(&decoded, loaded_memory, Instant::now())
        .expect("decoded inactive device profile should prepare off-side");

    assert!(!prepared.block_handler().is_device_activated());
    assert!(
        prepared.drive_config().path_on_host() == controller.drive_configs()[0].path_on_host(),
        "prepared drive path should match without logging either path"
    );
    assert!(
        prepared.vmgenid_device().range == decoded.vmgenid().range(),
        "prepared VMGenID range should match without logging guest addresses"
    );
    assert!(
        prepared.vmclock_device().range == decoded.vmclock().range(),
        "prepared VMClock range should match without logging guest addresses"
    );
    drop(prepared);

    let first_image_id = bundle.commit_record().memory_binding().image_id();
    let second_image_id = {
        let guard = session
            .quiesce_limiter_retry_wakeups()
            .expect("second snapshot retry work should quiesce");
        let state = session
            .capture_snapshot_v1_state_at(
                &controller.drive_configs()[0],
                controller.serial_config(),
                &guard,
                Instant::now(),
            )
            .expect("second complete native-v1 state should capture");
        let mut memory_image = Cursor::new(Vec::new());
        let binding = write_snapshot_memory_image(
            session
                .guest_memory()
                .expect("source guest memory should remain mapped for retry"),
            &mut memory_image,
        )
        .expect("second memory image should stream");
        HvfSnapshotV1Bundle::try_new(binding, state)
            .expect("second complete bundle should validate")
            .commit_record()
            .memory_binding()
            .image_id()
    };
    assert_ne!(first_image_id, second_image_id);
    assert!(matches!(
        session.run_once_and_handle_mmio(),
        Ok(HvfVcpuRunStepOutcome::Hvc {
            function_id: 0x8400_0000,
            return_value: 0x0001_0000,
            ..
        })
    ));
    session
        .capture_arm64_general_register_state()
        .expect("source owner should remain usable after resumption");
    session
        .shutdown()
        .expect("owned snapshot device session should shut down");

    let prepared = PreparedHvfSnapshotV1Load::from_loaded_artifacts(artifacts, Instant::now())
        .expect("production-published pair should prepare without constructing a VM");
    assert!(prepared.runtime().runtime_resources.boot_origin.is_none());

    let restored = OwnedHvfArm64BootSession::restore_snapshot_v1(prepared, true)
        .expect("fresh tracked destination VM should restore from native-v1 artifacts");
    let (mut restored_session, restored_drive, _serial_output, restored_serial_buffer) =
        restored.into_parts();
    assert!(restored_session.boot_registers().is_none());
    assert!(restored_session.runtime_resources().boot_origin.is_none());
    assert!(
        restored_session.arm64_fdt_cache_hierarchy().is_none(),
        "native-v1 restore must not invent cache presentation absent from the schema"
    );
    assert_eq!(restored_drive, controller.drive_configs()[0]);
    assert_eq!(
        restored_serial_buffer
            .bytes()
            .expect("restored serial buffer should read"),
        Vec::<u8>::new()
    );

    let mut destination_generation_id = [0; 16];
    restored_session
        .guest_memory()
        .expect("restored destination memory should remain mapped")
        .read_slice(
            &mut destination_generation_id,
            decoded.vmgenid().range().start(),
        )
        .expect("restored VMGenID bytes should read");
    assert_ne!(destination_generation_id, source_generation_id);
    assert_ne!(destination_generation_id, [0; 16]);
    assert!(
        restored_session
            .runtime_resources()
            .machine_config
            .track_dirty_pages(),
        "the destination load request must override the source tracking flag"
    );
    let restored_tracker = restored_session
        .guest_memory()
        .expect("tracked restored memory should remain mapped")
        .dirty_tracker()
        .expect("tracked restore should retain one shared dirty epoch");
    let page_size = host_page_size().expect("host page size should remain available");
    let vmgenid_page =
        GuestAddress::new(decoded.vmgenid().range().start().raw_value() & !(page_size - 1));
    assert_eq!(
        restored_tracker
            .dirty_pages()
            .expect("post-baseline VMGenID dirty page should query"),
        vec![vmgenid_page],
        "snapshot memory is the clean baseline and VMGenID is the first host write"
    );
    assert_eq!(restored_session.reset_dirty_epoch_quiesced(), Ok(Some(1)));
    assert!(
        restored_tracker
            .dirty_pages()
            .expect("committed restore epoch should clear")
            .is_empty()
    );

    let restored_state = {
        let guard = restored_session
            .quiesce_limiter_retry_wakeups()
            .expect("restored retry work should quiesce before first run");
        restored_session
            .capture_snapshot_v1_state_at(
                &restored_drive,
                &bangbang_runtime::serial::SerialConfig::default(),
                &guard,
                Instant::now(),
            )
            .expect("restored destination state should recapture before first run")
    };
    assert_eq!(restored_state.vcpu(), bundle.state().vcpu());
    assert_eq!(
        restored_state.interrupts().pending_interrupts,
        bundle.state().interrupts().pending_interrupts
    );
    assert!(
        !restored_state.interrupts().gic_device.is_empty(),
        "HVF should recapture a nonempty opaque GIC state after restore"
    );
    assert!(
        restored_state.interrupts().gic_device.len()
            <= bangbang_hvf::HVF_SNAPSHOT_V1_GIC_DEVICE_STATE_MAX_BYTES,
        "recaptured opaque GIC state should remain within the native-v1 bound"
    );
    assert_eq!(
        restored_state.interrupts().gic_icc,
        bundle.state().interrupts().gic_icc
    );
    assert_normalized_timer_restore_equivalent(
        bundle.state().interrupts().timer,
        restored_state.interrupts().timer,
    );
    assert_eq!(restored_state.device().serial_state().scratch(), 0x5a);

    assert!(matches!(
        restored_session.run_once_and_handle_mmio(),
        Ok(HvfVcpuRunStepOutcome::Hvc {
            function_id: 0x8400_0000,
            return_value: 0x0001_0000,
            ..
        })
    ));
    assert_eq!(
        restored_session
            .capture_arm64_general_register_state()
            .expect("restored destination registers should capture after continuation")
            .general_purpose_register(6),
        Some(0x5678)
    );
    restored_session
        .shutdown()
        .expect("restored destination session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn applies_and_verifies_mixed_width_arm64_cpu_template_on_two_hvf_vcpus() {
    use bangbang_hvf::{
        ARM64_LINUX_BOOT_CPSR, HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::cpu::{
        CpuConfigArmRegisterModifier, CpuConfigArmRegisterWidth, CpuConfigInput,
        KVM_REG_ARM64_ACTLR_EL1, KVM_REG_ARM64_CORE_ELR_EL1, KVM_REG_ARM64_CORE_FPCR,
        KVM_REG_ARM64_CORE_FPSR, KVM_REG_ARM64_CORE_PC, KVM_REG_ARM64_CORE_PSTATE,
        KVM_REG_ARM64_CORE_SP_EL0, KVM_REG_ARM64_CORE_SP_EL1, KVM_REG_ARM64_CORE_SPSR_EL1,
        KVM_REG_ARM64_ID_AA64DFR0_EL1, KVM_REG_ARM64_ID_AA64DFR1_EL1,
        KVM_REG_ARM64_ID_AA64ISAR0_EL1, KVM_REG_ARM64_ID_AA64ISAR1_EL1,
        KVM_REG_ARM64_ID_AA64MMFR0_EL1, KVM_REG_ARM64_ID_AA64MMFR1_EL1,
        KVM_REG_ARM64_ID_AA64MMFR2_EL1, KVM_REG_ARM64_ID_AA64PFR0_EL1,
        KVM_REG_ARM64_ID_AA64PFR1_EL1, KVM_REG_ARM64_ID_AA64SMFR0_EL1,
        KVM_REG_ARM64_ID_AA64ZFR0_EL1, kvm_reg_arm64_core_q, kvm_reg_arm64_core_x,
    };
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("cpu-template-kernel", &image)
        .expect("temporary CPU-template kernel should be created");
    let modifier = CpuConfigArmRegisterModifier::new;
    let x0_target = 0x1111_2222_3333_4444_u128;
    let x4_target = 0xffff_eeee_dddd_cccc_u128;
    let x30_target = 0x0123_4567_89ab_cdef_u128;
    let pc_target = 0x2000_u128;
    let pstate_target = 0xa000_0000_u128;
    let sp_el0_target = 0x7777_0000_u128;
    let sp_el1_target = 0x8888_0000_u128;
    let elr_el1_target = 0x9999_0000_u128;
    let spsr_el1_target = u128::from(ARM64_LINUX_BOOT_CPSR);
    let q0_target = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff_u128;
    let q31_target = 0xffee_ddcc_bbaa_9988_7766_5544_3322_1100_u128;
    let fpcr_target = 1_u128 << 22;
    let fpsr_target = 0x11_u128;
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 16)))
        .expect("two-vCPU machine config should store");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should store");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut baseline_session = OwnedHvfArm64BootSession::new(&controller, config.clone())
        .expect("disposable two-vCPU baseline session should prepare");
    let baseline_identification = baseline_session
        .capture_arm64_identification_register_state()
        .expect("baseline identification state should capture without logging values");
    let baseline_optional_identification = baseline_session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("baseline optional identification state should capture without logging values");
    let baseline_execution = baseline_session
        .capture_arm64_execution_control_register_state()
        .expect("baseline execution-control state should capture without logging values");
    baseline_session
        .shutdown()
        .expect("disposable baseline session should shut down cleanly");

    controller
        .handle_action(VmmAction::PutCpuConfig(CpuConfigInput::new(
            Vec::new(),
            vec![
                modifier(
                    kvm_reg_arm64_core_x(0).expect("X0 should have a KVM identity"),
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    x0_target,
                ),
                modifier(
                    kvm_reg_arm64_core_x(4).expect("X4 should have a KVM identity"),
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    x4_target,
                ),
                modifier(
                    kvm_reg_arm64_core_x(30).expect("X30 should have a KVM identity"),
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    x30_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_PC,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    pc_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_PSTATE,
                    CpuConfigArmRegisterWidth::U64,
                    0xf000_0000,
                    pstate_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_SP_EL0,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    sp_el0_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_SP_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    sp_el1_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_ELR_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    elr_el1_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_SPSR_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    spsr_el1_target,
                ),
                modifier(
                    kvm_reg_arm64_core_q(0).expect("Q0 should have a KVM identity"),
                    CpuConfigArmRegisterWidth::U128,
                    u128::MAX,
                    q0_target,
                ),
                modifier(
                    kvm_reg_arm64_core_q(31).expect("Q31 should have a KVM identity"),
                    CpuConfigArmRegisterWidth::U128,
                    u128::MAX,
                    q31_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_FPCR,
                    CpuConfigArmRegisterWidth::U32,
                    0x00c0_0000,
                    fpcr_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_FPSR,
                    CpuConfigArmRegisterWidth::U32,
                    0x1f,
                    fpsr_target,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64PFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0x000f_000f_0000_0000,
                    0,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64ISAR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0xf0ff_0fff_0000_f000,
                    0x1000,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64ISAR1_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0x00ff_f000_00ff_f00f,
                    0x0010_0001,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64MMFR2_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0x0000_000f_0000_0000,
                    0,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64PFR1_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_identification.id_aa64pfr1_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64DFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_identification.id_aa64dfr0_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64DFR1_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_identification.id_aa64dfr1_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64MMFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_identification.id_aa64mmfr0_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64MMFR1_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_identification.id_aa64mmfr1_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64ZFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_optional_identification.id_aa64zfr0_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64SMFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_optional_identification.id_aa64smfr0_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ACTLR_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    2,
                    2,
                ),
            ],
            Vec::new(),
        )))
        .expect("mixed-width CPU template should store");

    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("mixed-width template should write and read back on both HVF vCPUs");
    let boot_registers = session
        .boot_registers()
        .expect("ordinary CPU-template session should retain boot registers");
    let general = session
        .capture_arm64_general_register_state()
        .expect("mixed-width CPU-template general state should capture");
    assert!(
        general.general_purpose_register(0) == Some(boot_registers.fdt_address.raw_value()),
        "Linux boot setup must override the template's X0 value"
    );
    assert!(
        general.general_purpose_register(4) == u64::try_from(x4_target).ok(),
        "X4 must retain the exact mixed-width CPU-template target"
    );
    assert!(
        general.general_purpose_register(30) == u64::try_from(x30_target).ok(),
        "X30 must retain the exact mixed-width CPU-template target"
    );
    assert!(
        general.pc() == boot_registers.kernel_entry.raw_value(),
        "Linux boot setup must override the template's PC value"
    );
    assert!(
        general.cpsr() == ARM64_LINUX_BOOT_CPSR,
        "Linux boot setup must override the template's PSTATE value"
    );
    let core_system = session
        .capture_arm64_core_system_register_state()
        .expect("mixed-width CPU-template core system state should capture");
    assert!(
        core_system.sp_el0() == u64::try_from(sp_el0_target).expect("target should fit U64"),
        "SP_EL0 must retain the exact CPU-template target"
    );
    assert!(
        core_system.sp_el1() == u64::try_from(sp_el1_target).expect("target should fit U64"),
        "SP_EL1 must retain the exact CPU-template target"
    );
    assert!(
        core_system.elr_el1() == u64::try_from(elr_el1_target).expect("target should fit U64"),
        "ELR_EL1 must retain the exact CPU-template target"
    );
    assert!(
        core_system.spsr_el1() == u64::try_from(spsr_el1_target).expect("target should fit U64"),
        "SPSR_EL1 must retain the exact CPU-template target"
    );
    let simd_fp = session
        .capture_arm64_simd_fp_state()
        .expect("mixed-width CPU-template SIMD/FP state should capture");
    assert!(
        simd_fp.q_register(0) == Some(q0_target.to_le_bytes()),
        "Q0 must retain the exact little-endian CPU-template target"
    );
    assert!(
        simd_fp.q_register(31) == Some(q31_target.to_le_bytes()),
        "Q31 must retain the exact little-endian CPU-template target"
    );
    assert!(
        simd_fp.fpcr() == u64::try_from(fpcr_target).expect("target should fit U64"),
        "FPCR must retain the zero-extended U32 CPU-template target"
    );
    assert!(
        simd_fp.fpsr() == u64::try_from(fpsr_target).expect("target should fit U64"),
        "FPSR must retain the zero-extended U32 CPU-template target"
    );
    let identification = session
        .capture_arm64_identification_register_state()
        .expect("complete CPU-template identification state should capture");
    let identification_again = session
        .capture_arm64_identification_register_state()
        .expect("complete CPU-template identification state should recapture");
    assert!(
        identification == identification_again,
        "baseline-preserving ID targets must remain stable after exact transaction readback"
    );
    assert!(
        identification.id_aa64pfr1_el1() == baseline_identification.id_aa64pfr1_el1()
            && identification.id_aa64dfr0_el1() == baseline_identification.id_aa64dfr0_el1()
            && identification.id_aa64dfr1_el1() == baseline_identification.id_aa64dfr1_el1()
            && identification.id_aa64mmfr0_el1() == baseline_identification.id_aa64mmfr0_el1()
            && identification.id_aa64mmfr1_el1() == baseline_identification.id_aa64mmfr1_el1(),
        "all five new baseline-tier ID targets must match the disposable host baseline"
    );
    let optional_identification = session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("optional CPU-template identification state should capture");
    let optional_identification_again = session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("optional CPU-template identification state should recapture");
    assert!(
        optional_identification == optional_identification_again,
        "baseline-preserving ZFR0/SMFR0 targets must remain stable after exact readback"
    );
    assert!(
        optional_identification == baseline_optional_identification,
        "both optional ID targets must match the disposable host baseline"
    );
    let execution = session
        .capture_arm64_execution_control_register_state()
        .expect("CPU-template ACTLR state should capture");
    assert!(
        execution.actlr_el1() == (baseline_execution.actlr_el1() | 2),
        "ACTLR.EnTSO must retain the exact documented CPU-template target"
    );
    session
        .shutdown()
        .expect("CPU-template session should shut down cleanly");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn owned_hvf_arm64_boot_session_cleans_up_after_prepare_error() {
    use bangbang_hvf::{
        HvfArm64BootSessionConfig, HvfArm64BootSessionError, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::startup::Arm64BootResourceError;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let empty_controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");

    let err = OwnedHvfArm64BootSession::new(&empty_controller, config.clone())
        .expect_err("missing boot source should fail owned HVF session preparation");
    assert!(matches!(
        err,
        HvfArm64BootSessionError::AssembleResources {
            source: Arm64BootResourceError::MissingBootSource
        }
    ));

    let image = arm64_image().expect("test arm64 image should build");
    let kernel =
        TempFile::new("owned-session-retry-kernel", &image).expect("temp kernel should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");

    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("owned HVF arm64 boot session should prepare after failed preparation");
    session
        .shutdown()
        .expect("owned HVF arm64 boot session should shut down after retry");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn rejects_boot_session_on_existing_hvf_vm_without_destroying_it() {
    use bangbang_hvf::{HvfArm64BootSessionConfig, HvfArm64BootSessionError, HvfBackend};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("existing VM should be created");

    let err = backend
        .prepare_arm64_boot_session(
            &controller,
            HvfArm64BootSessionConfig::new(
                BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
                PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
                NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
                VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
                test_rtc_mmio_layout(),
            ),
        )
        .expect_err("existing VM should be rejected");

    assert!(matches!(
        err,
        HvfArm64BootSessionError::BackendAlreadyInitialized
    ));
    let _metadata = backend
        .create_gic()
        .expect("existing VM should remain available after rejected session");
    backend
        .destroy_vm()
        .expect("existing VM should remain owned by caller");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn host_page_size() -> Result<u64, std::num::TryFromIntError> {
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no pointer arguments and does not
    // require process-local invariants from Rust.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };

    u64::try_from(page_size)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Clone, Copy, Debug, Default)]
struct ProcessMemoryUsage {
    virtual_size: u64,
    resident_size: u64,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl ProcessMemoryUsage {
    const fn saturating_growth_from(self, baseline: Self) -> Self {
        Self {
            virtual_size: self.virtual_size.saturating_sub(baseline.virtual_size),
            resident_size: self.resident_size.saturating_sub(baseline.resident_size),
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn process_memory_usage() -> std::io::Result<ProcessMemoryUsage> {
    let mut task_info = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let expected_size =
        i32::try_from(std::mem::size_of::<libc::proc_taskinfo>()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "proc_taskinfo size exceeds c_int",
            )
        })?;
    // SAFETY: `task_info` points to writable storage of exactly
    // `expected_size` bytes. The current PID and fixed task-info flavor need no
    // additional lifetime, ownership, or thread-local guarantees.
    let returned_size = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            task_info.as_mut_ptr().cast(),
            expected_size,
        )
    };
    if returned_size == expected_size {
        // SAFETY: an exact successful `PROC_PIDTASKINFO` result initialized the
        // complete `proc_taskinfo` output object.
        let task_info = unsafe { task_info.assume_init() };
        Ok(ProcessMemoryUsage {
            virtual_size: task_info.pti_virtual_size,
            resident_size: task_info.pti_resident_size,
        })
    } else if returned_size == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "proc_pidinfo returned a partial task record",
        ))
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct TempSnapshotArtifacts {
    directory: std::path::PathBuf,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl TempSnapshotArtifacts {
    fn new(name: &str) -> std::io::Result<Self> {
        let id = NEXT_HVF_TEST_FILE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("bangbang-hvf-{name}-{}-{id}", std::process::id()));
        std::fs::create_dir(&directory)?;
        Ok(Self { directory })
    }

    fn paths(&self) -> bangbang_runtime::snapshot_artifact::SnapshotArtifactPaths {
        bangbang_runtime::snapshot_artifact::SnapshotArtifactPaths::new(
            self.directory.join("state.snap"),
            self.directory.join("memory.snap"),
        )
    }

    fn assert_committed_without_staging(&self) -> std::io::Result<()> {
        let paths = self.paths();
        assert!(paths.state().is_file());
        assert!(paths.memory().is_file());
        let entries = std::fs::read_dir(&self.directory)?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<_>>>()?;
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .all(|name| !name.starts_with(".bangbang-snapshot-"))
        );
        Ok(())
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for TempSnapshotArtifacts {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct TempFile {
    path: std::path::PathBuf,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl TempFile {
    fn new(name: &str, bytes: &[u8]) -> std::io::Result<Self> {
        use std::io::Write as _;

        let id = NEXT_HVF_TEST_FILE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("bangbang-hvf-{name}-{}-{}", std::process::id(), id));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(bytes)?;

        Ok(Self { path })
    }

    fn new_len(name: &str, len: u64) -> std::io::Result<Self> {
        let id = NEXT_HVF_TEST_FILE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("bangbang-hvf-{name}-{}-{}", std::process::id(), id));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.set_len(len)?;

        Ok(Self { path })
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn path_text(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn arm64_image() -> Result<Vec<u8>, &'static str> {
    const ARM64_IMAGE_HEADER_SIZE: usize = 64;
    const ARM64_IMAGE_TEXT_OFFSET_OFFSET: usize = 8;
    const ARM64_IMAGE_SIZE_OFFSET: usize = 16;
    const ARM64_IMAGE_MAGIC_OFFSET: usize = 56;
    const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;

    let mut bytes = vec![0xaa; ARM64_IMAGE_HEADER_SIZE];
    write_u64_le(&mut bytes, ARM64_IMAGE_TEXT_OFFSET_OFFSET, 0)?;
    write_u64_le(
        &mut bytes,
        ARM64_IMAGE_SIZE_OFFSET,
        ARM64_IMAGE_HEADER_SIZE as u64,
    )?;
    write_u32_le(&mut bytes, ARM64_IMAGE_MAGIC_OFFSET, ARM64_IMAGE_MAGIC)?;
    Ok(bytes)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn arm64_store_u32_and_hvc_program(target: u64, value: u32) -> Vec<u8> {
    const MOVZ_X0: u32 = 0xd280_0000;
    const MOVK_X0: u32 = 0xf280_0000;
    const MOVZ_W1: u32 = 0x5280_0001;
    const MOVK_W1_LSL_16: u32 = 0x72a0_0001;
    const STR_W1_X0: u32 = 0xb900_0001;
    const DMB_ISH: u32 = 0xd503_3bbf;
    const HVC_ZERO: u32 = 0xd400_0002;

    let mut instructions = Vec::with_capacity(9);
    instructions.push(MOVZ_X0 | u32::from(target as u16) << 5);
    for halfword in 1..4_u32 {
        let immediate = ((target >> (halfword * 16)) & u64::from(u16::MAX)) as u32;
        instructions.push(MOVK_X0 | (halfword << 21) | (immediate << 5));
    }
    instructions.push(MOVZ_W1 | u32::from(value as u16) << 5);
    instructions.push(MOVK_W1_LSL_16 | ((value >> 16) << 5));
    instructions.push(STR_W1_X0);
    instructions.push(DMB_ISH);
    instructions.push(HVC_ZERO);
    instructions
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_u64_le(bytes: &mut [u8], offset: usize, value: u64) -> Result<(), &'static str> {
    let end = offset + std::mem::size_of::<u64>();
    let destination = bytes
        .get_mut(offset..end)
        .ok_or("u64 write range should fit test image")?;
    destination.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_u32_le(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), &'static str> {
    let end = offset + std::mem::size_of::<u32>();
    let destination = bytes
        .get_mut(offset..end)
        .ok_or("u32 write range should fit test image")?;
    destination.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[test]
fn requires_macos_apple_silicon() {
    panic!("signed HVF lifecycle tests require macOS Apple Silicon");
}
