//! Shared execution boundary for Firecracker-shaped snapshot rebase tools.

use std::fmt;
#[cfg(target_os = "macos")]
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "macos")]
use bangbang_runtime::snapshot_rebase::{
    SnapshotV2DiffRebaseCleanup, SnapshotV2DiffRebaseCommit, SnapshotV2DiffRebaseCommitFailure,
    SnapshotV2DiffRebaseFailure, SnapshotV2DiffRebasePaths, SnapshotV2DiffRebaseStage,
    rebase_snapshot_v2_diff_paths_with_cancel,
};
#[cfg(target_os = "macos")]
use signal_hook::SigId;
#[cfg(target_os = "macos")]
use signal_hook::consts::signal::{SIGINT, SIGTERM};

const REDACTED: &str = "<redacted>";
const OPERATIONAL_EXIT: u8 = 1;
#[cfg(target_os = "macos")]
const COMMITTED_UNCERTAIN_EXIT: u8 = 3;
#[cfg(target_os = "macos")]
const SIGINT_EXIT: u8 = 128 + SIGINT as u8;
#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct CancellationState {
    interrupt: Arc<AtomicBool>,
    terminate: Arc<AtomicBool>,
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
struct CancellationSignals {
    state: CancellationState,
    registrations: [SigId; 2],
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
impl fmt::Debug for CancellationSignals {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationSignals")
            .field("received", &self.state.is_cancelled())
            .field("registrations", &self.registrations.len())
            .finish()
    }
}

#[cfg(target_os = "macos")]
impl Drop for CancellationSignals {
    fn drop(&mut self) {
        for registration in self.registrations {
            signal_hook::low_level::unregister(registration);
        }
    }
}

#[cfg(target_os = "macos")]
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
struct ExecutionReport {
    exit: u8,
    diagnostic: Option<String>,
}

impl ExecutionReport {
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
                commit_failure_message(failure),
                cleanup_message(cleanup),
                directory_sync_message(directory_sync),
            )),
        }
    }

    fn emit(self) -> ExitCode {
        if let Some(diagnostic) = self.diagnostic {
            eprintln!("{diagnostic}");
        }
        ExitCode::from(self.exit)
    }
}

#[cfg(target_os = "macos")]
fn report_for_commit(tool: RebaseTool, commit: SnapshotV2DiffRebaseCommit) -> ExecutionReport {
    match commit {
        SnapshotV2DiffRebaseCommit::Durable => ExecutionReport::durable(),
        SnapshotV2DiffRebaseCommit::Uncertain {
            stage,
            failure,
            cleanup,
            directory_sync,
        } => ExecutionReport::committed_uncertain(tool, stage, failure, cleanup, directory_sync),
    }
}

#[cfg(target_os = "macos")]
fn commit_failure_message(failure: SnapshotV2DiffRebaseCommitFailure) -> String {
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
fn cleanup_message(cleanup: SnapshotV2DiffRebaseCleanup) -> String {
    match cleanup {
        SnapshotV2DiffRebaseCleanup::Removed => "removed".to_string(),
        SnapshotV2DiffRebaseCleanup::AlreadyAbsent => "already absent".to_string(),
        SnapshotV2DiffRebaseCleanup::ChangedRefused => "changed entry retained".to_string(),
        SnapshotV2DiffRebaseCleanup::Failed(kind) => format!("failed with {kind:?}"),
    }
}

#[cfg(target_os = "macos")]
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
                return ExecutionReport::operational(
                    tool,
                    "failed to install cancellation handlers",
                )
                .emit();
            }
        };
        let paths = SnapshotV2DiffRebasePaths::new(request.base, request.diff);
        match rebase_snapshot_v2_diff_paths_with_cancel(&paths, |_| signals.state.is_cancelled()) {
            Ok(outcome) => report_for_commit(tool, outcome.commit()).emit(),
            Err(error) if matches!(error.failure(), SnapshotV2DiffRebaseFailure::Cancelled) => {
                ExecutionReport::cancelled(tool, &signals.state).emit()
            }
            Err(error) => ExecutionReport::operational(tool, error).emit(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let RebaseRequest { base, diff } = request;
        let _ = (base, diff);
        ExecutionReport::operational(tool, "native-v2 Diff rebase is supported only on macOS")
            .emit()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::cell::RefCell;
    #[cfg(target_os = "macos")]
    use std::rc::Rc;

    use super::*;

    #[test]
    fn request_debug_redacts_both_paths() {
        let request = RebaseRequest::new("secret-base", "secret-diff");
        let debug = format!("{request:?}");
        assert_eq!(
            debug,
            "RebaseRequest { base: \"<redacted>\", diff: \"<redacted>\" }"
        );
        assert!(!debug.contains("secret-base"));
        assert!(!debug.contains("secret-diff"));
    }

    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
    #[test]
    fn cancellation_uses_conventional_signal_exits_with_termination_precedence() {
        let state = CancellationState {
            interrupt: Arc::new(AtomicBool::new(true)),
            terminate: Arc::new(AtomicBool::new(false)),
        };
        assert_eq!(
            ExecutionReport::cancelled(RebaseTool::RebaseSnap, &state).exit,
            130
        );
        state.terminate.store(true, Ordering::Release);
        assert_eq!(
            ExecutionReport::cancelled(RebaseTool::SnapshotEditor, &state).exit,
            143
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn durable_and_committed_uncertain_results_are_distinct_and_redacted() {
        assert_eq!(
            report_for_commit(
                RebaseTool::SnapshotEditor,
                SnapshotV2DiffRebaseCommit::Durable
            ),
            ExecutionReport::durable()
        );
        let report = report_for_commit(
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
