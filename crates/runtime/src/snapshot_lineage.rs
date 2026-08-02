//! Transactional live-session lineage for native snapshot publication.

use std::fmt;

use crate::snapshot_diff_v2_13::SnapshotV2DiffBase;
use crate::snapshot_memory_v2::{SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError};

const REDACTED: &str = "<redacted>";

/// Kind of live snapshot transaction represented by one lineage token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSnapshotKind {
    /// A complete memory image publication.
    Full,
    /// A differential layer publication.
    Diff,
}

/// Opaque authority to complete exactly one live lineage transaction.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LiveSnapshotLineageToken(u64);

impl fmt::Debug for LiveSnapshotLineageToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("LiveSnapshotLineageToken")
            .field(&REDACTED)
            .finish()
    }
}

/// Exact result made durably visible by a live snapshot transaction.
pub enum LiveSnapshotPublishedResult {
    /// Native-v1 Full publication has no native-v2 predecessor binding.
    NativeV1Full,
    /// Native-v2 Full or Diff publication produced this complete result.
    NativeV2(SnapshotV2MemoryBinding),
}

impl fmt::Debug for LiveSnapshotPublishedResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveSnapshotPublishedResult")
            .field(
                "kind",
                &match self {
                    Self::NativeV1Full => "native-v1-full",
                    Self::NativeV2(_) => "native-v2",
                },
            )
            .field("binding", &REDACTED)
            .finish()
    }
}

/// Cause that made the live predecessor or dirty generation ambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSnapshotLineageTerminalCause {
    /// A published state/memory pair has uncertain durability.
    PublicationDurabilityUncertain,
    /// A durable publication could not retain its exact result binding.
    ResultBindingUnavailable,
    /// Dirty reprotection or generation reset failed after publication.
    DirtyEpochResetFailed,
    /// Dirty reset returned a generation other than the expected successor.
    DirtyEpochMismatch,
}

/// Prepared exact base and generation for one dormant Diff capture.
pub struct LiveSnapshotDiffBegin {
    token: LiveSnapshotLineageToken,
    base: SnapshotV2DiffBase,
    dirty_epoch: Option<u64>,
}

impl LiveSnapshotDiffBegin {
    /// Returns the authority for aborting or completing this transaction.
    pub const fn token(&self) -> LiveSnapshotLineageToken {
        self.token
    }

    /// Returns the exact predecessor required for omitted bytes.
    pub const fn base(&self) -> &SnapshotV2DiffBase {
        &self.base
    }

    /// Returns the tracked generation, or `None` for all-current mode.
    pub const fn dirty_epoch(&self) -> Option<u64> {
        self.dirty_epoch
    }

    /// Consumes this preparation into writer-ready parts.
    pub fn into_parts(self) -> (LiveSnapshotLineageToken, SnapshotV2DiffBase, Option<u64>) {
        (self.token, self.base, self.dirty_epoch)
    }
}

impl fmt::Debug for LiveSnapshotDiffBegin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiveSnapshotDiffBegin")
            .field("token", &self.token)
            .field("base", &self.base)
            .field("tracked", &self.dirty_epoch.is_some())
            .finish()
    }
}

/// Invalid or unrepresentable live-lineage transition.
#[derive(Debug)]
pub enum LiveSnapshotLineageError {
    AlreadyPending,
    AwaitingDirtyReset,
    Terminal,
    DirtyTrackingMismatch,
    DirtyEpochExhausted,
    TokenExhausted,
    DiffBaseUnavailable,
    StaleToken,
    NotPending,
    NotAwaitingDirtyReset,
    InvalidPublishedResult,
    ResultBinding(SnapshotV2MemoryBindingError),
    DirtyEpochMismatch,
}

impl fmt::Display for LiveSnapshotLineageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AlreadyPending => "a live snapshot lineage transaction is already pending",
            Self::AwaitingDirtyReset => {
                "a durable live snapshot publication is awaiting dirty reset"
            }
            Self::Terminal => "live snapshot lineage is terminally ambiguous",
            Self::DirtyTrackingMismatch => {
                "live snapshot lineage and dirty tracker generations do not match"
            }
            Self::DirtyEpochExhausted => "live snapshot dirty generation is exhausted",
            Self::TokenExhausted => "live snapshot lineage token space is exhausted",
            Self::DiffBaseUnavailable => "live snapshot Diff predecessor is unavailable",
            Self::StaleToken => "live snapshot lineage token is stale",
            Self::NotPending => "no live snapshot lineage transaction is pending",
            Self::NotAwaitingDirtyReset => {
                "no durable live snapshot publication is awaiting dirty reset"
            }
            Self::InvalidPublishedResult => {
                "published snapshot result is invalid for this lineage transaction"
            }
            Self::ResultBinding(_) => "failed to retain the published native-v2 memory binding",
            Self::DirtyEpochMismatch => {
                "dirty reset did not produce the expected live snapshot generation"
            }
        })
    }
}

