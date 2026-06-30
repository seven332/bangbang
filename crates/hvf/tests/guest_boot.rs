// clippy.toml allows these in #[test] bodies, but integration-test helpers are
// ordinary functions in the test crate. Keep the exception scoped to this test.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static GUEST_BOOT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BOOT_MARKER: &[u8] = b"BANGBANG_BOOT_OK";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BOOT_ARGS: &str = "console=ttyS0 reboot=k panic=1 rdinit=/init";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GUEST_BOOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SERIAL_MMIO_BASE: u64 = 0x4000_0000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BLOCK_MMIO_BASE: u64 = 0x5000_0000;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn boots_firecracker_kernel_to_guest_marker() {
    use std::num::NonZeroUsize;
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::serial::SharedSerialOutputBuffer;
    use bangbang_runtime::{VmmAction, VmmController};

    let _test_lock = GUEST_BOOT_TEST_LOCK
        .lock()
        .expect("guest boot integration test lock should not be poisoned");
    let kernel_path = env_path("BANGBANG_GUEST_KERNEL_PATH");
    let initrd_path = env_path("BANGBANG_GUEST_INITRD_PATH");
    let serial_output = SharedSerialOutputBuffer::default();
    let mut controller = VmmController::new("guest-boot", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(
            BootSourceConfigInput::new(kernel_path.clone())
                .with_initrd_path(initrd_path.clone())
                .with_boot_args(BOOT_ARGS),
        ))
        .expect("guest boot test boot source should configure");
    let serial_address = GuestAddress::new(SERIAL_MMIO_BASE);
    let config = HvfArm64BootSessionConfig::new(BlockMmioLayout::new(
        GuestAddress::new(BLOCK_MMIO_BASE),
        MmioRegionId::new(1),
    ))
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        MmioRegionId::new(0),
        serial_address,
        serial_output.clone(),
    ));
    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("guest boot test session should prepare");
    let boot_diagnostics =
        GuestBootDiagnostics::from_session(&session, kernel_path, initrd_path, serial_address);
    validate_pre_run_boot_metadata(&session, &boot_diagnostics);
    let run_loop_control = session.run_loop_control();
    let stop_token = run_loop_control.stop_token();
    let watchdog = GuestBootWatchdog::spawn(run_loop_control.clone());
    let one_step = NonZeroUsize::new(1).expect("one-step limit should be non-zero");
    let started_at = Instant::now();
    let mut run_diagnostics = GuestBootRunDiagnostics::default();
    let mut terminal_outcome = None;

    while started_at.elapsed() < GUEST_BOOT_TIMEOUT {
        if serial_contains_marker(&serial_output) {
            break;
        }

        let outcome = session
            .run_loop_with_observer(&stop_token, one_step, |step| {
                run_diagnostics.record_step(step);
            })
            .expect("guest boot test run-loop should not fail before marker");
        run_diagnostics.record_loop_outcome(&outcome);

        if serial_contains_marker(&serial_output) {
            break;
        }

        if !run_diagnostics.loop_outcome_was_step_limit(&outcome) {
            terminal_outcome = Some(outcome);
            break;
        }
    }

    let marker_observed = serial_contains_marker(&serial_output);
    let stop_requested_after_loop = if marker_observed {
        false
    } else {
        let _ = run_loop_control.request_stop();
        true
    };
    let watchdog_timed_out = watchdog.finish();
    let elapsed = started_at.elapsed();
    let serial_bytes = serial_output
        .bytes()
        .expect("guest boot test serial output should read");
    run_diagnostics.finish(
        elapsed,
        marker_observed,
        stop_requested_after_loop,
        watchdog_timed_out,
        serial_bytes.len(),
        terminal_outcome.as_ref(),
    );
    session
        .shutdown()
        .expect("guest boot test session should shut down");

    assert!(
        !watchdog_timed_out,
        "guest boot test watchdog canceled the vCPU run\n{}\nserial output:\n{}",
        GuestBootFailureReport::new(&boot_diagnostics, &run_diagnostics),
        String::from_utf8_lossy(&serial_bytes)
    );
    assert!(
        bytes_contain_marker(&serial_bytes),
        "guest boot test did not observe marker {:?}\n{}\nserial output:\n{}",
        String::from_utf8_lossy(BOOT_MARKER),
        GuestBootFailureReport::new(&boot_diagnostics, &run_diagnostics),
        String::from_utf8_lossy(&serial_bytes)
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct GuestBootWatchdog {
    done_sender: Option<std::sync::mpsc::Sender<()>>,
    handle: Option<std::thread::JoinHandle<bool>>,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl GuestBootWatchdog {
    fn spawn(control: bangbang_hvf::HvfArm64BootRunLoopControl) -> Self {
        let (done_sender, done_receiver) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            if done_receiver.recv_timeout(GUEST_BOOT_TIMEOUT).is_err() {
                let _ = control.request_stop();
                true
            } else {
                false
            }
        });

        Self {
            done_sender: Some(done_sender),
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> bool {
        self.signal_done();
        self.join().expect("guest boot test watchdog should join")
    }

    fn signal_done(&mut self) {
        if let Some(done_sender) = self.done_sender.take() {
            let _ = done_sender.send(());
        }
    }

    fn join(&mut self) -> std::thread::Result<bool> {
        if let Some(handle) = self.handle.take() {
            handle.join()
        } else {
            Ok(false)
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for GuestBootWatchdog {
    fn drop(&mut self) {
        self.signal_done();
        let _ = self.join();
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug, Clone)]
struct GuestBootDiagnostics {
    kernel_path: std::path::PathBuf,
    initrd_path: std::path::PathBuf,
    boot_args: &'static str,
    boot_pc: u64,
    fdt_address: u64,
    fdt_size: usize,
    initrd_address: u64,
    initrd_size: u64,
    serial_mmio_base: u64,
    serial_mmio_size: u64,
    serial_interrupt_line: u32,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl GuestBootDiagnostics {
    fn from_session(
        session: &bangbang_hvf::OwnedHvfArm64BootSession,
        kernel_path: std::path::PathBuf,
        initrd_path: std::path::PathBuf,
        expected_serial_address: bangbang_runtime::memory::GuestAddress,
    ) -> Self {
        let resources = session.runtime_resources();
        let initrd = resources
            .loaded_boot_source
            .initrd
            .expect("guest boot test initrd should be loaded");
        let serial = resources
            .serial_device
            .as_ref()
            .expect("guest boot test serial device should be registered");
        assert_eq!(
            serial.region.range().start(),
            expected_serial_address,
            "guest boot test serial MMIO base should match test config"
        );
        assert_eq!(
            Some(serial.fdt_device.interrupt_line),
            session.serial_interrupt_line(),
            "guest boot test runtime and HVF serial interrupt metadata should match"
        );

        Self {
            kernel_path,
            initrd_path,
            boot_args: BOOT_ARGS,
            boot_pc: session.boot_registers().kernel_entry.raw_value(),
            fdt_address: resources.fdt.address.raw_value(),
            fdt_size: resources.fdt.size,
            initrd_address: initrd.address.raw_value(),
            initrd_size: initrd.size,
            serial_mmio_base: serial.region.range().start().raw_value(),
            serial_mmio_size: serial.region.range().size(),
            serial_interrupt_line: serial.fdt_device.interrupt_line.raw_value(),
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GuestBootRunDiagnostics {
    run_loop_calls: usize,
    raw_steps: usize,
    completed_steps: usize,
    step_limit_outcomes: usize,
    hvc_steps: usize,
    sys64_steps: usize,
    mmio_steps: usize,
    virtual_timer_steps: usize,
    canceled_steps: usize,
    unknown_steps: usize,
    terminal_outcome: Option<String>,
    last_step: Option<String>,
    elapsed: Option<std::time::Duration>,
    marker_observed: bool,
    stop_requested_after_loop: bool,
    watchdog_timed_out: bool,
    serial_byte_count: usize,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl GuestBootRunDiagnostics {
    fn record_step(&mut self, step: &bangbang_hvf::HvfVcpuRunStepOutcome) {
        self.raw_steps += 1;
        self.last_step = Some(format!("{step:?}"));
        match step {
            bangbang_hvf::HvfVcpuRunStepOutcome::Canceled => {
                self.canceled_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::Hvc { .. } => {
                self.hvc_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::Sys64 { .. } => {
                self.sys64_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::Mmio { .. } => {
                self.mmio_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::VtimerActivated => {
                self.virtual_timer_steps += 1;
            }
            bangbang_hvf::HvfVcpuRunStepOutcome::Unknown { .. } => {
                self.unknown_steps += 1;
            }
        }
    }

    fn record_loop_outcome(&mut self, outcome: &bangbang_hvf::HvfArm64BootRunLoopOutcome) {
        self.run_loop_calls += 1;
        self.completed_steps += run_loop_completed_steps(outcome);
        if self.loop_outcome_was_step_limit(outcome) {
            self.step_limit_outcomes += 1;
        } else {
            self.terminal_outcome = Some(format!("{outcome:?}"));
        }
    }

    fn loop_outcome_was_step_limit(
        &self,
        outcome: &bangbang_hvf::HvfArm64BootRunLoopOutcome,
    ) -> bool {
        matches!(
            outcome,
            bangbang_hvf::HvfArm64BootRunLoopOutcome::StepLimitReached { .. }
        )
    }

    fn finish(
        &mut self,
        elapsed: std::time::Duration,
        marker_observed: bool,
        stop_requested_after_loop: bool,
        watchdog_timed_out: bool,
        serial_byte_count: usize,
        terminal_outcome: Option<&bangbang_hvf::HvfArm64BootRunLoopOutcome>,
    ) {
        self.elapsed = Some(elapsed);
        self.marker_observed = marker_observed;
        self.stop_requested_after_loop = stop_requested_after_loop;
        self.watchdog_timed_out = watchdog_timed_out;
        self.serial_byte_count = serial_byte_count;
        if let Some(outcome) = terminal_outcome {
            self.terminal_outcome = Some(format!("{outcome:?}"));
        }
    }

    fn timeout_classification(&self) -> &'static str {
        if self.marker_observed {
            "marker-observed"
        } else if self.watchdog_timed_out {
            "watchdog-canceled-in-flight-vcpu-run"
        } else if self.terminal_outcome.is_some() {
            "terminal-run-loop-outcome"
        } else if self.run_loop_calls > 0 && self.run_loop_calls == self.step_limit_outcomes {
            "outer-timeout-after-handled-steps"
        } else {
            "outer-timeout-without-terminal-outcome"
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Debug)]
struct GuestBootFailureReport<'a> {
    boot: &'a GuestBootDiagnostics,
    run: &'a GuestBootRunDiagnostics,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl<'a> GuestBootFailureReport<'a> {
    const fn new(boot: &'a GuestBootDiagnostics, run: &'a GuestBootRunDiagnostics) -> Self {
        Self { boot, run }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl std::fmt::Display for GuestBootFailureReport<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let elapsed = self
            .run
            .elapsed
            .map(|duration| format!("{duration:?}"))
            .unwrap_or_else(|| "unknown".to_string());
        let terminal = self.run.terminal_outcome.as_deref().unwrap_or("none");
        let last_step = self.run.last_step.as_deref().unwrap_or("none");

        writeln!(f, "guest boot diagnostics:")?;
        writeln!(f, "  classification: {}", self.run.timeout_classification())?;
        writeln!(f, "  elapsed: {elapsed}")?;
        writeln!(f, "  marker observed: {}", self.run.marker_observed)?;
        writeln!(f, "  watchdog timed out: {}", self.run.watchdog_timed_out)?;
        writeln!(
            f,
            "  stop requested after loop: {}",
            self.run.stop_requested_after_loop
        )?;
        writeln!(f, "  serial bytes captured: {}", self.run.serial_byte_count)?;
        writeln!(f, "  run-loop calls: {}", self.run.run_loop_calls)?;
        writeln!(
            f,
            "  completed run-loop steps: {}",
            self.run.completed_steps
        )?;
        writeln!(f, "  raw observed steps: {}", self.run.raw_steps)?;
        writeln!(f, "  step-limit outcomes: {}", self.run.step_limit_outcomes)?;
        writeln!(
            f,
            "  raw step counts: hvc={}, sys64={}, mmio={}, vtimer={}, canceled={}, unknown={}",
            self.run.hvc_steps,
            self.run.sys64_steps,
            self.run.mmio_steps,
            self.run.virtual_timer_steps,
            self.run.canceled_steps,
            self.run.unknown_steps
        )?;
        writeln!(f, "  terminal outcome: {terminal}")?;
        writeln!(f, "  last raw step: {last_step}")?;
        writeln!(f, "  kernel path: {}", self.boot.kernel_path.display())?;
        writeln!(f, "  initrd path: {}", self.boot.initrd_path.display())?;
        writeln!(f, "  boot args: {}", self.boot.boot_args)?;
        writeln!(f, "  boot PC: 0x{:x}", self.boot.boot_pc)?;
        writeln!(
            f,
            "  FDT: address=0x{:x}, size={}",
            self.boot.fdt_address, self.boot.fdt_size
        )?;
        writeln!(
            f,
            "  initrd: address=0x{:x}, size={}",
            self.boot.initrd_address, self.boot.initrd_size
        )?;
        writeln!(
            f,
            "  serial: base=0x{:x}, size={}, interrupt_line={}",
            self.boot.serial_mmio_base, self.boot.serial_mmio_size, self.boot.serial_interrupt_line
        )
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn validate_pre_run_boot_metadata(
    session: &bangbang_hvf::OwnedHvfArm64BootSession,
    diagnostics: &GuestBootDiagnostics,
) {
    use device_tree::DeviceTree;

    let resources = session.runtime_resources();
    assert_eq!(
        resources.loaded_boot_source.command_line.as_str(),
        BOOT_ARGS,
        "guest boot test boot args should match diagnostics"
    );
    assert_eq!(
        resources
            .loaded_boot_source
            .initrd
            .expect("guest boot test initrd should be loaded")
            .address
            .raw_value(),
        diagnostics.initrd_address,
        "guest boot test initrd address should match diagnostics"
    );

    let mut fdt_bytes = vec![0; resources.fdt.size];
    session
        .guest_memory()
        .expect("guest boot test memory should be mapped")
        .read_slice(&mut fdt_bytes, resources.fdt.address)
        .expect("guest boot test FDT bytes should read");
    let tree = DeviceTree::load(&fdt_bytes).expect("guest boot test FDT should parse");
    let chosen = tree
        .find("/chosen")
        .expect("guest boot test FDT should contain /chosen");
    assert_eq!(chosen.prop_str("bootargs").unwrap(), BOOT_ARGS);
    assert_eq!(
        chosen.prop_u64("linux,initrd-start").unwrap(),
        diagnostics.initrd_address
    );
    assert_eq!(
        chosen.prop_u64("linux,initrd-end").unwrap(),
        diagnostics.initrd_address + diagnostics.initrd_size
    );

    let serial_node_path = format!("/uart@{:x}", diagnostics.serial_mmio_base);
    let serial = tree
        .find(&serial_node_path)
        .expect("guest boot test FDT should contain serial node");
    assert_eq!(serial.prop_str("compatible").unwrap(), "ns16550a");
    assert_eq!(
        prop_u64_cells(serial, "reg"),
        [diagnostics.serial_mmio_base, diagnostics.serial_mmio_size]
    );
    assert_eq!(
        prop_u32_cells(serial, "interrupts"),
        [0, diagnostics.serial_interrupt_line - 32, 1]
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn run_loop_completed_steps(outcome: &bangbang_hvf::HvfArm64BootRunLoopOutcome) -> usize {
    match outcome {
        bangbang_hvf::HvfArm64BootRunLoopOutcome::StepLimitReached { steps }
        | bangbang_hvf::HvfArm64BootRunLoopOutcome::Stopped { steps }
        | bangbang_hvf::HvfArm64BootRunLoopOutcome::Canceled { steps }
        | bangbang_hvf::HvfArm64BootRunLoopOutcome::Unknown { steps, .. } => *steps,
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn prop_u32_cells(node: &device_tree::Node, name: &str) -> Vec<u32> {
    let raw = node.prop_raw(name).expect("property should exist");
    assert_eq!(raw.len() % 4, 0, "{name} property should contain u32 cells");

    raw.chunks_exact(4)
        .map(|chunk| u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn prop_u64_cells(node: &device_tree::Node, name: &str) -> Vec<u64> {
    let raw = node.prop_raw(name).expect("property should exist");
    assert_eq!(raw.len() % 8, 0, "{name} property should contain u64 cells");

    raw.chunks_exact(8)
        .map(|chunk| {
            u64::from_be_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ])
        })
        .collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn env_path(name: &str) -> std::path::PathBuf {
    std::env::var_os(name)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| panic!("{name} must be set by scripts/run-guest-boot-tests.sh"))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn serial_contains_marker(output: &bangbang_runtime::serial::SharedSerialOutputBuffer) -> bool {
    output
        .bytes()
        .expect("guest boot test serial output should read")
        .windows(BOOT_MARKER.len())
        .any(|window| window == BOOT_MARKER)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn bytes_contain_marker(bytes: &[u8]) -> bool {
    bytes
        .windows(BOOT_MARKER.len())
        .any(|window| window == BOOT_MARKER)
}

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod tests {
    use super::{
        GuestBootDiagnostics, GuestBootFailureReport, GuestBootRunDiagnostics,
        bytes_contain_marker, run_loop_completed_steps,
    };

    #[test]
    fn guest_boot_run_diagnostics_classifies_outer_timeout_after_handled_steps() {
        let mut diagnostics = GuestBootRunDiagnostics::default();
        let outcome = bangbang_hvf::HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 };
        diagnostics.record_step(&bangbang_hvf::HvfVcpuRunStepOutcome::VtimerActivated);
        diagnostics.record_loop_outcome(&outcome);
        diagnostics.finish(
            std::time::Duration::from_secs(30),
            false,
            true,
            false,
            0,
            None,
        );

        assert_eq!(
            diagnostics.timeout_classification(),
            "outer-timeout-after-handled-steps"
        );
        assert_eq!(diagnostics.run_loop_calls, 1);
        assert_eq!(diagnostics.step_limit_outcomes, 1);
        assert_eq!(diagnostics.virtual_timer_steps, 1);
    }

    #[test]
    fn guest_boot_run_diagnostics_classifies_watchdog_cancellation() {
        let mut diagnostics = GuestBootRunDiagnostics::default();
        let outcome = bangbang_hvf::HvfArm64BootRunLoopOutcome::Stopped { steps: 1 };
        diagnostics.record_step(&bangbang_hvf::HvfVcpuRunStepOutcome::Canceled);
        diagnostics.record_loop_outcome(&outcome);
        diagnostics.finish(
            std::time::Duration::from_secs(30),
            false,
            true,
            true,
            0,
            Some(&outcome),
        );

        assert_eq!(
            diagnostics.timeout_classification(),
            "watchdog-canceled-in-flight-vcpu-run"
        );
        assert_eq!(
            diagnostics.terminal_outcome.as_deref(),
            Some("Stopped { steps: 1 }")
        );
    }

    #[test]
    fn guest_boot_failure_report_includes_boot_and_run_context() {
        let boot = GuestBootDiagnostics {
            kernel_path: "/tmp/vmlinux".into(),
            initrd_path: "/tmp/initrd.cpio".into(),
            boot_args: super::BOOT_ARGS,
            boot_pc: 0x8020_0000,
            fdt_address: 0x87e0_0000,
            fdt_size: 4096,
            initrd_address: 0x87df_f000,
            initrd_size: 512,
            serial_mmio_base: 0x4000_0000,
            serial_mmio_size: 4096,
            serial_interrupt_line: 32,
        };
        let mut run = GuestBootRunDiagnostics::default();
        run.finish(
            std::time::Duration::from_secs(30),
            false,
            true,
            false,
            0,
            None,
        );

        let report = GuestBootFailureReport::new(&boot, &run).to_string();

        assert!(report.contains("classification: outer-timeout-without-terminal-outcome"));
        assert!(report.contains("kernel path: /tmp/vmlinux"));
        assert!(report.contains("boot args: console=ttyS0 reboot=k panic=1 rdinit=/init"));
        assert!(report.contains("serial: base=0x40000000, size=4096, interrupt_line=32"));
    }

    #[test]
    fn completed_steps_reads_all_run_loop_outcome_variants() {
        assert_eq!(
            run_loop_completed_steps(
                &bangbang_hvf::HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 7 }
            ),
            7
        );
        assert_eq!(
            run_loop_completed_steps(&bangbang_hvf::HvfArm64BootRunLoopOutcome::Stopped {
                steps: 2
            }),
            2
        );
        assert_eq!(
            run_loop_completed_steps(&bangbang_hvf::HvfArm64BootRunLoopOutcome::Canceled {
                steps: 3
            }),
            3
        );
        assert_eq!(
            run_loop_completed_steps(&bangbang_hvf::HvfArm64BootRunLoopOutcome::Unknown {
                steps: 4,
                reason: 99
            }),
            4
        );
    }

    #[test]
    fn marker_match_accepts_tty_crlf_translation() {
        assert!(bytes_contain_marker(b"BANGBANG_BOOT_OK\r\n"));
    }
}
