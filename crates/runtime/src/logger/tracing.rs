use std::cell::RefCell;
use std::env;
use std::fmt;
use std::marker::PhantomData;
use std::panic::Location;
use std::rc::Rc;

use super::delivery::{LoggerDeliveryConfig, LoggerProducer, PreparedLoggerWriter};
use super::event::{LogBatch, LogOrigin, LogRecord, TracePhase};
use super::{GuestLogger, LoggerLevel, LoggerState, module_filter_allows};

/// Maximum number of simultaneously active developer-tracing scopes per
/// thread.
pub const MAX_TRACE_DEPTH: usize = 32;

const TOOL_TRACE_ENVIRONMENT: &str = "BANGBANG_TRACE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraceDelivery {
    BoundedHost,
    NonblockingGuest,
}

/// Narrow, cloneable developer-tracing admission capability.
///
/// Values are snapshots of the configured logger. They expose no writer,
/// mutable configuration, generic field, retry, or delivery receipt.
#[derive(Clone)]
pub struct TraceLogger {
    producer: Option<LoggerProducer>,
    level: LoggerLevel,
    show_level: bool,
    show_log_origin: bool,
    module_filter: Option<String>,
    delivery: TraceDelivery,
}

impl Default for TraceLogger {
    fn default() -> Self {
        Self {
            producer: None,
            level: LoggerLevel::Off,
            show_level: false,
            show_log_origin: false,
            module_filter: None,
            delivery: TraceDelivery::BoundedHost,
        }
    }
}

impl fmt::Debug for TraceLogger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceLogger")
            .field("producer", &self.producer.as_ref().map(|_| "<owned>"))
            .field("level", &self.level)
            .field("show_level", &self.show_level)
            .field("show_log_origin", &self.show_log_origin)
            .field(
                "module_filter",
                &self.module_filter.as_ref().map(|_| "<redacted>"),
            )
            .field("delivery", &self.delivery)
            .finish()
    }
}

impl TraceLogger {
    fn from_logger_state(state: &LoggerState, delivery: TraceDelivery) -> Self {
        Self {
            producer: state.delivery.as_ref().map(|output| output.producer()),
            level: state.level,
            show_level: state.show_level,
            show_log_origin: state.show_log_origin,
            module_filter: state.module.clone(),
            delivery,
        }
    }

    fn from_guest_logger(logger: &GuestLogger) -> Self {
        Self {
            producer: logger.inner.producer.clone(),
            level: logger.inner.level,
            show_level: logger.inner.show_level,
            show_log_origin: logger.inner.show_log_origin,
            module_filter: logger.inner.module.clone(),
            delivery: TraceDelivery::NonblockingGuest,
        }
    }

    /// Enters one fixed trace scope.
    ///
    /// Callers should use [`crate::bangbang_trace_scope!`] so default builds
    /// remove this call and its logger expression completely.
    #[doc(hidden)]
    #[track_caller]
    pub fn enter_fixed(&self, module: &'static str, scope: &'static str) -> TraceScope {
        if module.is_empty()
            || scope.is_empty()
            || self.producer.is_none()
            || !self.level.allows(LoggerLevel::Trace)
            || !module_filter_allows(self.module_filter.as_deref(), module)
        {
            return TraceScope::inactive();
        }

        let pushed = TRACE_STACK
            .try_with(|stack| {
                let mut stack = stack.try_borrow_mut().ok()?;
                stack.push(scope)
            })
            .ok()
            .flatten();
        let Some((depth, path)) = pushed else {
            return TraceScope::inactive();
        };

        let origin = Location::caller();
        self.emit(module, origin, path, TracePhase::Enter);
        TraceScope {
            active: Some(ActiveTraceScope {
                logger: self.clone(),
                module,
                scope,
                depth,
                origin,
            }),
            not_send: PhantomData,
        }
    }

