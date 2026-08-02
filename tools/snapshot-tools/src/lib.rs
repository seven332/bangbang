//! Shared execution boundaries for Firecracker-shaped snapshot tools.

use std::fmt;
#[cfg(unix)]
use std::fmt::Write as FmtWrite;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use bangbang_hvf::{
    HvfNativeSnapshotDocument, HvfNativeSnapshotRegisterRemovalOutcome,
    HvfNativeSnapshotRegisterRemovalReport, HvfNativeSnapshotRegisterRemovalRequest,
};
#[cfg(target_os = "macos")]
use bangbang_runtime::snapshot_rebase::{
    SnapshotV2DiffRebaseCleanup, SnapshotV2DiffRebaseCommit, SnapshotV2DiffRebaseCommitFailure,
    SnapshotV2DiffRebaseFailure, SnapshotV2DiffRebasePaths, SnapshotV2DiffRebaseStage,
    rebase_snapshot_v2_diff_paths_with_cancel,
};
#[cfg(unix)]
use bangbang_runtime::snapshot_state_edit::{
    SnapshotStateEditCleanup, SnapshotStateEditCommit, SnapshotStateEditCommitFailure,
    SnapshotStateEditFailure, SnapshotStateEditPaths, SnapshotStateEditTransactionError,
    publish_edited_snapshot_state_with_cancel, read_snapshot_state_file_with_cancel,
};
#[cfg(unix)]
use signal_hook::SigId;
#[cfg(unix)]
use signal_hook::consts::signal::{SIGINT, SIGTERM};

const REDACTED: &str = "<redacted>";
const OPERATIONAL_EXIT: u8 = 1;
const COMMITTED_UNCERTAIN_EXIT: u8 = 3;
#[cfg(unix)]
const SIGINT_EXIT: u8 = 128 + SIGINT as u8;
#[cfg(unix)]
const SIGTERM_EXIT: u8 = 128 + SIGTERM as u8;

/// The public command surface requesting a shared rebase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebaseTool {
    /// Firecracker's deprecated standalone command.
    RebaseSnap,
    /// Firecracker's replacement snapshot editor command.
    SnapshotEditor,
}

impl fmt::Display for RebaseTool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::RebaseSnap => "rebase-snap",
            Self::SnapshotEditor => "snapshot-editor",
        })
    }
}

/// One owned base/diff request passed by either command frontend.
#[derive(Clone, PartialEq, Eq)]
pub struct RebaseRequest {
    base: PathBuf,
    diff: PathBuf,
}

impl RebaseRequest {
    /// Creates a request without opening or canonicalizing either path.
    pub fn new(base: impl Into<PathBuf>, diff: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            diff: diff.into(),
        }
    }
}

impl fmt::Debug for RebaseRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RebaseRequest")
            .field("base", &REDACTED)
            .field("diff", &REDACTED)
            .finish()
    }
}

/// Canonical native-state view selected by `info-vmstate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotInfoView {
    /// Exact native document semantic version.
    Version,
    /// Canonical vCPU-state inspection.
    VcpuStates,
    /// Canonical full-VM inspection.
    VmState,
}

#[cfg(unix)]
#[derive(Clone, PartialEq, Eq)]
struct SnapshotInfoRequest {
    view: SnapshotInfoView,
    path: PathBuf,
}

#[cfg(unix)]
impl fmt::Debug for SnapshotInfoRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotInfoRequest")
            .field("view", &self.view)
            .field("path", &REDACTED)
            .finish()
    }
}

#[cfg(unix)]
struct RegisterRemovalRequest {
    reviewed: HvfNativeSnapshotRegisterRemovalRequest,
    paths: SnapshotStateEditPaths,
}

#[cfg(unix)]
impl fmt::Debug for RegisterRemovalRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisterRemovalRequest")
            .field("request_count", &self.reviewed.request_count())
            .field("paths", &self.paths)
            .finish()
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct CancellationState {
    interrupt: Arc<AtomicBool>,
    terminate: Arc<AtomicBool>,
}