impl std::error::Error for LiveSnapshotLineageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ResultBinding(source) => Some(source),
            _ => None,
        }
    }
}

/// Owner-local native-v2 predecessor and dirty-generation transaction state.
pub struct LiveSnapshotLineage {
    state: LineageState,
    next_token: Option<u64>,
}

impl LiveSnapshotLineage {
    /// Creates an origin with no proven native-v2 predecessor.
    pub const fn unavailable(dirty_epoch: Option<u64>) -> Self {
        Self::from_ready(ReadyLineage {
            base: ReadyBase::Unavailable,
            dirty_epoch,
        })
    }

    /// Creates a proven zero root covered by the supplied tracked generation.
    pub const fn zero(dirty_epoch: u64) -> Self {
        Self::from_ready(ReadyLineage {
            base: ReadyBase::Zero,
            dirty_epoch: Some(dirty_epoch),
        })
    }

    /// Creates an exact complete-image predecessor origin.
    pub const fn image(binding: SnapshotV2MemoryBinding, dirty_epoch: Option<u64>) -> Self {
        Self::from_ready(ReadyLineage {
            base: ReadyBase::Image(binding),
            dirty_epoch,
        })
    }

    const fn from_ready(ready: ReadyLineage) -> Self {
        Self {
            state: LineageState::Ready(ready),
            next_token: Some(1),
        }
    }

    /// Returns the current proven complete-image predecessor when ready.
    pub const fn current_image_binding(&self) -> Option<&SnapshotV2MemoryBinding> {
        match &self.state {
            LineageState::Ready(ready) => ready.base.image_binding(),
            LineageState::Pending(pending) => pending.prior.base.image_binding(),
            LineageState::Published(published) => published.prior.base.image_binding(),
            LineageState::Terminal(_) => None,
        }
    }

    /// Returns the current proven dirty generation when one is tracked.
    pub const fn current_dirty_epoch(&self) -> Option<u64> {
        match &self.state {
            LineageState::Ready(ready) => ready.dirty_epoch,
            LineageState::Pending(pending) => pending.prior.dirty_epoch,
            LineageState::Published(published) => published.prior.dirty_epoch,
            LineageState::Terminal(_) => None,
        }
    }

    /// Returns whether a transaction currently owns the lineage.
    pub const fn is_pending(&self) -> bool {
        matches!(
            self.state,
            LineageState::Pending(_) | LineageState::Published(_)
        )
    }

    /// Returns whether visible ambiguity permanently ended this lineage.
    pub const fn is_terminal(&self) -> bool {
        matches!(self.state, LineageState::Terminal(_))
    }

    /// Returns the recorded visible ambiguity when this lineage is terminal.
    pub const fn terminal_cause(&self) -> Option<LiveSnapshotLineageTerminalCause> {
        match &self.state {
            LineageState::Terminal(cause) => Some(*cause),
            LineageState::Ready(_) | LineageState::Pending(_) | LineageState::Published(_) => None,
        }
    }

    /// Begins one Full publication after checking the observed dirty epoch.
    pub fn begin_full(
        &mut self,
        observed_dirty_epoch: Option<u64>,
    ) -> Result<LiveSnapshotLineageToken, LiveSnapshotLineageError> {
        self.validate_begin(observed_dirty_epoch)?;
        let token = self.allocate_token()?;
        self.move_ready_to_pending(token, LiveSnapshotKind::Full)?;
        Ok(token)
    }

    /// Begins one Diff publication and fallibly retains its exact base.
    pub fn begin_diff(
        &mut self,
        observed_dirty_epoch: Option<u64>,
    ) -> Result<LiveSnapshotDiffBegin, LiveSnapshotLineageError> {
        let ready = self.validate_begin(observed_dirty_epoch)?;
        let base = match &ready.base {
            ReadyBase::Unavailable => return Err(LiveSnapshotLineageError::DiffBaseUnavailable),
            ReadyBase::Zero => SnapshotV2DiffBase::Zero,
            ReadyBase::Image(binding) => SnapshotV2DiffBase::Image(
                binding
                    .try_clone()
                    .map_err(LiveSnapshotLineageError::ResultBinding)?,
            ),
        };
        let token = self.allocate_token()?;
        self.move_ready_to_pending(token, LiveSnapshotKind::Diff)?;
        Ok(LiveSnapshotDiffBegin {
            token,
            base,
            dirty_epoch: observed_dirty_epoch,
        })
    }

