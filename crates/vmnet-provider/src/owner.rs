use std::fmt;
use std::sync::Arc;

use bangbang_session::credential::{CredentialPrefix, CredentialTarget};
use bangbang_session::vmnet_provider::{
    ProviderCleanup, ProviderStatus, RealizedVmnetParameters, VmnetPacketBatch,
};

use crate::policy::ResolvedVmnetPolicy;

/// Restricted callback publisher installed only after credential drop.
pub type OwnerReadinessCallback = Arc<dyn Fn(u16) + Send + Sync + 'static>;

/// Redacted failure from one credential transition or attestation operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerCredentialError;

impl fmt::Display for OwnerCredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private vmnet owner credential failure")
    }
}

impl std::error::Error for OwnerCredentialError {}

/// Stable backend start outcome with exact cleanup certainty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerStartFailure {
    status: ProviderStatus,
    cleanup: ProviderCleanup,
}

impl OwnerStartFailure {
    /// Constructs one categorical start failure.
    #[must_use]
    pub const fn new(status: ProviderStatus, cleanup: ProviderCleanup) -> Self {
        Self { status, cleanup }
    }

    /// Returns the provider status.
    #[must_use]
    pub const fn status(self) -> ProviderStatus {
        self.status
    }

    /// Returns cleanup certainty.
    #[must_use]
    pub const fn cleanup(self) -> ProviderCleanup {
        self.cleanup
    }
}

/// Backend operations required by one privilege-dropped owner.
pub trait OwnerBackend: fmt::Debug + Sized {
    /// Starts and freezes one backend while the caller is still exact root.
    fn start(
        policy: &ResolvedVmnetPolicy,
    ) -> Result<(Self, RealizedVmnetParameters), OwnerStartFailure>;

    /// Enables bounded packet-readiness publication after credential drop.
    fn enable_readiness(&mut self, callback: OwnerReadinessCallback) -> Result<(), ProviderStatus>;

    /// Reads one bounded ordered packet batch.
    fn read_packets(&mut self, maximum: u16) -> Result<VmnetPacketBatch, ProviderStatus>;

    /// Writes one bounded ordered packet batch and returns its completed prefix.
    fn write_packets(&mut self, packets: &VmnetPacketBatch) -> Result<u16, ProviderStatus>;

    /// Drains callbacks and stops the exact backend.
    fn stop(&mut self) -> ProviderCleanup;
}

/// Credential operations injected between root start and all packet service.
pub trait OwnerCredentialOps: fmt::Debug {
    /// Performs the production credential transition and returns its exact prefix.
    fn transition(
        &mut self,
        target: CredentialTarget,
    ) -> Result<CredentialPrefix, OwnerCredentialError>;

    /// Re-attests the exact final identity immediately before service.
    fn attest(&mut self, target: CredentialTarget) -> Result<(), OwnerCredentialError>;
}

/// Redacted owner startup/service failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerError {
    /// Backend start failed.
    Start(OwnerStartFailure),
    /// Credential transition or re-attestation failed.
    Credential {
        /// Cleanup certainty after the failed transition.
        cleanup: ProviderCleanup,
    },
    /// A post-drop backend operation failed.
    Backend(ProviderStatus),
}

impl fmt::Display for OwnerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private vmnet interface owner failure")
    }
}

impl std::error::Error for OwnerError {}

/// Root-only owner state. This type intentionally exposes no callback or packet
/// operation.
pub struct PrivilegedOwner<B: OwnerBackend> {
    backend: B,
    parameters: RealizedVmnetParameters,
}

impl<B: OwnerBackend> PrivilegedOwner<B> {
    /// Starts one real or injected backend while the caller retains root.
    pub fn start(policy: &ResolvedVmnetPolicy) -> Result<Self, OwnerError> {
        let (backend, parameters) = B::start(policy).map_err(OwnerError::Start)?;
        Ok(Self {
            backend,
            parameters,
        })
    }

