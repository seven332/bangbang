use std::any::Any;
use std::fmt;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{self, Thread};
use std::time::Duration;

use bangbang_runtime::logger::{EmergencyLogger, PanicLogRecords};

const FALLBACK_POLL_INTERVAL: Duration = Duration::from_millis(100);
const FALLBACK_ARMED: u8 = 0;
const FALLBACK_PENDING: u8 = 1;
const FALLBACK_CLAIMED: u8 = 2;
const FALLBACK_CLOSED: u8 = 3;

type PanicPayload = Box<dyn Any + Send + 'static>;
type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

struct FallbackIngress {
    state: AtomicU8,
    shutdown: AtomicBool,
    records: PanicLogRecords,
}

impl fmt::Debug for FallbackIngress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FallbackIngress")
            .field("state", &self.state.load(Ordering::Relaxed))
            .field("shutdown", &self.shutdown.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl FallbackIngress {
    fn new(records: PanicLogRecords) -> Self {
        Self {
            state: AtomicU8::new(FALLBACK_ARMED),
            shutdown: AtomicBool::new(false),
            records,
        }
    }

    /// Makes one publication attempt and never retries.
    fn publish_once(&self) -> bool {
        self.state
            .compare_exchange(
                FALLBACK_ARMED,
                FALLBACK_PENDING,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    fn claim(&self) -> Option<&[u8]> {
        self.state
            .compare_exchange(
                FALLBACK_PENDING,
                FALLBACK_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()?;
        Some(self.records.plain_bytes())
    }

    fn close_if_idle(&self) -> bool {
        match self.state.compare_exchange(
            FALLBACK_ARMED,
            FALLBACK_CLOSED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => true,
            Err(FALLBACK_CLAIMED | FALLBACK_CLOSED) => true,
            Err(_) => false,
        }
    }

    fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
    }

    fn shutdown_requested(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    fn force_close(&self) {
        self.state.store(FALLBACK_CLOSED, Ordering::Release);
    }
}

struct FallbackWorkerGuard(Arc<FallbackIngress>);

impl Drop for FallbackWorkerGuard {
    fn drop(&mut self) {
        self.0.force_close();
    }
}

struct FallbackWorker {
    ingress: Arc<FallbackIngress>,
    thread: Thread,
}

impl fmt::Debug for FallbackWorker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FallbackWorker")
            .field("ingress", &self.ingress)
            .finish_non_exhaustive()
    }
}

impl FallbackWorker {
    fn spawn(writer: impl Write + Send + 'static) -> Result<Self, std::io::ErrorKind> {
        let ingress = Arc::new(FallbackIngress::new(PanicLogRecords::new()));
        let worker_ingress = ingress.clone();
        let handle = thread::Builder::new()
            .name("bangbang-panic-stderr".to_owned())
            .spawn(move || run_fallback_worker(worker_ingress, writer))
            .map_err(|error| error.kind())?;
        let worker_thread = handle.thread().clone();
        drop(handle);
        Ok(Self {
            ingress,
            thread: worker_thread,
        })
    }

    fn publish_once(&self) -> bool {
        self.ingress.publish_once()
    }

    fn nudge(&self) {
        self.thread.unpark();
    }

    fn shutdown(&self) {
        self.ingress.request_shutdown();
        self.thread.unpark();
    }
}

fn run_fallback_worker(ingress: Arc<FallbackIngress>, mut writer: impl Write) {
    let _guard = FallbackWorkerGuard(ingress.clone());
    loop {
        deliver_fallback(&mut writer, &ingress);
        if ingress.shutdown_requested() {
            while !ingress.close_if_idle() {
                deliver_fallback(&mut writer, &ingress);
                thread::yield_now();
            }
            return;
        }
        thread::park_timeout(FALLBACK_POLL_INTERVAL);
    }
}

fn deliver_fallback(writer: &mut impl Write, ingress: &FallbackIngress) {
    let Some(record) = ingress.claim() else {
        return;
    };
    match writer.write(record) {
        Ok(written) if written == record.len() => {
            let _ = writer.flush();
        }
        Ok(_) | Err(_) => {}
    }
}

struct PanicBridgeState {
    first_panic_seen: AtomicBool,
    emergency_logger: OnceLock<EmergencyLogger>,
    fallback: Option<FallbackWorker>,
}

impl fmt::Debug for PanicBridgeState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PanicBridgeState")
            .field(
                "first_panic_seen",
                &self.first_panic_seen.load(Ordering::Relaxed),
            )
            .field(
                "emergency_logger_attached",
                &self.emergency_logger.get().is_some(),
            )
            .field("fallback", &self.fallback)
            .finish()
    }
}