    /// Aborts one pre-visible transaction and restores its exact prior state.
    pub fn abort(
        &mut self,
        token: LiveSnapshotLineageToken,
    ) -> Result<(), LiveSnapshotLineageError> {
        match self.take_state() {
            LineageState::Pending(pending) if pending.token == token => {
                self.state = LineageState::Ready(pending.prior);
                Ok(())
            }
            state => self.restore_after_invalid_pending_token(state),
        }
    }

    /// Records a durable visible result before dirty reset is attempted.
    pub fn published_durable(
        &mut self,
        token: LiveSnapshotLineageToken,
        result: LiveSnapshotPublishedResult,
    ) -> Result<(), LiveSnapshotLineageError> {
        let pending = match self.take_state() {
            LineageState::Pending(pending) if pending.token == token => pending,
            state => return self.restore_after_invalid_pending_token(state),
        };
        let next_base = match (pending.kind, result) {
            (LiveSnapshotKind::Full, LiveSnapshotPublishedResult::NativeV1Full) => {
                ReadyBase::Unavailable
            }
            (
                LiveSnapshotKind::Full | LiveSnapshotKind::Diff,
                LiveSnapshotPublishedResult::NativeV2(binding),
            ) => ReadyBase::Image(binding),
            (LiveSnapshotKind::Diff, LiveSnapshotPublishedResult::NativeV1Full) => {
                self.state = LineageState::Terminal(
                    LiveSnapshotLineageTerminalCause::ResultBindingUnavailable,
                );
                return Err(LiveSnapshotLineageError::InvalidPublishedResult);
            }
        };
        self.state = LineageState::Published(PublishedLineage {
            token,
            prior: pending.prior,
            next_base,
        });
        Ok(())
    }

    /// Commits a durable result after the expected dirty reset completed.
    pub fn commit_reset(
        &mut self,
        token: LiveSnapshotLineageToken,
        reset_dirty_epoch: Option<u64>,
    ) -> Result<(), LiveSnapshotLineageError> {
        let published = match self.take_state() {
            LineageState::Published(published) if published.token == token => published,
            state => return self.restore_after_invalid_published_token(state),
        };
        let expected = match published.prior.dirty_epoch {
            Some(epoch) => match epoch.checked_add(1) {
                Some(epoch) => Some(epoch),
                None => {
                    self.state = LineageState::Terminal(
                        LiveSnapshotLineageTerminalCause::DirtyEpochMismatch,
                    );
                    return Err(LiveSnapshotLineageError::DirtyEpochExhausted);
                }
            },
            None => None,
        };
        if reset_dirty_epoch != expected {
            self.state =
                LineageState::Terminal(LiveSnapshotLineageTerminalCause::DirtyEpochMismatch);
            return Err(LiveSnapshotLineageError::DirtyEpochMismatch);
        }
        self.state = LineageState::Ready(ReadyLineage {
            base: published.next_base,
            dirty_epoch: reset_dirty_epoch,
        });
        Ok(())
    }

    /// Permanently records ambiguity after a result became visible.
    pub fn terminalize_visible(
        &mut self,
        token: LiveSnapshotLineageToken,
        cause: LiveSnapshotLineageTerminalCause,
    ) -> Result<(), LiveSnapshotLineageError> {
        match &self.state {
            LineageState::Pending(pending) if pending.token == token => {}
            LineageState::Published(published) if published.token == token => {}
            LineageState::Pending(_) | LineageState::Published(_) => {
                return Err(LiveSnapshotLineageError::StaleToken);
            }
            LineageState::Ready(_) => return Err(LiveSnapshotLineageError::NotPending),
            LineageState::Terminal(_) => return Err(LiveSnapshotLineageError::Terminal),
        }
        self.state = LineageState::Terminal(cause);
        Ok(())
    }