    fn emit(
        &self,
        module: &'static str,
        origin: &'static Location<'static>,
        path: TracePath,
        phase: TracePhase,
    ) {
        let Some(producer) = &self.producer else {
            return;
        };
        let record = LogRecord::encode_trace(
            self.show_level,
            self.show_log_origin,
            LogOrigin::from(origin),
            module,
            std::thread::current().id(),
            path.as_slice(),
            phase,
        );
        let batch = LogBatch::one(record);
        let _delivered = match self.delivery {
            TraceDelivery::BoundedHost => producer.deliver_host(batch),
            TraceDelivery::NonblockingGuest => producer.deliver_nonblocking(batch),
        };
    }

    #[cfg(test)]
    pub(crate) fn wait_for_delivery_for_test(&self) -> bool {
        self.producer
            .as_ref()
            .is_none_or(LoggerProducer::wait_for_idle_for_test)
    }
}

/// One active developer-tracing scope.
///
/// The guard is deliberately not `Send`; dropping it on another thread would
/// corrupt per-thread nesting. Its destructor never propagates logger or
/// thread-local failures.
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<bangbang_runtime::logger::TraceScope>();
/// ```
pub struct TraceScope {
    active: Option<ActiveTraceScope>,
    not_send: PhantomData<Rc<()>>,
}

impl TraceScope {
    fn inactive() -> Self {
        Self {
            active: None,
            not_send: PhantomData,
        }
    }
}

impl fmt::Debug for TraceScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceScope")
            .field("active", &self.active.is_some())
            .finish()
    }
}

impl Drop for TraceScope {
    fn drop(&mut self) {
        let Some(active) = self.active.take() else {
            return;
        };
        let path = TRACE_STACK
            .try_with(|stack| {
                let mut stack = stack.try_borrow_mut().ok()?;
                stack.close(active.depth, active.scope)
            })
            .ok()
            .flatten();
        if let Some(path) = path {
            active
                .logger
                .emit(active.module, active.origin, path, TracePhase::Exit);
        }
    }
}

struct ActiveTraceScope {
    logger: TraceLogger,
    module: &'static str,
    scope: &'static str,
    depth: usize,
    origin: &'static Location<'static>,
}

#[derive(Clone, Copy)]
struct TracePath {
    scopes: [&'static str; MAX_TRACE_DEPTH],
    len: usize,
}

impl TracePath {
    fn as_slice(&self) -> &[&str] {
        self.scopes.get(..self.len).unwrap_or_default()
    }
}

struct TraceStack {
    scopes: [&'static str; MAX_TRACE_DEPTH],
    len: usize,
}

impl TraceStack {
    const fn new() -> Self {
        Self {
            scopes: [""; MAX_TRACE_DEPTH],
            len: 0,
        }
    }

    fn push(&mut self, scope: &'static str) -> Option<(usize, TracePath)> {
        let depth = self.len;
        let slot = self.scopes.get_mut(depth)?;
        *slot = scope;
        self.len += 1;
        Some((depth, self.path_through(depth)?))
    }

    fn close(&mut self, depth: usize, scope: &'static str) -> Option<TracePath> {
        if self.scopes.get(depth).copied() != Some(scope) || depth >= self.len {
            return None;
        }
        let path = self.path_through(depth)?;
        for slot in self.scopes.get_mut(depth..self.len)? {
            *slot = "";
        }
        self.len = depth;
        Some(path)
    }

    fn path_through(&self, depth: usize) -> Option<TracePath> {
        let len = depth.checked_add(1)?;
        if len > self.len {
            return None;
        }
        Some(TracePath {
            scopes: self.scopes,
            len,
        })
    }
}

thread_local! {
    static TRACE_STACK: RefCell<TraceStack> = const { RefCell::new(TraceStack::new()) };
}

/// Feature-only standalone-tool tracing owner.
///
/// The session is disabled unless `BANGBANG_TRACE` is `*` or a nonempty module
/// prefix. It owns a bounded worker backed by stderr until every scope using
/// its logger has emitted its exit record.
pub struct ToolTraceSession {
    state: LoggerState,
}

impl ToolTraceSession {
    /// Constructs a standalone-tool trace session from the diagnostic
    /// environment.
    #[doc(hidden)]
    pub fn from_environment() -> Self {
        let mut state = LoggerState::default();
        let Some(module_filter) = tool_module_filter() else {
            return Self { state };
        };

        state.delivery_config = LoggerDeliveryConfig::for_tool_tracing();
        if state
            .commit_writer(PreparedLoggerWriter::new(std::io::stderr()))
            .is_ok()
        {
            state.level = LoggerLevel::Trace;
            state.show_level = true;
            state.module = module_filter;
        }
        Self { state }
    }