#[cfg(unix)]
impl CancellationState {
    fn is_cancelled(&self) -> bool {
        self.interrupt.load(Ordering::Acquire) || self.terminate.load(Ordering::Acquire)
    }

    fn exit_code(&self) -> u8 {
        if self.terminate.load(Ordering::Acquire) {
            SIGTERM_EXIT
        } else {
            SIGINT_EXIT
        }
    }
}

#[cfg(unix)]
struct CancellationSignals {
    state: CancellationState,
    registrations: [SigId; 2],
}

#[cfg(unix)]
impl CancellationSignals {
    fn install() -> io::Result<Self> {
        let (state, registrations) =
            register_signal_flags_with(signal_hook::flag::register, |registration| {
                signal_hook::low_level::unregister(registration);
            })?;
        Ok(Self {
            state,
            registrations,
        })
    }
}

#[cfg(unix)]
impl fmt::Debug for CancellationSignals {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationSignals")
            .field("received", &self.state.is_cancelled())
            .field("registrations", &self.registrations.len())
            .finish()
    }
}

#[cfg(unix)]
impl Drop for CancellationSignals {
    fn drop(&mut self) {
        for registration in self.registrations {
            signal_hook::low_level::unregister(registration);
        }
    }
}

#[cfg(unix)]
fn register_signal_flags_with<T>(
    mut register: impl FnMut(i32, Arc<AtomicBool>) -> io::Result<T>,
    mut unregister: impl FnMut(T),
) -> io::Result<(CancellationState, [T; 2])> {
    let interrupt = Arc::new(AtomicBool::new(false));
    let terminate = Arc::new(AtomicBool::new(false));
    let interrupt_registration = register(SIGINT, Arc::clone(&interrupt))?;
    let terminate_registration = match register(SIGTERM, Arc::clone(&terminate)) {
        Ok(registration) => registration,
        Err(error) => {
            unregister(interrupt_registration);
            return Err(error);
        }
    };
    Ok((
        CancellationState {
            interrupt,
            terminate,
        },
        [interrupt_registration, terminate_registration],
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RebaseExecutionReport {
    exit: u8,
    diagnostic: Option<String>,
}

impl RebaseExecutionReport {
    #[cfg(target_os = "macos")]
    const fn durable() -> Self {
        Self {
            exit: 0,
            diagnostic: None,
        }
    }

    fn operational(tool: RebaseTool, diagnostic: impl fmt::Display) -> Self {
        Self {
            exit: OPERATIONAL_EXIT,
            diagnostic: Some(format!("{tool}: {diagnostic}")),
        }
    }

    #[cfg(target_os = "macos")]
    fn cancelled(tool: RebaseTool, signals: &CancellationState) -> Self {
        Self {
            exit: signals.exit_code(),
            diagnostic: Some(format!(
                "{tool}: native-v2 Diff rebase cancelled before commit"
            )),
        }
    }

    #[cfg(target_os = "macos")]
    fn committed_uncertain(
        tool: RebaseTool,
        stage: SnapshotV2DiffRebaseStage,
        failure: SnapshotV2DiffRebaseCommitFailure,
        cleanup: SnapshotV2DiffRebaseCleanup,
        directory_sync: Option<io::ErrorKind>,
    ) -> Self {
        Self {
            exit: COMMITTED_UNCERTAIN_EXIT,
            diagnostic: Some(format!(
                "{tool}: native-v2 Diff rebase committed, but completion is uncertain during \
                 {stage}: {}; displaced cleanup {}; directory synchronization {}",
                rebase_commit_failure_message(failure),
                rebase_cleanup_message(cleanup),
                directory_sync_message(directory_sync),
            )),
        }
    }

    fn emit(self) -> ExitCode {
        let mut stderr = io::stderr().lock();
        emit_diagnostic(&mut stderr, self.diagnostic.as_deref());
        ExitCode::from(self.exit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorCommandReport {
    exit: u8,
    output: Option<Vec<u8>>,
    output_failure_exit: u8,
    output_failure_diagnostic: &'static str,
    diagnostic: Option<String>,
}

impl EditorCommandReport {
    fn operational(diagnostic: impl Into<String>) -> Self {
        Self {
            exit: OPERATIONAL_EXIT,
            output: None,
            output_failure_exit: OPERATIONAL_EXIT,
            output_failure_diagnostic: "snapshot-editor: failed to write command output",
            diagnostic: Some(diagnostic.into()),
        }
    }

    #[cfg(unix)]
    fn cancelled(diagnostic: &'static str, signals: &CancellationState) -> Self {
        Self {
            exit: signals.exit_code(),
            output: None,
            output_failure_exit: signals.exit_code(),
            output_failure_diagnostic: diagnostic,
            diagnostic: Some(diagnostic.to_string()),
        }
    }

    fn output(
        bytes: Vec<u8>,
        output_failure_exit: u8,
        output_failure_diagnostic: &'static str,
    ) -> Self {
        Self {
            exit: 0,
            output: Some(bytes),
            output_failure_exit,
            output_failure_diagnostic,
            diagnostic: None,
        }
    }

    fn emit(self) -> ExitCode {
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        ExitCode::from(self.emit_with(&mut stdout, &mut stderr))
    }

    fn emit_with(self, stdout: &mut impl Write, stderr: &mut impl Write) -> u8 {
        if let Some(output) = self.output
            && (stdout.write_all(&output).is_err() || stdout.flush().is_err())
        {
            emit_diagnostic(stderr, Some(self.output_failure_diagnostic));
            return self.output_failure_exit;
        }
        emit_diagnostic(stderr, self.diagnostic.as_deref());
        self.exit
    }
}

fn emit_diagnostic(stderr: &mut impl Write, diagnostic: Option<&str>) {
    if let Some(diagnostic) = diagnostic {
        let _ = stderr.write_all(diagnostic.as_bytes());
        let _ = stderr.write_all(b"\n");
        let _ = stderr.flush();
    }
}

#[cfg(target_os = "macos")]
fn report_for_rebase_commit(
    tool: RebaseTool,
    commit: SnapshotV2DiffRebaseCommit,
) -> RebaseExecutionReport {
    match commit {
        SnapshotV2DiffRebaseCommit::Durable => RebaseExecutionReport::durable(),
        SnapshotV2DiffRebaseCommit::Uncertain {
            stage,
            failure,
            cleanup,
            directory_sync,
        } => RebaseExecutionReport::committed_uncertain(
            tool,
            stage,
            failure,
            cleanup,
            directory_sync,
        ),
    }
}

#[cfg(target_os = "macos")]
fn rebase_commit_failure_message(failure: SnapshotV2DiffRebaseCommitFailure) -> String {
    match failure {
        SnapshotV2DiffRebaseCommitFailure::DirectoryChanged { input } => {
            format!("{input} directory identity changed")
        }
        SnapshotV2DiffRebaseCommitFailure::BaseEntryChanged => {
            "committed base entry identity changed".to_string()
        }
        SnapshotV2DiffRebaseCommitFailure::DisplacedEntryChanged => {
            "displaced base entry identity changed".to_string()
        }
        SnapshotV2DiffRebaseCommitFailure::DiffChanged => {
            "immutable diff identity changed".to_string()
        }
        SnapshotV2DiffRebaseCommitFailure::BaseSourceChanged => {
            "retained base identity changed".to_string()
        }
        SnapshotV2DiffRebaseCommitFailure::Cleanup => {
            "displaced base cleanup was inconclusive".to_string()
        }
        SnapshotV2DiffRebaseCommitFailure::Io(kind) => {
            format!("post-commit I/O failed with {kind:?}")
        }
    }
}

#[cfg(target_os = "macos")]
fn rebase_cleanup_message(cleanup: SnapshotV2DiffRebaseCleanup) -> String {
    match cleanup {
        SnapshotV2DiffRebaseCleanup::Removed => "removed".to_string(),
        SnapshotV2DiffRebaseCleanup::AlreadyAbsent => "already absent".to_string(),
        SnapshotV2DiffRebaseCleanup::ChangedRefused => "changed entry retained".to_string(),
        SnapshotV2DiffRebaseCleanup::Failed(kind) => format!("failed with {kind:?}"),
    }
}

#[cfg(unix)]
fn directory_sync_message(directory_sync: Option<io::ErrorKind>) -> String {
    match directory_sync {
        Some(kind) => format!("failed with {kind:?}"),
        None => "completed".to_string(),
    }
}

/// Executes one request through the shared path transaction.
pub fn execute_rebase(tool: RebaseTool, request: RebaseRequest) -> ExitCode {
    #[cfg(target_os = "macos")]
    {
        let signals = match CancellationSignals::install() {
            Ok(signals) => signals,
            Err(_) => {
                return RebaseExecutionReport::operational(
                    tool,
                    "failed to install cancellation handlers",
                )
                .emit();
            }
        };
        let paths = SnapshotV2DiffRebasePaths::new(request.base, request.diff);
        match rebase_snapshot_v2_diff_paths_with_cancel(&paths, |_| signals.state.is_cancelled()) {
            Ok(outcome) => report_for_rebase_commit(tool, outcome.commit()).emit(),
            Err(error) if matches!(error.failure(), SnapshotV2DiffRebaseFailure::Cancelled) => {
                RebaseExecutionReport::cancelled(tool, &signals.state).emit()
            }
            Err(error) => RebaseExecutionReport::operational(tool, error).emit(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let RebaseRequest { base, diff } = request;
        let _ = (base, diff);
        RebaseExecutionReport::operational(tool, "native-v2 Diff rebase is supported only on macOS")
            .emit()
    }
}

/// Executes one descriptor-anchored native snapshot inspection.
pub fn execute_snapshot_info(view: SnapshotInfoView, path: PathBuf) -> ExitCode {
    #[cfg(unix)]
    {
        execute_snapshot_info_unix(SnapshotInfoRequest { view, path }).emit()
    }
    #[cfg(not(unix))]
    {
        let _ = (view, path);
        EditorCommandReport::operational(
            "snapshot-editor: native snapshot inspection requires Unix",
        )
        .emit()
    }
}

#[cfg(unix)]
fn execute_snapshot_info_unix(request: SnapshotInfoRequest) -> EditorCommandReport {
    let signals = match CancellationSignals::install() {
        Ok(signals) => signals,
        Err(_) => {
            return EditorCommandReport::operational(
                "snapshot-editor: failed to install cancellation handlers",
            );
        }
    };
    let bytes =
        read_snapshot_state_file_with_cancel(&request.path, |_| signals.state.is_cancelled());
    if signals.state.is_cancelled() {
        return EditorCommandReport::cancelled(
            "snapshot-editor: snapshot inspection cancelled before output",
            &signals.state,
        );
    }
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) => {
            return EditorCommandReport::operational(format!("snapshot-editor: {error}"));
        }
    };

    let document = HvfNativeSnapshotDocument::decode(&bytes);
    if signals.state.is_cancelled() {
        return EditorCommandReport::cancelled(
            "snapshot-editor: snapshot inspection cancelled before output",
            &signals.state,
        );
    }
    let document = match document {
        Ok(document) => document,
        Err(_) => {
            return EditorCommandReport::operational(
                "snapshot-editor: input is not a supported Bangbang native snapshot state document",
            );
        }
    };

    let output = match request.view {
        SnapshotInfoView::Version => Ok(format!("v{}\n", document.version()).into_bytes()),
        SnapshotInfoView::VcpuStates => document
            .inspect_vcpu_states()
            .to_pretty_json()
            .map_err(|_| ())
            .and_then(append_newline),
        SnapshotInfoView::VmState => document
            .inspect_vm_state()
            .to_pretty_json()
            .map_err(|_| ())
            .and_then(append_newline),
    };
    if signals.state.is_cancelled() {
        return EditorCommandReport::cancelled(
            "snapshot-editor: snapshot inspection cancelled before output",
            &signals.state,
        );
    }
    match output {
        Ok(output) => EditorCommandReport::output(
            output,
            OPERATIONAL_EXIT,
            "snapshot-editor: failed to write snapshot inspection output",
        ),
        Err(()) => EditorCommandReport::operational(
            "snapshot-editor: failed to construct snapshot inspection output",
        ),
    }
}

#[cfg(unix)]
fn append_newline(mut output: String) -> Result<Vec<u8>, ()> {
    output.try_reserve(1).map_err(|_| ())?;
    output.push('\n');
    Ok(output.into_bytes())
}

/// Executes one reviewed native-state register-removal transaction.
///
/// Semantic register admission completes and the submitted identifiers are
/// discarded before cancellation handlers are installed or either path is
/// accessed.
pub fn execute_snapshot_register_removal(
    register_ids: Vec<u64>,
    input: PathBuf,
    output: PathBuf,
) -> ExitCode {
    #[cfg(unix)]
    {
        let reviewed = match HvfNativeSnapshotRegisterRemovalRequest::try_new(&register_ids) {
            Ok(reviewed) => reviewed,
            Err(_) => {
                return EditorCommandReport::operational(
                    "snapshot-editor: reviewed register-removal request is invalid",
                )
                .emit();
            }
        };
        drop(register_ids);
        execute_snapshot_register_removal_unix(RegisterRemovalRequest {
            reviewed,
            paths: SnapshotStateEditPaths::new(input, output),
        })
        .emit()
    }
    #[cfg(not(unix))]
    {
        let _ = (register_ids, input, output);
        EditorCommandReport::operational(
            "snapshot-editor: native snapshot register editing requires Unix",
        )
        .emit()
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegisterRemovalOperationFailure {
    Decode,
    Transform,
    Summary,
    Encode,
    VerifyDecode,
    VerifyMismatch,
}

#[cfg(unix)]
struct EditedSnapshotProduct {
    outcome: HvfNativeSnapshotRegisterRemovalOutcome,
    summary: Vec<u8>,
}

#[cfg(unix)]
fn execute_snapshot_register_removal_unix(request: RegisterRemovalRequest) -> EditorCommandReport {
    let signals = match CancellationSignals::install() {
        Ok(signals) => signals,
        Err(_) => {
            return EditorCommandReport::operational(
                "snapshot-editor: failed to install cancellation handlers",
            );
        }
    };
    let RegisterRemovalRequest { reviewed, paths } = request;
    let result = publish_edited_snapshot_state_with_cancel(
        &paths,
        |bytes| {
            let document = HvfNativeSnapshotDocument::decode(bytes)
                .map_err(|_| RegisterRemovalOperationFailure::Decode)?;
            let outcome = document
                .try_remove_reviewed_kvm_register_request(&reviewed)
                .map_err(|_| RegisterRemovalOperationFailure::Transform)?;
            let summary = register_removal_summary(outcome.report())?;
            Ok(EditedSnapshotProduct { outcome, summary })
        },
        |product| {
            product
                .outcome
                .document()
                .encode()
                .map_err(|_| RegisterRemovalOperationFailure::Encode)
        },
        |staged, product| {
            let staged = HvfNativeSnapshotDocument::decode(staged)
                .map_err(|_| RegisterRemovalOperationFailure::VerifyDecode)?;
            if staged == *product.outcome.document() {
                Ok(())
            } else {
                Err(RegisterRemovalOperationFailure::VerifyMismatch)
            }
        },
        |_| signals.state.is_cancelled(),
    );

    match result {
        Ok(outcome) => {
            let (product, commit) = outcome.into_parts();
            match commit {
                SnapshotStateEditCommit::Durable => EditorCommandReport::output(
                    product.summary,
                    COMMITTED_UNCERTAIN_EXIT,
                    "snapshot-editor: edited snapshot state committed, but summary output failed",
                ),
                SnapshotStateEditCommit::Uncertain {
                    stage,
                    failure,
                    staging_cleanup,
                    directory_sync,
                } => EditorCommandReport {
                    exit: COMMITTED_UNCERTAIN_EXIT,
                    output: None,
                    output_failure_exit: COMMITTED_UNCERTAIN_EXIT,
                    output_failure_diagnostic: "snapshot-editor: edited snapshot state completion is uncertain",
                    diagnostic: Some(format!(
                        "snapshot-editor: edited snapshot state committed, but completion is \
                         uncertain during {stage}: {}; staging cleanup {}; directory \
                         synchronization {}",
                        state_commit_failure_message(failure),
                        state_cleanup_message(staging_cleanup),
                        directory_sync_message(directory_sync),
                    )),
                },
            }
        }
        Err(SnapshotStateEditTransactionError::Publication(error))
            if matches!(error.failure(), SnapshotStateEditFailure::Cancelled) =>
        {
            EditorCommandReport::cancelled(
                "snapshot-editor: snapshot register editing cancelled before commit",
                &signals.state,
            )
        }
        Err(SnapshotStateEditTransactionError::Publication(error)) => {
            EditorCommandReport::operational(format!("snapshot-editor: {error}"))
        }
        Err(SnapshotStateEditTransactionError::Operation(error)) => {
            EditorCommandReport::operational(format!(
                "snapshot-editor: snapshot register editing failed during {}",
                error.stage()
            ))
        }
    }
}

#[cfg(unix)]
fn register_removal_summary(
    report: &HvfNativeSnapshotRegisterRemovalReport,
) -> Result<Vec<u8>, RegisterRemovalOperationFailure> {
    let capacity = report
        .vcpus()
        .len()
        .checked_mul(96)
        .and_then(|value| value.checked_add(96))
        .ok_or(RegisterRemovalOperationFailure::Summary)?;
    let mut summary = String::new();
    summary
        .try_reserve(capacity)
        .map_err(|_| RegisterRemovalOperationFailure::Summary)?;
    for vcpu in report.vcpus() {
        writeln!(
            summary,
            "vcpu {}: removed {}, not-present {}",
            vcpu.vcpu_index(),
            vcpu.removed_count(),
            vcpu.not_present_count(),
        )
        .map_err(|_| RegisterRemovalOperationFailure::Summary)?;
    }
    writeln!(
        summary,
        "total: requested {}, removed {}, not-present {}",
        report.request_count(),
        report.removed_count(),
        report.not_present_count(),
    )
    .map_err(|_| RegisterRemovalOperationFailure::Summary)?;
    Ok(summary.into_bytes())
}

#[cfg(unix)]
fn state_commit_failure_message(failure: SnapshotStateEditCommitFailure) -> String {
    match failure {
        SnapshotStateEditCommitFailure::DirectoryChanged { path } => {
            format!("{path} directory identity changed")
        }
        SnapshotStateEditCommitFailure::InputChanged => {
            "immutable input identity changed".to_string()
        }
        SnapshotStateEditCommitFailure::OutputEntryChanged => {
            "committed output entry identity changed".to_string()
        }
        SnapshotStateEditCommitFailure::StagingEntryChanged => {
            "private staging entry identity changed".to_string()
        }
        SnapshotStateEditCommitFailure::Cleanup => {
            "private staging cleanup was inconclusive".to_string()
        }
        SnapshotStateEditCommitFailure::Io(kind) => {
            format!("post-commit I/O failed with {kind:?}")
        }
    }
}

#[cfg(unix)]
fn state_cleanup_message(cleanup: SnapshotStateEditCleanup) -> String {
    match cleanup {
        SnapshotStateEditCleanup::Removed => "removed".to_string(),
        SnapshotStateEditCleanup::AlreadyAbsent => "already absent".to_string(),
        SnapshotStateEditCleanup::ChangedRefused => "changed entry retained".to_string(),
        SnapshotStateEditCleanup::Failed(kind) => format!("failed with {kind:?}"),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::cell::RefCell;
    #[cfg(unix)]
    use std::rc::Rc;

    use super::*;

    #[test]
    fn rebase_request_debug_redacts_both_paths() {
        let request = RebaseRequest::new("secret-base", "secret-diff");
        let debug = format!("{request:?}");
        assert_eq!(
            debug,
            "RebaseRequest { base: \"<redacted>\", diff: \"<redacted>\" }"
        );
        assert!(!debug.contains("secret-base"));
        assert!(!debug.contains("secret-diff"));
    }

    #[cfg(unix)]
    #[test]
    fn editor_requests_redact_paths_and_register_targets() {
        let info = SnapshotInfoRequest {
            view: SnapshotInfoView::VmState,
            path: PathBuf::from("secret-info"),
        };
        let removal = RegisterRemovalRequest {
            reviewed: HvfNativeSnapshotRegisterRemovalRequest::try_new(&[0x6030_0000_0013_8004])
                .expect("reviewed ID should be accepted"),
            paths: SnapshotStateEditPaths::new("secret-input", "secret-output"),
        };
        let debug = format!("{info:?} {removal:?}");
        assert!(debug.contains("request_count: 1"));
        assert!(!debug.contains("secret-info"));
        assert!(!debug.contains("secret-input"));
        assert!(!debug.contains("secret-output"));
        assert!(!debug.contains("6030"));
    }

    #[cfg(unix)]
    #[test]
    fn partial_signal_registration_is_reversed() {
        let attempts = Rc::new(RefCell::new(Vec::new()));
        let unregistered = Rc::new(RefCell::new(Vec::new()));
        let register_attempts = Rc::clone(&attempts);
        let recorded_unregistrations = Rc::clone(&unregistered);
        let error = register_signal_flags_with(
            move |signal, _| {
                register_attempts.borrow_mut().push(signal);
                if signal == SIGTERM {
                    Err(io::Error::other("test registration failure"))
                } else {
                    Ok(41_u8)
                }
            },
            move |registration| recorded_unregistrations.borrow_mut().push(registration),
        )
        .expect_err("second registration should fail");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(*attempts.borrow(), vec![SIGINT, SIGTERM]);
        assert_eq!(*unregistered.borrow(), vec![41]);
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_uses_signal_exits_with_termination_precedence() {
        let state = CancellationState {
            interrupt: Arc::new(AtomicBool::new(true)),
            terminate: Arc::new(AtomicBool::new(false)),
        };
        assert_eq!(state.exit_code(), 130);
        state.terminate.store(true, Ordering::Release);
        assert_eq!(state.exit_code(), 143);
    }

    #[test]
    fn writer_failures_keep_precommit_and_postcommit_exit_classes() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
        }

        let inspection = EditorCommandReport::output(
            b"output\n".to_vec(),
            1,
            "snapshot-editor: inspection output failed",
        );
        let committed = EditorCommandReport::output(
            b"summary\n".to_vec(),
            3,
            "snapshot-editor: committed summary failed",
        );
        let mut stderr = Vec::new();
        assert_eq!(inspection.emit_with(&mut Closed, &mut stderr), 1);
        assert_eq!(stderr, b"snapshot-editor: inspection output failed\n");
        assert_eq!(committed.emit_with(&mut Closed, &mut Closed), 3);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rebase_commit_reports_keep_durable_and_uncertain_distinct() {
        assert_eq!(
            report_for_rebase_commit(
                RebaseTool::SnapshotEditor,
                SnapshotV2DiffRebaseCommit::Durable
            ),
            RebaseExecutionReport::durable()
        );
        let report = report_for_rebase_commit(
            RebaseTool::RebaseSnap,
            SnapshotV2DiffRebaseCommit::Uncertain {
                stage: SnapshotV2DiffRebaseStage::DisplacedCleanup,
                failure: SnapshotV2DiffRebaseCommitFailure::DirectoryChanged {
                    input: bangbang_runtime::snapshot_rebase::SnapshotV2DiffRebaseInput::Base,
                },
                cleanup: SnapshotV2DiffRebaseCleanup::Failed(io::ErrorKind::PermissionDenied),
                directory_sync: Some(io::ErrorKind::Other),
            },
        );
        assert_eq!(report.exit, 3);
        let diagnostic = report
            .diagnostic
            .expect("uncertain outcome should have a diagnostic");
        assert!(diagnostic.starts_with("rebase-snap: native-v2 Diff rebase committed"));
        assert!(diagnostic.contains("displaced-base cleanup"));
        assert!(diagnostic.contains("base directory identity changed"));
        assert!(!diagnostic.contains('/'));
    }
}