    fn validate_begin(
        &self,
        observed_dirty_epoch: Option<u64>,
    ) -> Result<&ReadyLineage, LiveSnapshotLineageError> {
        let ready = match &self.state {
            LineageState::Ready(ready) => ready,
            LineageState::Pending(_) => return Err(LiveSnapshotLineageError::AlreadyPending),
            LineageState::Published(_) => {
                return Err(LiveSnapshotLineageError::AwaitingDirtyReset);
            }
            LineageState::Terminal(_) => return Err(LiveSnapshotLineageError::Terminal),
        };
        if ready.dirty_epoch != observed_dirty_epoch {
            return Err(LiveSnapshotLineageError::DirtyTrackingMismatch);
        }
        if observed_dirty_epoch == Some(u64::MAX) {
            return Err(LiveSnapshotLineageError::DirtyEpochExhausted);
        }
        Ok(ready)
    }

    fn allocate_token(&mut self) -> Result<LiveSnapshotLineageToken, LiveSnapshotLineageError> {
        let token = self
            .next_token
            .ok_or(LiveSnapshotLineageError::TokenExhausted)?;
        self.next_token = token.checked_add(1);
        Ok(LiveSnapshotLineageToken(token))
    }

    fn move_ready_to_pending(
        &mut self,
        token: LiveSnapshotLineageToken,
        kind: LiveSnapshotKind,
    ) -> Result<(), LiveSnapshotLineageError> {
        match self.take_state() {
            LineageState::Ready(prior) => {
                self.state = LineageState::Pending(PendingLineage { token, kind, prior });
                Ok(())
            }
            state => {
                let error = match &state {
                    LineageState::Pending(_) => LiveSnapshotLineageError::AlreadyPending,
                    LineageState::Published(_) => LiveSnapshotLineageError::AwaitingDirtyReset,
                    LineageState::Terminal(_) => LiveSnapshotLineageError::Terminal,
                    LineageState::Ready(_) => LiveSnapshotLineageError::NotPending,
                };
                self.state = state;
                Err(error)
            }
        }
    }

    fn restore_after_invalid_pending_token(
        &mut self,
        state: LineageState,
    ) -> Result<(), LiveSnapshotLineageError> {
        let error = match &state {
            LineageState::Pending(_) | LineageState::Published(_) => {
                LiveSnapshotLineageError::StaleToken
            }
            LineageState::Ready(_) => LiveSnapshotLineageError::NotPending,
            LineageState::Terminal(_) => LiveSnapshotLineageError::Terminal,
        };
        self.state = state;
        Err(error)
    }

    fn restore_after_invalid_published_token(
        &mut self,
        state: LineageState,
    ) -> Result<(), LiveSnapshotLineageError> {
        let error = match &state {
            LineageState::Pending(_) | LineageState::Published(_) => {
                LiveSnapshotLineageError::StaleToken
            }
            LineageState::Ready(_) => LiveSnapshotLineageError::NotAwaitingDirtyReset,
            LineageState::Terminal(_) => LiveSnapshotLineageError::Terminal,
        };
        self.state = state;
        Err(error)
    }

    fn take_state(&mut self) -> LineageState {
        std::mem::replace(
            &mut self.state,
            LineageState::Terminal(LiveSnapshotLineageTerminalCause::DirtyEpochMismatch),
        )
    }
}

impl fmt::Debug for LiveSnapshotLineage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (state, base, tracked) = match &self.state {
            LineageState::Ready(ready) => ("ready", ready.base.kind(), ready.dirty_epoch.is_some()),
            LineageState::Pending(pending) => (
                "pending",
                pending.prior.base.kind(),
                pending.prior.dirty_epoch.is_some(),
            ),
            LineageState::Published(published) => (
                "published",
                published.prior.base.kind(),
                published.prior.dirty_epoch.is_some(),
            ),
            LineageState::Terminal(_) => ("terminal", "ambiguous", false),
        };
        formatter
            .debug_struct("LiveSnapshotLineage")
            .field("state", &state)
            .field("base", &base)
            .field("tracked", &tracked)
            .field("identity", &REDACTED)
            .finish()
    }
}

struct ReadyLineage {
    base: ReadyBase,
    dirty_epoch: Option<u64>,
}

enum ReadyBase {
    Unavailable,
    Zero,
    Image(SnapshotV2MemoryBinding),
}

impl ReadyBase {
    const fn image_binding(&self) -> Option<&SnapshotV2MemoryBinding> {
        match self {
            Self::Image(binding) => Some(binding),
            Self::Unavailable | Self::Zero => None,
        }
    }

    const fn kind(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Zero => "zero",
            Self::Image(_) => "image",
        }
    }
}

struct PendingLineage {
    token: LiveSnapshotLineageToken,
    kind: LiveSnapshotKind,
    prior: ReadyLineage,
}