impl PanicBridgeState {
    fn invoke(&self) {
        if self.first_panic_seen.swap(true, Ordering::AcqRel) {
            return;
        }
        if self
            .emergency_logger
            .get()
            .is_some_and(EmergencyLogger::try_log_panic)
        {
            return;
        }
        if let Some(fallback) = &self.fallback {
            let _ = fallback.publish_once();
        }
    }

    fn nudge_fallback(&self) {
        if let Some(fallback) = &self.fallback {
            fallback.nudge();
        }
    }

    fn shutdown_fallback(&self) {
        if let Some(fallback) = &self.fallback {
            fallback.shutdown();
        }
    }
}

/// Executable-owned exclusive panic-hook interval.
#[must_use = "the prior hook must be restored explicitly"]
pub(crate) struct PanicBridge {
    state: Arc<PanicBridgeState>,
    prior_hook: PanicHook,
}

impl fmt::Debug for PanicBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PanicBridge")
            .field("state", &self.state)
            .field("prior_hook", &"<retained>")
            .finish()
    }
}

impl PanicBridge {
    pub(crate) fn install() -> Self {
        Self::install_with_writer(io::stderr())
    }

    fn install_with_writer(writer: impl Write + Send + 'static) -> Self {
        let fallback = FallbackWorker::spawn(writer).ok();
        let state = Arc::new(PanicBridgeState {
            first_panic_seen: AtomicBool::new(false),
            emergency_logger: OnceLock::new(),
            fallback,
        });
        let hook_state = state.clone();
        let hook: PanicHook = Box::new(move |_info| hook_state.invoke());
        let prior_hook = panic::take_hook();
        panic::set_hook(hook);
        Self { state, prior_hook }
    }

    pub(crate) fn attach(&self, logger: EmergencyLogger) -> bool {
        self.state.emergency_logger.set(logger).is_ok()
    }

    pub(crate) fn nudge_fallback(&self) {
        self.state.nudge_fallback();
    }

    pub(crate) fn restore(self) {
        self.state.shutdown_fallback();
        panic::set_hook(self.prior_hook);
    }
}

/// Runs suspect cleanup without allowing a secondary payload to replace the original.
pub(crate) fn isolate_secondary_panic(operation: impl FnOnce()) {
    if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(operation)) {
        std::mem::forget(payload);
    }
}

