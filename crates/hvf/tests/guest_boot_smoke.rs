#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static GUEST_BOOT_SMOKE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BOOT_MARKER: &[u8] = b"BANGBANG_BOOT_OK\n";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GUEST_BOOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_to_guest_marker() {
    use std::num::NonZeroUsize;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootRunLoopOutcome, HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
        OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::serial::SharedSerialOutputBuffer;
    use bangbang_runtime::{VmmAction, VmmController};

    let _test_lock = GUEST_BOOT_SMOKE_TEST_LOCK
        .lock()
        .expect("guest boot smoke test lock should not be poisoned");
    let kernel_path = env_path("BANGBANG_GUEST_KERNEL_PATH");
    let initrd_path = env_path("BANGBANG_GUEST_INITRD_PATH");
    let serial_output = SharedSerialOutputBuffer::default();
    let mut controller = VmmController::new("guest-boot-smoke", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(
            BootSourceConfigInput::new(kernel_path)
                .with_initrd_path(initrd_path)
                .with_boot_args("console=ttyS0 reboot=k panic=1 rdinit=/init"),
        ))
        .expect("guest boot smoke boot source should configure");
    let config = HvfArm64BootSessionConfig::new(BlockMmioLayout::new(
        GuestAddress::new(0x5000_0000),
        MmioRegionId::new(1),
    ))
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        MmioRegionId::new(0),
        GuestAddress::new(0x4000_0000),
        serial_output.clone(),
    ));
    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("guest boot smoke session should prepare");
    let run_loop_control = session.run_loop_control();
    let stop_token = run_loop_control.stop_token();
    let watchdog_control = run_loop_control.clone();
    let (done_sender, done_receiver) = mpsc::channel();
    let watchdog = thread::spawn(move || {
        if done_receiver.recv_timeout(GUEST_BOOT_TIMEOUT).is_err() {
            let _ = watchdog_control.request_stop();
            true
        } else {
            false
        }
    });
    let one_step = NonZeroUsize::new(1).expect("one-step limit should be non-zero");
    let started_at = Instant::now();
    let mut terminal_outcome = None;

    while started_at.elapsed() < GUEST_BOOT_TIMEOUT {
        if serial_contains_marker(&serial_output) {
            break;
        }

        let outcome = session
            .run_loop(&stop_token, one_step)
            .expect("guest boot smoke run-loop should not fail before marker");

        if serial_contains_marker(&serial_output) {
            break;
        }

        if !matches!(outcome, HvfArm64BootRunLoopOutcome::StepLimitReached { .. }) {
            terminal_outcome = Some(outcome);
            break;
        }
    }

    if !serial_contains_marker(&serial_output) {
        let _ = run_loop_control.request_stop();
    }
    let _ = done_sender.send(());
    let watchdog_timed_out = watchdog
        .join()
        .expect("guest boot smoke watchdog should join");
    let serial_bytes = serial_output
        .bytes()
        .expect("guest boot smoke serial output should read");
    session
        .shutdown()
        .expect("guest boot smoke session should shut down");

    assert!(
        !watchdog_timed_out,
        "guest boot smoke exceeded {:?}; serial output:\n{}",
        GUEST_BOOT_TIMEOUT,
        String::from_utf8_lossy(&serial_bytes)
    );
    assert!(
        bytes_contain_marker(&serial_bytes),
        "guest boot smoke did not observe marker {:?}; terminal outcome: {:?}; serial output:\n{}",
        String::from_utf8_lossy(BOOT_MARKER),
        terminal_outcome,
        String::from_utf8_lossy(&serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn env_path(name: &str) -> std::path::PathBuf {
    std::env::var_os(name)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| panic!("{name} must be set by scripts/run-guest-boot-smoke.sh"))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn serial_contains_marker(output: &bangbang_runtime::serial::SharedSerialOutputBuffer) -> bool {
    output
        .bytes()
        .expect("guest boot smoke serial output should read")
        .windows(BOOT_MARKER.len())
        .any(|window| window == BOOT_MARKER)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn bytes_contain_marker(bytes: &[u8]) -> bool {
    bytes
        .windows(BOOT_MARKER.len())
        .any(|window| window == BOOT_MARKER)
}