struct PublishedLineage {
    token: LiveSnapshotLineageToken,
    prior: ReadyLineage,
    next_base: ReadyBase,
}

enum LineageState {
    Ready(ReadyLineage),
    Pending(PendingLineage),
    Published(PublishedLineage),
    Terminal(LiveSnapshotLineageTerminalCause),
}

#[cfg(test)]
mod tests {
    use crate::memory::{GuestAddress, GuestMemoryRange, aarch64};
    use crate::snapshot_diff_v2_13::SnapshotV2DiffBase;
    use crate::snapshot_format::SnapshotFormatVersion;
    use crate::snapshot_memory_v2::{
        SnapshotV2MemoryImageId, snapshot_v2_memory_binding_from_ranges_for_test,
    };

    use super::{
        LiveSnapshotLineage, LiveSnapshotLineageError, LiveSnapshotLineageTerminalCause,
        LiveSnapshotPublishedResult,
    };

    const VERSION: SnapshotFormatVersion = SnapshotFormatVersion::new(2, 13, 0);

    fn binding(id: u8) -> crate::snapshot_memory_v2::SnapshotV2MemoryBinding {
        let range = GuestMemoryRange::new(GuestAddress::new(aarch64::DRAM_MEM_START), 0x4000)
            .expect("test range should validate");
        snapshot_v2_memory_binding_from_ranges_for_test(
            VERSION,
            SnapshotV2MemoryImageId::from_bytes([id; 16]),
            &[range],
        )
        .expect("test binding should validate")
    }

    #[test]
    fn tracked_zero_diff_abort_restores_exact_generation() {
        let mut lineage = LiveSnapshotLineage::zero(7);
        let begin = lineage.begin_diff(Some(7)).expect("Diff should begin");
        assert!(matches!(begin.base(), SnapshotV2DiffBase::Zero));
        assert_eq!(begin.dirty_epoch(), Some(7));
        assert!(lineage.is_pending());

        lineage.abort(begin.token()).expect("abort should succeed");
        assert!(!lineage.is_pending());
        assert_eq!(lineage.current_dirty_epoch(), Some(7));
        assert!(lineage.current_image_binding().is_none());
    }

    #[test]
    fn image_diff_durable_reset_commits_exact_result() {
        let first = binding(1);
        let next = binding(2);
        let mut lineage = LiveSnapshotLineage::image(first, Some(3));
        let begin = lineage.begin_diff(Some(3)).expect("Diff should begin");
        assert_eq!(
            begin.base().binding().map(|value| value.image_id()),
            Some(binding(1).image_id())
        );

        lineage
            .published_durable(begin.token(), LiveSnapshotPublishedResult::NativeV2(next))
            .expect("durable result should record");
        assert_eq!(lineage.current_dirty_epoch(), Some(3));
        lineage
            .commit_reset(begin.token(), Some(4))
            .expect("reset should commit");
        assert_eq!(lineage.current_dirty_epoch(), Some(4));
        assert_eq!(
            lineage
                .current_image_binding()
                .map(|value| value.image_id()),
            Some(binding(2).image_id())
        );
    }

    #[test]
    fn native_v1_full_discards_native_v2_base_only_after_reset() {
        let original = binding(3);
        let original_id = original.image_id();
        let mut lineage = LiveSnapshotLineage::image(original, Some(9));
        let token = lineage.begin_full(Some(9)).expect("Full should begin");
        lineage
            .published_durable(token, LiveSnapshotPublishedResult::NativeV1Full)
            .expect("v1 result should record");
        assert_eq!(
            lineage
                .current_image_binding()
                .map(|value| value.image_id()),
            Some(original_id)
        );
        lineage
            .commit_reset(token, Some(10))
            .expect("reset should commit");
        assert!(lineage.current_image_binding().is_none());
        assert_eq!(lineage.current_dirty_epoch(), Some(10));
    }

    #[test]
    fn untracked_image_diff_commits_without_inventing_epoch() {
        let mut lineage = LiveSnapshotLineage::image(binding(4), None);
        let begin = lineage
            .begin_diff(None)
            .expect("untracked Diff should begin");
        assert_eq!(begin.dirty_epoch(), None);
        lineage
            .published_durable(
                begin.token(),
                LiveSnapshotPublishedResult::NativeV2(binding(5)),
            )
            .expect("result should record");
        lineage
            .commit_reset(begin.token(), None)
            .expect("untracked commit should succeed");
        assert_eq!(lineage.current_dirty_epoch(), None);
    }