    /// Returns the bounded-host trace logger owned by this session.
    #[doc(hidden)]
    pub fn trace_logger(&self) -> TraceLogger {
        TraceLogger::from_logger_state(&self.state, TraceDelivery::BoundedHost)
    }
}

impl fmt::Debug for ToolTraceSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolTraceSession")
            .field("enabled", &self.state.delivery.is_some())
            .finish()
    }
}

fn tool_module_filter() -> Option<Option<String>> {
    let value = env::var_os(TOOL_TRACE_ENVIRONMENT)?;
    let value = value.to_str()?;
    if value == "*" {
        Some(None)
    } else if value.is_empty() {
        None
    } else {
        Some(Some(value.to_owned()))
    }
}

impl LoggerState {
    /// Returns a bounded-host developer-tracing snapshot.
    pub fn trace_logger(&self) -> TraceLogger {
        TraceLogger::from_logger_state(self, TraceDelivery::BoundedHost)
    }
}

impl GuestLogger {
    /// Returns a nonblocking guest/device developer-tracing snapshot.
    pub fn trace_logger(&self) -> TraceLogger {
        TraceLogger::from_guest_logger(self)
    }
}

#[cfg(test)]
mod tests {
    use std::hint::black_box;
    use std::sync::{Arc, Barrier};
    use std::time::Instant;

    use super::*;
    use crate::logger::{LoggerConfigInput, LoggerTestCapture};

    fn configured_trace_logger(
        level: LoggerLevel,
        module: Option<&str>,
        show_log_origin: bool,
    ) -> (LoggerState, LoggerTestCapture, TraceLogger) {
        let capture = LoggerTestCapture::default();
        let mut state = LoggerState::default();
        state.configure_test_writer(capture.clone());
        let mut input = LoggerConfigInput::new()
            .with_level(level)
            .with_show_log_origin(show_log_origin);
        if let Some(module) = module {
            input = input.with_module(module);
        }
        state
            .configure(input)
            .expect("trace logger configuration should succeed");
        let logger = state.trace_logger();
        (state, capture, logger)
    }

    fn without_thread_identity(line: &str) -> String {
        let Some((prefix, rest)) = line.split_once(" thread=") else {
            return line.to_owned();
        };
        let Some((_, suffix)) = rest.split_once(" scope=") else {
            return line.to_owned();
        };
        let prefix = prefix
            .strip_prefix("origin=")
            .and_then(|prefix| prefix.split_once(" trace module="))
            .and_then(|(origin, module)| {
                origin
                    .rsplit_once(':')
                    .map(|(path, _)| format!("origin={path}:<line> trace module={module}"))
            })
            .unwrap_or_else(|| prefix.to_owned());
        format!("{prefix} scope={suffix}")
    }

    fn normalized_lines(output: &str) -> Vec<String> {
        output.lines().map(without_thread_identity).collect()
    }