    /// Irreversibly transitions to the target and returns the only packet-capable type.
    pub fn drop_credentials<C: OwnerCredentialOps>(
        mut self,
        credentials: &mut C,
        target: CredentialTarget,
    ) -> Result<DroppedOwner<B>, OwnerError> {
        if credentials.transition(target) != Ok(CredentialPrefix::Irreversible)
            || credentials.attest(target).is_err()
        {
            let cleanup = self.backend.stop();
            return Err(OwnerError::Credential { cleanup });
        }
        Ok(DroppedOwner {
            backend: self.backend,
            parameters: self.parameters,
            cleanup: None,
        })
    }
}

impl<B: OwnerBackend> fmt::Debug for PrivilegedOwner<B> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PrivilegedOwner(<redacted>)")
    }
}

/// Irreversibly privilege-dropped owner state that alone exposes packet work.
pub struct DroppedOwner<B: OwnerBackend> {
    backend: B,
    parameters: RealizedVmnetParameters,
    cleanup: Option<ProviderCleanup>,
}

impl<B: OwnerBackend> DroppedOwner<B> {
    /// Returns frozen provider-v1 parameters.
    #[must_use]
    pub const fn parameters(&self) -> RealizedVmnetParameters {
        self.parameters
    }

    /// Enables the restricted packet callback after the credential boundary.
    pub fn enable_readiness(&mut self, callback: OwnerReadinessCallback) -> Result<(), OwnerError> {
        self.backend
            .enable_readiness(callback)
            .map_err(OwnerError::Backend)
    }

    /// Reads one bounded ordered packet batch after credential drop.
    pub fn read_packets(&mut self, maximum: u16) -> Result<VmnetPacketBatch, OwnerError> {
        self.backend
            .read_packets(maximum)
            .map_err(OwnerError::Backend)
    }

    /// Writes one bounded ordered packet batch after credential drop.
    pub fn write_packets(&mut self, packets: &VmnetPacketBatch) -> Result<u16, OwnerError> {
        self.backend
            .write_packets(packets)
            .map_err(OwnerError::Backend)
    }

    /// Drains callback work and stops the backend exactly once.
    pub fn stop(&mut self) -> ProviderCleanup {
        if let Some(cleanup) = self.cleanup {
            return cleanup;
        }
        let cleanup = self.backend.stop();
        self.cleanup = Some(cleanup);
        cleanup
    }
}

impl<B: OwnerBackend> fmt::Debug for DroppedOwner<B> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DroppedOwner")
            .field("backend", &"<owned>")
            .field("parameters", &"<redacted>")
            .field("stop_attempted", &self.cleanup.is_some())
            .finish()
    }
}

impl<B: OwnerBackend> Drop for DroppedOwner<B> {
    fn drop(&mut self) {
        if self.cleanup.is_none() {
            let _ = self.backend.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use bangbang_session::credential::CredentialTarget;
    use bangbang_session::vmnet_provider::RequestedVmnetParameters;

    use super::*;

    #[derive(Clone)]
    struct Log(Arc<Mutex<Vec<&'static str>>>);

    impl Log {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }

        fn push(&self, value: &'static str) {
            self.0.lock().expect("log should lock").push(value);
        }

        fn values(&self) -> Vec<&'static str> {
            self.0.lock().expect("log should lock").clone()
        }
    }

    impl fmt::Debug for Log {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("Log(<redacted>)")
        }
    }

    thread_local! {
        static NEXT_LOG: std::cell::RefCell<Option<Log>> = const { std::cell::RefCell::new(None) };
    }

    #[derive(Debug)]
    struct FakeBackend {
        log: Log,
        stopped: bool,
    }

    impl OwnerBackend for FakeBackend {
        fn start(
            _policy: &ResolvedVmnetPolicy,
        ) -> Result<(Self, RealizedVmnetParameters), OwnerStartFailure> {
            let log = NEXT_LOG
                .with_borrow_mut(|slot| slot.take())
                .expect("test log should exist");
            log.push("start");
            log.push("freeze");
            Ok((
                Self {
                    log,
                    stopped: false,
                },
                RealizedVmnetParameters::new([2, 0, 0, 0, 0, 1], 1500, 2048)
                    .expect("parameters should validate"),
            ))
        }