    #[test]
    fn unavailable_lineage_rejects_diff_but_allows_full() {
        let mut lineage = LiveSnapshotLineage::unavailable(Some(0));
        assert!(matches!(
            lineage.begin_diff(Some(0)),
            Err(LiveSnapshotLineageError::DiffBaseUnavailable)
        ));
        assert!(!lineage.is_pending());
        assert!(lineage.begin_full(Some(0)).is_ok());
    }

    #[test]
    fn observed_epoch_must_match_and_must_not_be_saturated() {
        let mut lineage = LiveSnapshotLineage::zero(8);
        assert!(matches!(
            lineage.begin_full(Some(7)),
            Err(LiveSnapshotLineageError::DirtyTrackingMismatch)
        ));
        let mut exhausted = LiveSnapshotLineage::zero(u64::MAX);
        assert!(matches!(
            exhausted.begin_full(Some(u64::MAX)),
            Err(LiveSnapshotLineageError::DirtyEpochExhausted)
        ));
    }

    #[test]
    fn pending_token_cannot_be_reused_or_replaced() {
        let mut lineage = LiveSnapshotLineage::zero(0);
        let first = lineage.begin_full(Some(0)).expect("first should begin");
        assert!(matches!(
            lineage.begin_full(Some(0)),
            Err(LiveSnapshotLineageError::AlreadyPending)
        ));
        lineage.abort(first).expect("first should abort");
        let second = lineage.begin_full(Some(0)).expect("second should begin");
        assert_ne!(first, second);
        assert!(matches!(
            lineage.abort(first),
            Err(LiveSnapshotLineageError::StaleToken)
        ));
        lineage.abort(second).expect("second should abort");
        assert!(matches!(
            lineage.abort(second),
            Err(LiveSnapshotLineageError::NotPending)
        ));
    }

    #[test]
    fn token_space_never_wraps_or_reuses_the_last_token() {
        let mut lineage = LiveSnapshotLineage::zero(0);
        lineage.next_token = Some(u64::MAX);
        let last = lineage
            .begin_full(Some(0))
            .expect("last token should issue");
        lineage.abort(last).expect("last token should abort");
        assert!(matches!(
            lineage.begin_full(Some(0)),
            Err(LiveSnapshotLineageError::TokenExhausted)
        ));
    }

    #[test]
    fn uncertain_visibility_is_irreversibly_terminal() {
        let mut lineage = LiveSnapshotLineage::zero(0);
        let token = lineage.begin_full(Some(0)).expect("Full should begin");
        lineage
            .terminalize_visible(
                token,
                LiveSnapshotLineageTerminalCause::PublicationDurabilityUncertain,
            )
            .expect("uncertainty should terminalize");
        assert!(lineage.is_terminal());
        assert_eq!(
            lineage.terminal_cause(),
            Some(LiveSnapshotLineageTerminalCause::PublicationDurabilityUncertain)
        );
        assert!(matches!(
            lineage.begin_full(Some(0)),
            Err(LiveSnapshotLineageError::Terminal)
        ));
        assert!(matches!(
            lineage.abort(token),
            Err(LiveSnapshotLineageError::Terminal)
        ));
    }

    #[test]
    fn wrong_reset_epoch_terminalizes_visible_result() {
        let mut lineage = LiveSnapshotLineage::zero(2);
        let token = lineage.begin_full(Some(2)).expect("Full should begin");
        lineage
            .published_durable(token, LiveSnapshotPublishedResult::NativeV2(binding(6)))
            .expect("result should record");
        assert!(matches!(
            lineage.commit_reset(token, Some(4)),
            Err(LiveSnapshotLineageError::DirtyEpochMismatch)
        ));
        assert!(lineage.is_terminal());
    }

    #[test]
    fn diff_cannot_publish_native_v1_result() {
        let mut lineage = LiveSnapshotLineage::zero(0);
        let begin = lineage.begin_diff(Some(0)).expect("Diff should begin");
        assert!(matches!(
            lineage.published_durable(begin.token(), LiveSnapshotPublishedResult::NativeV1Full),
            Err(LiveSnapshotLineageError::InvalidPublishedResult)
        ));
        assert!(lineage.is_terminal());
    }

    #[test]
    fn diagnostics_redact_tokens_and_bindings() {
        let mut lineage = LiveSnapshotLineage::image(binding(7), Some(1));
        let begin = lineage.begin_diff(Some(1)).expect("Diff should begin");
        let debug = format!("{lineage:?} {begin:?} {:?}", begin.token());
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&format!("{:?}", binding(7).image_id())));
    }
}