    fn return_from_traced_scope(logger: &TraceLogger, value: u32) -> Result<u32, &'static str> {
        let _scope = logger.enter_fixed("test::trace", "early-return");
        if value == 7 {
            return Ok(value);
        }
        Err("unexpected value")
    }

    #[test]
    fn records_nested_entry_exit_and_normalized_origin() {
        let (_state, capture, logger) =
            configured_trace_logger(LoggerLevel::Trace, Some("test::"), true);
        {
            let _outer = logger.enter_fixed("test::trace", "outer");
            {
                let _inner = logger.enter_fixed("test::trace", "inner");
            }
        }
        assert!(logger.wait_for_delivery_for_test());

        let output = capture.output();
        assert_eq!(
            normalized_lines(&output),
            [
                "origin=crates/runtime/src/logger/tracing.rs:<line> trace module=test::trace scope=outer phase=enter",
                "origin=crates/runtime/src/logger/tracing.rs:<line> trace module=test::trace scope=outer::inner phase=enter",
                "origin=crates/runtime/src/logger/tracing.rs:<line> trace module=test::trace scope=outer::inner phase=exit",
                "origin=crates/runtime/src/logger/tracing.rs:<line> trace module=test::trace scope=outer phase=exit",
            ]
        );
        assert!(!output.contains("/Users/"));
    }

    #[test]
    fn early_return_emits_exit_without_changing_the_result() {
        let (_state, capture, logger) = configured_trace_logger(LoggerLevel::Trace, None, false);

        assert_eq!(return_from_traced_scope(&logger, 7), Ok(7));
        assert!(logger.wait_for_delivery_for_test());

        assert_eq!(
            normalized_lines(&capture.output()),
            [
                "trace module=test::trace scope=early-return phase=enter",
                "trace module=test::trace scope=early-return phase=exit",
            ]
        );
    }

    #[test]
    fn applies_level_and_module_filters_before_stack_work() {
        let (_info_state, info_capture, info_logger) =
            configured_trace_logger(LoggerLevel::Info, None, false);
        let (_module_state, module_capture, module_logger) =
            configured_trace_logger(LoggerLevel::Trace, Some("allowed::"), false);

        {
            let _scope = info_logger.enter_fixed("allowed::trace", "info-filtered");
        }
        {
            let _scope = module_logger.enter_fixed("blocked::trace", "module-filtered");
        }
        assert!(info_logger.wait_for_delivery_for_test());
        assert!(module_logger.wait_for_delivery_for_test());
        assert!(info_capture.output().is_empty());
        assert!(module_capture.output().is_empty());

        {
            let _scope = module_logger.enter_fixed("allowed::trace", "admitted");
        }
        assert!(module_logger.wait_for_delivery_for_test());
        assert_eq!(module_capture.output().lines().count(), 2);
    }

    #[test]
    fn unwind_and_forgotten_inner_scope_leave_a_clean_stack() {
        let (_state, capture, logger) = configured_trace_logger(LoggerLevel::Trace, None, false);

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _scope = logger.enter_fixed("test::trace", "unwind");
            panic!("expected tracing unwind");
        }));
        assert!(unwind.is_err());

        {
            let outer = logger.enter_fixed("test::trace", "forgotten-outer");
            let inner = logger.enter_fixed("test::trace", "forgotten-inner");
            std::mem::forget(inner);
            drop(outer);
        }
        {
            let _clean = logger.enter_fixed("test::trace", "clean");
        }
        assert!(logger.wait_for_delivery_for_test());

        let lines = normalized_lines(&capture.output());
        assert!(lines.contains(&"trace module=test::trace scope=unwind phase=exit".to_owned()));
        assert!(
            lines.contains(&"trace module=test::trace scope=forgotten-outer phase=exit".to_owned())
        );
        assert!(lines.contains(&"trace module=test::trace scope=clean phase=enter".to_owned()));
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("forgotten-inner::clean"))
        );
    }

    #[test]
    fn overlapping_threads_keep_independent_paths() {
        let capture = LoggerTestCapture::default();
        let mut state = LoggerState::default();
        state.configure_test_writer(capture.clone());
        state
            .configure(LoggerConfigInput::new().with_level(LoggerLevel::Trace))
            .expect("trace logger configuration should succeed");
        let logger = state.guest_logger().trace_logger();
        let barrier = Arc::new(Barrier::new(3));

        let handles = ["alpha", "beta"].map(|scope| {
            let logger = logger.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let _scope = logger.enter_fixed("test::thread", scope);
                barrier.wait();
                barrier.wait();
            })
        });
        barrier.wait();
        barrier.wait();
        for handle in handles {
            handle.join().expect("trace thread should finish");
        }
        assert!(logger.wait_for_delivery_for_test());

        let output = capture.output();
        assert_eq!(output.lines().count(), 4);
        assert!(!output.contains("alpha::beta"));
        assert!(!output.contains("beta::alpha"));
    }

    #[test]
    fn caps_depth_and_record_bytes() {
        const LONG_SCOPE: &str = concat!(
            "secret-free-fixed-scope-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg",
        );

        let capture = LoggerTestCapture::default();
        let mut state = LoggerState::default();
        state.configure_test_writer(capture.clone());
        state
            .configure(LoggerConfigInput::new().with_level(LoggerLevel::Trace))
            .expect("trace logger configuration should succeed");
        let logger = state.guest_logger().trace_logger();

        let mut scopes = Vec::new();
        for _ in 0..=MAX_TRACE_DEPTH {
            scopes.push(logger.enter_fixed("test::depth", "scope"));
        }
        while scopes.pop().is_some() {}
        {
            let _long = logger.enter_fixed("test::record", LONG_SCOPE);
        }
        assert!(logger.wait_for_delivery_for_test());

        let output = capture.output();
        assert_eq!(output.lines().count(), MAX_TRACE_DEPTH * 2 + 2);
        assert!(output.as_bytes().split(|byte| *byte == b'\n').all(|line| {
            line.is_empty() || line.len() < super::super::event::MAX_LOG_RECORD_BYTES
        }));
        assert!(output.contains("... phase="));
        assert!(
            output
                .lines()
                .all(|line| { line.ends_with("phase=enter") || line.ends_with("phase=exit") })
        );
    }

    #[test]
    fn delivery_failure_is_loss_accounted_and_cannot_replace_result() {
        let (state, _capture, logger) = configured_trace_logger(LoggerLevel::Trace, None, false);
        let missed_before = state.metrics.missed_log_count();
        assert!(state.disconnect_delivery_for_test());

        let result: Result<u32, &'static str> = {
            let _scope = logger.enter_fixed("test::failure", "operation");
            Ok(7)
        };
        assert_eq!(result, Ok(7));
        assert_eq!(state.metrics.missed_log_count(), missed_before + 2);
    }

    #[test]
    fn debug_output_redacts_the_module_filter() {
        let (_state, _capture, logger) =
            configured_trace_logger(LoggerLevel::Trace, Some("private-module-filter"), false);
        let debug = format!("{logger:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("private-module-filter"));
    }

    #[test]
    #[ignore = "descriptive release diagnostic; no machine-dependent threshold"]
    fn reports_trace_scope_overhead() {
        const ITERATIONS: u32 = 10_000;

        let disabled = TraceLogger::default();
        let disabled_started = Instant::now();
        for _ in 0..ITERATIONS {
            black_box(disabled.enter_fixed("bench::trace", "disabled"));
        }
        let disabled_elapsed = disabled_started.elapsed();

        let (_filtered_state, _filtered_capture, filtered) =
            configured_trace_logger(LoggerLevel::Info, None, false);
        let filtered_started = Instant::now();
        for _ in 0..ITERATIONS {
            black_box(filtered.enter_fixed("bench::trace", "filtered"));
        }
        let filtered_elapsed = filtered_started.elapsed();

        let mut active_state = LoggerState::default();
        active_state.configure_test_writer(std::io::sink());
        active_state
            .configure(LoggerConfigInput::new().with_level(LoggerLevel::Trace))
            .expect("active trace logger configuration should succeed");
        let active = active_state.guest_logger().trace_logger();
        let active_started = Instant::now();
        for _ in 0..ITERATIONS {
            black_box(active.enter_fixed("bench::trace", "enabled"));
        }
        let active_elapsed = active_started.elapsed();
        assert!(active.wait_for_delivery_for_test());

        let iterations = u128::from(ITERATIONS);
        println!(
            "trace-scope ns/iteration: disabled={} filtered={} enabled-nonblocking={} missed={}",
            disabled_elapsed.as_nanos() / iterations,
            filtered_elapsed.as_nanos() / iterations,
            active_elapsed.as_nanos() / iterations,
            active_state.metrics.missed_log_count(),
        );
    }
}