        fn enable_readiness(
            &mut self,
            _callback: OwnerReadinessCallback,
        ) -> Result<(), ProviderStatus> {
            self.log.push("callback");
            Ok(())
        }

        fn read_packets(&mut self, _maximum: u16) -> Result<VmnetPacketBatch, ProviderStatus> {
            self.log.push("read");
            VmnetPacketBatch::read(&[&[1, 2, 3]]).map_err(|_| ProviderStatus::BackendFailure)
        }

        fn write_packets(&mut self, packets: &VmnetPacketBatch) -> Result<u16, ProviderStatus> {
            self.log.push("write");
            u16::try_from(packets.packet_count()).map_err(|_| ProviderStatus::TooManyPackets)
        }

        fn stop(&mut self) -> ProviderCleanup {
            if !self.stopped {
                self.log.push("stop");
                self.stopped = true;
            }
            ProviderCleanup::Complete
        }
    }

    #[derive(Debug)]
    struct FakeCredentials {
        log: Log,
        prefix: CredentialPrefix,
        attest: bool,
    }

    impl OwnerCredentialOps for FakeCredentials {
        fn transition(
            &mut self,
            _target: CredentialTarget,
        ) -> Result<CredentialPrefix, OwnerCredentialError> {
            self.log.push("drop");
            Ok(self.prefix)
        }

        fn attest(&mut self, _target: CredentialTarget) -> Result<(), OwnerCredentialError> {
            self.log.push("attest");
            self.attest.then_some(()).ok_or(OwnerCredentialError)
        }
    }

    fn policy() -> ResolvedVmnetPolicy {
        ResolvedVmnetPolicy::Shared {
            requested: RequestedVmnetParameters::new(None, None).expect("request should validate"),
        }
    }

    fn target() -> CredentialTarget {
        CredentialTarget::new(501, 20).expect("target should validate")
    }

    #[test]
    fn packet_service_exists_only_after_irreversible_drop_and_attestation() {
        let log = Log::new();
        NEXT_LOG.with_borrow_mut(|slot| *slot = Some(log.clone()));
        let privileged = PrivilegedOwner::<FakeBackend>::start(&policy()).expect("start");
        assert_eq!(log.values(), ["start", "freeze"]);
        let mut credentials = FakeCredentials {
            log: log.clone(),
            prefix: CredentialPrefix::Irreversible,
            attest: true,
        };
        let mut dropped = privileged
            .drop_credentials(&mut credentials, target())
            .expect("drop should succeed");
        dropped
            .enable_readiness(Arc::new(|_| {}))
            .expect("callback should enable");
        let packets = dropped.read_packets(1).expect("read should succeed");
        assert_eq!(dropped.write_packets(&packets), Ok(1));
        assert_eq!(dropped.stop(), ProviderCleanup::Complete);
        assert_eq!(
            log.values(),
            [
                "start", "freeze", "drop", "attest", "callback", "read", "write", "stop"
            ]
        );
    }

    #[test]
    fn incomplete_transition_stops_without_exposing_service() {
        let log = Log::new();
        NEXT_LOG.with_borrow_mut(|slot| *slot = Some(log.clone()));
        let privileged = PrivilegedOwner::<FakeBackend>::start(&policy()).expect("start");
        let mut credentials = FakeCredentials {
            log: log.clone(),
            prefix: CredentialPrefix::UidSet,
            attest: true,
        };
        assert!(matches!(
            privileged.drop_credentials(&mut credentials, target()),
            Err(OwnerError::Credential {
                cleanup: ProviderCleanup::Complete
            })
        ));
        assert_eq!(log.values(), ["start", "freeze", "drop", "stop"]);
    }
}