pub(crate) fn resume_original(payload: PanicPayload) -> ! {
    panic::resume_unwind(payload)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Error, ErrorKind, Write};
    use std::path::PathBuf;
    use std::process::{Command, Output, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use bangbang_runtime::VmmController;
    use bangbang_runtime::logger::{
        LoggerConfigInput, LoggerLevel, PanicLogRecords, ProcessTerminalCategory,
    };

    use super::{
        FallbackIngress, PanicBridge, PanicBridgeState, deliver_fallback, isolate_secondary_panic,
        resume_original,
    };

    const CHILD_CASE_ENV: &str = "BANGBANG_PANIC_BRIDGE_CHILD_CASE";
    static NEXT_PATH_ID: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("shared output lock should succeed")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
            Err(Error::from(ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct WriterGateState {
        entered: bool,
        released: bool,
    }

    #[derive(Debug, Default)]
    struct WriterGate {
        state: Mutex<WriterGateState>,
        changed: Condvar,
    }

    impl WriterGate {
        fn wait_until_entered(&self) {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut state = self.state.lock().expect("gate lock should succeed");
            while !state.entered {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "fallback writer should enter");
                let (next, timeout) = self
                    .changed
                    .wait_timeout(state, remaining)
                    .expect("gate wait should succeed");
                state = next;
                assert!(!timeout.timed_out() || state.entered);
            }
        }

        fn release(&self) {
            self.state
                .lock()
                .expect("gate lock should succeed")
                .released = true;
            self.changed.notify_all();
        }
    }

    #[derive(Debug)]
    struct HeldWriter(Arc<WriterGate>);

    impl Write for HeldWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let mut state = self.0.state.lock().expect("gate lock should succeed");
            state.entered = true;
            self.0.changed.notify_all();
            while !state.released {
                state = self
                    .0
                    .changed
                    .wait(state)
                    .expect("gate wait should succeed");
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct SecretPayload(&'static str);

    struct PanicOnDropPayload(Arc<AtomicUsize>);

    impl Drop for PanicOnDropPayload {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
            panic!("secondary-destructor-secret");
        }
    }

    fn unique_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should follow epoch")
            .as_nanos();
        let id = NEXT_PATH_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "bangbang-panic-bridge-{}-{nanos}-{id}-{name}",
            std::process::id()
        ))
    }

    fn wait_for_output(output: &Arc<Mutex<Vec<u8>>>) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while output
            .lock()
            .expect("output lock should succeed")
            .is_empty()
            && Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        !output
            .lock()
            .expect("output lock should succeed")
            .is_empty()
    }

    fn run_default_redaction_case() {
        let harness_hook = std::panic::take_hook();
        let output = Arc::new(Mutex::new(Vec::new()));
        let bridge = PanicBridge::install_with_writer(SharedWriter(output.clone()));
        let payload = std::panic::catch_unwind(|| panic!("ordinary-secret-payload"))
            .expect_err("panic should be caught");
        bridge.nudge_fallback();
        let delivered = wait_for_output(&output);
        bridge.restore();
        std::panic::set_hook(harness_hook);

        assert!(delivered);
        assert_eq!(
            *payload
                .downcast::<&'static str>()
                .expect("original string payload should resume"),
            "ordinary-secret-payload"
        );
        assert_eq!(
            *output.lock().expect("output lock should succeed"),
            b"event=process-panic\n"
        );
    }

    fn run_sentinel_restore_case() {
        let harness_hook = std::panic::take_hook();
        let sentinel_hits = Arc::new(AtomicUsize::new(0));
        let hook_hits = sentinel_hits.clone();
        std::panic::set_hook(Box::new(move |_info| {
            hook_hits.fetch_add(1, Ordering::AcqRel);
        }));
        let bridge = PanicBridge::install_with_writer(std::io::sink());
        let expected = Arc::new(SecretPayload("custom-secret-payload"));
        let thrown = expected.clone();
        let payload = std::panic::catch_unwind(move || std::panic::panic_any(thrown))
            .expect_err("custom panic should be caught");
        let owned_hits = sentinel_hits.load(Ordering::Acquire);
        let payload =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| resume_original(payload)))
                .expect_err("resumed payload should unwind to the harness catch");
        bridge.restore();
        let _ = std::panic::catch_unwind(|| panic!("sentinel-probe"));
        let restored_hits = sentinel_hits.load(Ordering::Acquire);
        std::panic::set_hook(harness_hook);

        let resumed = payload
            .downcast::<Arc<SecretPayload>>()
            .expect("custom payload should retain its type");
        assert!(Arc::ptr_eq(&expected, &resumed));
        assert_eq!(resumed.0, "custom-secret-payload");
        assert_eq!(owned_hits, 0);
        assert_eq!(restored_hits, 1);
    }

    fn run_attached_logger_case() {
        let fallback = Arc::new(Mutex::new(Vec::new()));
        let bridge = PanicBridge::install_with_writer(SharedWriter(fallback.clone()));
        let path = unique_path("attached");
        let mut controller = VmmController::new("demo", "0.1.0", "bangbang");
        assert!(bridge.attach(controller.emergency_logger()));
        let config = controller
            .prepare_logger_config(LoggerConfigInput::new().with_log_path(&path))
            .expect("logger config should validate");
        let prepared = controller
            .prepare_logger_update(config, None)
            .expect("logger update should prepare");
        controller
            .commit_logger_config(prepared)
            .expect("logger should configure");

        let payload = std::panic::catch_unwind(|| panic!("configured-secret"))
            .expect_err("panic should be caught");
        controller.settle_emergency_logger_loss();
        assert!(controller.log_process_terminal(ProcessTerminalCategory::Panic));
        drop(payload);
        bridge.restore();
        drop(controller);

        assert_eq!(
            fs::read_to_string(&path).expect("logger output should be readable"),
            "event=process-panic\nevent=process-exit category=panic\n"
        );
        assert!(
            fallback
                .lock()
                .expect("fallback lock should succeed")
                .is_empty()
        );
        fs::remove_file(path).expect("logger fixture should clean up");
    }

    fn run_occupied_logger_fallback_case() {
        let fallback = Arc::new(Mutex::new(Vec::new()));
        let bridge = PanicBridge::install_with_writer(SharedWriter(fallback.clone()));
        let path = unique_path("occupied");
        let mut controller = VmmController::new("demo", "0.1.0", "bangbang");
        let emergency = controller.emergency_logger();
        assert!(bridge.attach(emergency.clone()));
        let config = controller
            .prepare_logger_config(LoggerConfigInput::new().with_log_path(&path))
            .expect("logger config should validate");
        let prepared = controller
            .prepare_logger_update(config, None)
            .expect("logger update should prepare");
        controller
            .commit_logger_config(prepared)
            .expect("logger should configure");
        assert!(emergency.try_log_panic());

        let payload = std::panic::catch_unwind(|| panic!("occupied-logger-secret"))
            .expect_err("panic should be caught");
        bridge.nudge_fallback();
        let delivered = wait_for_output(&fallback);
        assert!(controller.log_process_terminal(ProcessTerminalCategory::Panic));
        drop(payload);
        bridge.restore();
        drop(controller);

        assert!(delivered);
        assert_eq!(
            *fallback.lock().expect("fallback lock should succeed"),
            b"event=process-panic\n"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("logger output should be readable"),
            "event=process-panic\nevent=process-exit category=panic\n"
        );
        fs::remove_file(path).expect("logger fixture should clean up");
    }

    fn run_filtered_fallback_case() {
        let fallback = Arc::new(Mutex::new(Vec::new()));
        let bridge = PanicBridge::install_with_writer(SharedWriter(fallback.clone()));
        let path = unique_path("filtered");
        let mut controller = VmmController::new("demo", "0.1.0", "bangbang");
        assert!(bridge.attach(controller.emergency_logger()));
        let config = controller
            .prepare_logger_config(
                LoggerConfigInput::new()
                    .with_log_path(&path)
                    .with_level(LoggerLevel::Off),
            )
            .expect("logger config should validate");
        let prepared = controller
            .prepare_logger_update(config, None)
            .expect("logger update should prepare");
        controller
            .commit_logger_config(prepared)
            .expect("logger should configure");

        let payload = std::panic::catch_unwind(|| panic!("filtered-secret"))
            .expect_err("panic should be caught");
        bridge.nudge_fallback();
        let delivered = wait_for_output(&fallback);
        drop(payload);
        bridge.restore();
        drop(controller);

        assert!(delivered);
        assert_eq!(
            *fallback.lock().expect("fallback lock should succeed"),
            b"event=process-panic\n"
        );
        assert_eq!(
            fs::read_to_string(&path).expect("logger output should be readable"),
            ""
        );
        fs::remove_file(path).expect("logger fixture should clean up");
    }

    fn run_second_hook_silence_case() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let bridge = PanicBridge::install_with_writer(SharedWriter(output.clone()));
        let first = std::panic::catch_unwind(|| panic!("first-secret"));
        let second = std::panic::catch_unwind(|| panic!("second-secret"));
        bridge.nudge_fallback();
        let delivered = wait_for_output(&output);
        drop(first);
        drop(second);
        bridge.restore();

        assert!(delivered);
        assert_eq!(
            *output.lock().expect("output lock should succeed"),
            b"event=process-panic\n"
        );
    }

    fn run_stalled_fallback_case() {
        let gate = Arc::new(WriterGate::default());
        let bridge = PanicBridge::install_with_writer(HeldWriter(gate.clone()));
        let caught = std::panic::catch_unwind(|| panic!("stalled-secret"));
        assert!(
            caught.is_err(),
            "hook must return before fallback writer runs"
        );
        bridge.nudge_fallback();
        gate.wait_until_entered();
        gate.release();
        drop(caught);
        bridge.restore();
    }

    fn run_secondary_isolation_case() {
        let bridge = PanicBridge::install_with_writer(std::io::sink());
        let payload_drops = Arc::new(AtomicUsize::new(0));
        let thrown = payload_drops.clone();

        isolate_secondary_panic(|| std::panic::panic_any(PanicOnDropPayload(thrown)));

        assert_eq!(payload_drops.load(Ordering::Acquire), 0);
        bridge.restore();
    }

    struct DoublePanicGuard;

    impl Drop for DoublePanicGuard {
        fn drop(&mut self) {
            panic!("secondary-double-panic-secret");
        }
    }

    fn run_double_panic_case() -> ! {
        let _bridge = PanicBridge::install_with_writer(std::io::sink());
        let _guard = DoublePanicGuard;
        panic!("primary-double-panic-secret");
    }

    #[test]
    fn panic_bridge_child() {
        let Ok(case) = std::env::var(CHILD_CASE_ENV) else {
            return;
        };
        match case.as_str() {
            "default-redaction" => run_default_redaction_case(),
            "sentinel-restore" => run_sentinel_restore_case(),
            "attached-logger" => run_attached_logger_case(),
            "filtered-fallback" => run_filtered_fallback_case(),
            "occupied-logger-fallback" => run_occupied_logger_fallback_case(),
            "second-hook-silence" => run_second_hook_silence_case(),
            "stalled-fallback" => run_stalled_fallback_case(),
            "secondary-isolation" => run_secondary_isolation_case(),
            "double-panic" => run_double_panic_case(),
            _ => panic!("unknown panic bridge child case"),
        }
    }

    fn spawn_child(case: &str) -> Output {
        let mut child =
            Command::new(std::env::current_exe().expect("test executable should exist"))
                .args([
                    "--exact",
                    "panic_bridge::tests::panic_bridge_child",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD_CASE_ENV, case)
                .env("RUST_BACKTRACE", "0")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("panic bridge child should spawn");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child
                .try_wait()
                .expect("child status should be observable")
                .is_some()
            {
                return child
                    .wait_with_output()
                    .expect("child output should be collected");
            }
            if Instant::now() >= deadline {
                child.kill().expect("timed-out child should be killed");
                let output = child
                    .wait_with_output()
                    .expect("timed-out child output should be collected");
                panic!(
                    "panic bridge child {case} timed out: stdout={:?} stderr={:?}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::yield_now();
        }
    }

    fn assert_success_without_secrets(case: &str, secrets: &[&str]) {
        let output = spawn_child(case);
        assert!(
            output.status.success(),
            "case {case} failed: stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        for secret in secrets {
            assert!(!stderr.contains(secret), "stderr exposed {secret:?}");
        }
    }

    #[test]
    fn subprocess_redacts_payloads_and_restores_prior_hooks() {
        assert_success_without_secrets("default-redaction", &["ordinary-secret-payload"]);
        assert_success_without_secrets("sentinel-restore", &["custom-secret-payload"]);
    }

    #[test]
    fn subprocess_routes_logger_then_fixed_fallback() {
        assert_success_without_secrets("attached-logger", &["configured-secret"]);
        assert_success_without_secrets("filtered-fallback", &["filtered-secret"]);
        assert_success_without_secrets("occupied-logger-fallback", &["occupied-logger-secret"]);
    }

    #[test]
    fn subprocess_bounds_second_hook_and_stalled_fallback() {
        assert_success_without_secrets("second-hook-silence", &["first-secret", "second-secret"]);
        assert_success_without_secrets("stalled-fallback", &["stalled-secret"]);
        assert_success_without_secrets("secondary-isolation", &["secondary-destructor-secret"]);
    }

    #[test]
    fn fallback_ingress_has_one_attempt_and_closes_after_all_outcomes() {
        let admitted = FallbackIngress::new(PanicLogRecords::new());
        assert!(admitted.publish_once());
        assert!(!admitted.publish_once());
        let mut output = Vec::new();
        deliver_fallback(&mut output, &admitted);
        assert_eq!(output, b"event=process-panic\n");
        assert!(admitted.close_if_idle());

        let failed = FallbackIngress::new(PanicLogRecords::new());
        assert!(failed.publish_once());
        deliver_fallback(&mut FailingWriter, &failed);
        assert!(failed.close_if_idle());

        let closed = FallbackIngress::new(PanicLogRecords::new());
        closed.force_close();
        assert!(!closed.publish_once());
        assert!(closed.close_if_idle());
    }

    #[test]
    fn unavailable_fallback_returns_after_the_first_attempt() {
        let state = PanicBridgeState {
            first_panic_seen: AtomicBool::new(false),
            emergency_logger: std::sync::OnceLock::new(),
            fallback: None,
        };

        state.invoke();
        state.invoke();

        assert!(state.first_panic_seen.load(Ordering::Acquire));
    }

    #[test]
    fn subprocess_double_panic_is_bounded_and_redacted() {
        let output = spawn_child("double-panic");
        assert!(!output.status.success(), "double panic must terminate");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("primary-double-panic-secret"));
        assert!(!stderr.contains("secondary-double-panic-secret"));
    }
}
