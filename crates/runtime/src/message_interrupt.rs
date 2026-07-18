//! Authority-safe guest-message interrupt routing.
//!
//! A transport may retain guest-programmed message address/data values, but it
//! must not turn those values into a general-purpose host interrupt primitive.
//! [`GuestMessageInterruptRegistry`] resolves a tuple only against opaque live
//! capabilities supplied by the backend and closes admission before teardown.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex};

/// One address/data tuple programmed by a guest interrupt domain.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct GuestMessage {
    address: u64,
    data: u32,
}

impl GuestMessage {
    pub const fn new(address: u64, data: u32) -> Self {
        Self { address, data }
    }

    pub const fn address(self) -> u64 {
        self.address
    }

    pub const fn data(self) -> u32 {
        self.data
    }
}

impl fmt::Debug for GuestMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestMessage")
            .field("address", &"<redacted>")
            .field("data", &"<redacted>")
            .finish()
    }
}

/// An opaque backend-owned message route.
///
/// Implementations must revalidate `message` while signaling. `matches` is a
/// side-effect-free registry lookup hint, not authority by itself.
pub trait GuestMessageInterrupt: fmt::Debug + Send + Sync {
    fn matches(&self, message: GuestMessage) -> bool;

    fn signal(&self, message: GuestMessage) -> Result<(), GuestMessageInterruptSignalError>;
}

/// A value-redacted backend signal failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestMessageInterruptSignalError {
    message: String,
    delivery_ambiguous: bool,
}

impl GuestMessageInterruptSignalError {
    pub fn new(message: impl Into<String>, delivery_ambiguous: bool) -> Self {
        Self {
            message: message.into(),
            delivery_ambiguous,
        }
    }

    pub fn delivery_ambiguous(&self) -> bool {
        self.delivery_ambiguous
    }
}

impl fmt::Display for GuestMessageInterruptSignalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GuestMessageInterruptSignalError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMessageInterruptRegistryPhase {
    Active,
    Quiescing,
    Released,
}

struct RegistryState {
    phase: GuestMessageInterruptRegistryPhase,
    in_flight: usize,
}

struct RegistryInner {
    routes: Vec<Arc<dyn GuestMessageInterrupt>>,
    state: Mutex<RegistryState>,
    drained: Condvar,
}

/// A device-scoped set of opaque live message routes.
#[derive(Clone)]
pub struct GuestMessageInterruptRegistry {
    inner: Arc<RegistryInner>,
}

impl GuestMessageInterruptRegistry {
    pub fn new(
        routes: Vec<Arc<dyn GuestMessageInterrupt>>,
    ) -> Result<Self, GuestMessageInterruptRegistryError> {
        if routes.is_empty() {
            return Err(GuestMessageInterruptRegistryError::Empty);
        }

        Ok(Self {
            inner: Arc::new(RegistryInner {
                routes,
                state: Mutex::new(RegistryState {
                    phase: GuestMessageInterruptRegistryPhase::Active,
                    in_flight: 0,
                }),
                drained: Condvar::new(),
            }),
        })
    }

    pub fn route_count(&self) -> usize {
        self.inner.routes.len()
    }

    pub fn phase(
        &self,
    ) -> Result<GuestMessageInterruptRegistryPhase, GuestMessageInterruptRegistryError> {
        self.inner
            .state
            .lock()
            .map(|state| state.phase)
            .map_err(|_| GuestMessageInterruptRegistryError::StatePoisoned)
    }

    /// Resolve and signal one tuple while holding an in-flight lifecycle guard.
    pub fn signal(&self, message: GuestMessage) -> Result<(), GuestMessageInterruptRegistryError> {
        let route = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| GuestMessageInterruptRegistryError::StatePoisoned)?;
            if state.phase != GuestMessageInterruptRegistryPhase::Active {
                return Err(GuestMessageInterruptRegistryError::NotActive { phase: state.phase });
            }

            let mut matches = self
                .inner
                .routes
                .iter()
                .filter(|route| route.matches(message));
            let route = matches
                .next()
                .cloned()
                .ok_or(GuestMessageInterruptRegistryError::UnknownMessage)?;
            if matches.next().is_some() {
                return Err(GuestMessageInterruptRegistryError::AmbiguousMessage);
            }
            state.in_flight = state
                .in_flight
                .checked_add(1)
                .ok_or(GuestMessageInterruptRegistryError::InFlightOverflow)?;
            route
        };

        let _guard = InFlightRegistrySignal {
            inner: Arc::clone(&self.inner),
        };
        route
            .signal(message)
            .map_err(|source| GuestMessageInterruptRegistryError::Signal { source })
    }

    /// Close admission without waiting for already admitted sends.
    pub fn begin_quiesce(&self) -> Result<(), GuestMessageInterruptRegistryError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| GuestMessageInterruptRegistryError::StatePoisoned)?;
        match state.phase {
            GuestMessageInterruptRegistryPhase::Active => {
                state.phase = GuestMessageInterruptRegistryPhase::Quiescing;
                Ok(())
            }
            GuestMessageInterruptRegistryPhase::Quiescing => Ok(()),
            GuestMessageInterruptRegistryPhase::Released => {
                Err(GuestMessageInterruptRegistryError::NotActive { phase: state.phase })
            }
        }
    }

    /// Close admission, wait for all admitted sends, and invalidate the set.
    pub fn release(&self) -> Result<(), GuestMessageInterruptRegistryError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| GuestMessageInterruptRegistryError::StatePoisoned)?;
        if state.phase == GuestMessageInterruptRegistryPhase::Released {
            return Ok(());
        }
        state.phase = GuestMessageInterruptRegistryPhase::Quiescing;
        while state.in_flight != 0 {
            state = self
                .inner
                .drained
                .wait(state)
                .map_err(|_| GuestMessageInterruptRegistryError::StatePoisoned)?;
        }
        state.phase = GuestMessageInterruptRegistryPhase::Released;
        Ok(())
    }
}

impl fmt::Debug for GuestMessageInterruptRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestMessageInterruptRegistry")
            .field("route_count", &self.inner.routes.len())
            .field("routes", &"<redacted>")
            .finish_non_exhaustive()
    }
}

struct InFlightRegistrySignal {
    inner: Arc<RegistryInner>,
}

impl Drop for InFlightRegistrySignal {
    fn drop(&mut self) {
        let mut state = match self.inner.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        state.in_flight = state.in_flight.saturating_sub(1);
        if state.in_flight == 0 {
            self.inner.drained.notify_all();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestMessageInterruptRegistryError {
    Empty,
    StatePoisoned,
    InFlightOverflow,
    NotActive {
        phase: GuestMessageInterruptRegistryPhase,
    },
    UnknownMessage,
    AmbiguousMessage,
    Signal {
        source: GuestMessageInterruptSignalError,
    },
}

impl fmt::Display for GuestMessageInterruptRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("guest-message interrupt registry cannot be empty"),
            Self::StatePoisoned => {
                f.write_str("guest-message interrupt registry state is unavailable")
            }
            Self::InFlightOverflow => {
                f.write_str("guest-message interrupt registry in-flight count overflowed")
            }
            Self::NotActive { phase } => {
                write!(
                    f,
                    "guest-message interrupt registry is not active ({phase:?})"
                )
            }
            Self::UnknownMessage => {
                f.write_str("guest-message interrupt does not match a live route")
            }
            Self::AmbiguousMessage => {
                f.write_str("guest-message interrupt matches multiple live routes")
            }
            Self::Signal { source } => {
                write!(f, "guest-message interrupt delivery failed: {source}")
            }
        }
    }
}

impl std::error::Error for GuestMessageInterruptRegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Signal { source } => Some(source),
            Self::Empty
            | Self::StatePoisoned
            | Self::InFlightOverflow
            | Self::NotActive { .. }
            | Self::UnknownMessage
            | Self::AmbiguousMessage => None,
        }
    }
}

/// Owns the backend resources represented by one device-scoped registry.
///
/// Transport teardown first closes and drains the cloned registry held by the
/// endpoint, then calls [`GuestMessageInterruptResources::release`] to return
/// the underlying backend leases.
pub trait GuestMessageInterruptResources: fmt::Debug + Send {
    fn registry(&self) -> GuestMessageInterruptRegistry;

    fn release(&mut self) -> Result<(), GuestMessageInterruptResourcesError>;
}

/// Registry-only resource owner useful for host-independent endpoints.
#[derive(Debug)]
pub struct RegistryGuestMessageInterruptResources {
    registry: GuestMessageInterruptRegistry,
}

impl RegistryGuestMessageInterruptResources {
    pub fn new(registry: GuestMessageInterruptRegistry) -> Self {
        Self { registry }
    }
}

impl GuestMessageInterruptResources for RegistryGuestMessageInterruptResources {
    fn registry(&self) -> GuestMessageInterruptRegistry {
        self.registry.clone()
    }

    fn release(&mut self) -> Result<(), GuestMessageInterruptResourcesError> {
        self.registry
            .release()
            .map_err(|source| GuestMessageInterruptResourcesError::new(source.to_string()))
    }
}

/// Value-redacted failure while returning backend message-interrupt leases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestMessageInterruptResourcesError {
    message: String,
}

impl GuestMessageInterruptResourcesError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for GuestMessageInterruptResourcesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GuestMessageInterruptResourcesError {}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[derive(Debug)]
    struct RecordingRoute {
        message: GuestMessage,
        sent: Arc<Mutex<Vec<GuestMessage>>>,
        failure: Option<GuestMessageInterruptSignalError>,
    }

    impl GuestMessageInterrupt for RecordingRoute {
        fn matches(&self, message: GuestMessage) -> bool {
            self.message == message
        }

        fn signal(&self, message: GuestMessage) -> Result<(), GuestMessageInterruptSignalError> {
            if !self.matches(message) {
                return Err(GuestMessageInterruptSignalError::new(
                    "route rejected mismatched message",
                    false,
                ));
            }
            if let Some(failure) = self.failure.clone() {
                return Err(failure);
            }
            self.sent
                .lock()
                .expect("recording lock should work")
                .push(message);
            Ok(())
        }
    }

    fn route(
        message: GuestMessage,
    ) -> (
        Arc<Mutex<Vec<GuestMessage>>>,
        Arc<dyn GuestMessageInterrupt>,
    ) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (
            Arc::clone(&sent),
            Arc::new(RecordingRoute {
                message,
                sent,
                failure: None,
            }),
        )
    }

    #[test]
    fn guest_message_debug_redacts_both_values() {
        let debug = format!("{:?}", GuestMessage::new(0x3ffc_0040, 126));
        assert_eq!(
            debug,
            "GuestMessage { address: \"<redacted>\", data: \"<redacted>\" }"
        );
        assert!(!debug.contains("3ffc"));
        assert!(!debug.contains("126"));
    }

    #[test]
    fn registry_requires_at_least_one_opaque_route() {
        assert!(matches!(
            GuestMessageInterruptRegistry::new(Vec::new()),
            Err(GuestMessageInterruptRegistryError::Empty)
        ));
    }

    #[test]
    fn registry_resolves_only_the_exact_unique_route() {
        let first = GuestMessage::new(0x1000, 32);
        let second = GuestMessage::new(0x1000, 33);
        let (first_sent, first_route) = route(first);
        let (second_sent, second_route) = route(second);
        let registry = GuestMessageInterruptRegistry::new(vec![first_route, second_route])
            .expect("registry should build");

        registry.signal(second).expect("second route should signal");

        assert!(
            first_sent
                .lock()
                .expect("first lock should work")
                .is_empty()
        );
        assert_eq!(
            *second_sent.lock().expect("second lock should work"),
            vec![second]
        );
        assert_eq!(
            registry.signal(GuestMessage::new(0x1000, 34)),
            Err(GuestMessageInterruptRegistryError::UnknownMessage)
        );
    }

    #[test]
    fn registry_rejects_ambiguous_authority() {
        let message = GuestMessage::new(0x1000, 32);
        let (_, first) = route(message);
        let (_, second) = route(message);
        let registry =
            GuestMessageInterruptRegistry::new(vec![first, second]).expect("registry should build");

        assert_eq!(
            registry.signal(message),
            Err(GuestMessageInterruptRegistryError::AmbiguousMessage)
        );
    }

    #[test]
    fn duplicate_table_use_can_signal_the_same_registry_route_repeatedly() {
        let message = GuestMessage::new(0x1000, 32);
        let (sent, route) = route(message);
        let registry =
            GuestMessageInterruptRegistry::new(vec![route]).expect("registry should build");

        registry.signal(message).expect("first signal should work");
        registry.signal(message).expect("second signal should work");

        assert_eq!(
            *sent.lock().expect("recording lock should work"),
            vec![message, message]
        );
    }

    #[test]
    fn signal_failure_preserves_delivery_ambiguity_without_values() {
        let message = GuestMessage::new(0xfeed_0000, 77);
        let registry = GuestMessageInterruptRegistry::new(vec![Arc::new(RecordingRoute {
            message,
            sent: Arc::new(Mutex::new(Vec::new())),
            failure: Some(GuestMessageInterruptSignalError::new(
                "backend delivery failed",
                true,
            )),
        })])
        .expect("registry should build");

        let error = registry
            .signal(message)
            .expect_err("backend failure should propagate");
        assert!(matches!(
            error,
            GuestMessageInterruptRegistryError::Signal { ref source }
                if source.delivery_ambiguous()
        ));
        assert!(!error.to_string().contains("feed"));
        assert!(!error.to_string().contains("77"));
    }

    #[test]
    fn release_closes_admission_and_is_idempotent() {
        let message = GuestMessage::new(0x1000, 32);
        let (_, route) = route(message);
        let registry =
            GuestMessageInterruptRegistry::new(vec![route]).expect("registry should build");

        registry.begin_quiesce().expect("quiesce should start");
        assert_eq!(
            registry.signal(message),
            Err(GuestMessageInterruptRegistryError::NotActive {
                phase: GuestMessageInterruptRegistryPhase::Quiescing,
            })
        );
        registry.release().expect("release should finish");
        registry.release().expect("release should be idempotent");
        assert_eq!(
            registry.phase().expect("phase should be available"),
            GuestMessageInterruptRegistryPhase::Released
        );
    }

    #[test]
    fn release_waits_for_an_in_flight_signal() {
        #[derive(Debug)]
        struct BlockingRoute {
            message: GuestMessage,
            entered: Mutex<Option<mpsc::SyncSender<()>>>,
            release: Mutex<mpsc::Receiver<()>>,
        }

        impl GuestMessageInterrupt for BlockingRoute {
            fn matches(&self, message: GuestMessage) -> bool {
                self.message == message
            }

            fn signal(
                &self,
                message: GuestMessage,
            ) -> Result<(), GuestMessageInterruptSignalError> {
                assert_eq!(message, self.message);
                if let Some(entered) = self
                    .entered
                    .lock()
                    .expect("entered lock should work")
                    .take()
                {
                    entered.send(()).expect("entry should be observed");
                }
                self.release
                    .lock()
                    .expect("release lock should work")
                    .recv()
                    .expect("test should release the signal");
                Ok(())
            }
        }

        let message = GuestMessage::new(0x1000, 32);
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let registry = GuestMessageInterruptRegistry::new(vec![Arc::new(BlockingRoute {
            message,
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(release_rx),
        })])
        .expect("registry should build");

        std::thread::scope(|scope| {
            let sending = registry.clone();
            let signal = scope.spawn(move || sending.signal(message));
            entered_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("signal should enter route");

            let releasing = registry.clone();
            let (released_tx, released_rx) = mpsc::sync_channel(1);
            let release = scope.spawn(move || {
                let result = releasing.release();
                released_tx
                    .send(())
                    .expect("release result should be observed");
                result
            });
            assert!(matches!(
                released_rx.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));

            release_tx.send(()).expect("blocked route should resume");
            signal
                .join()
                .expect("signal thread should finish")
                .expect("signal should succeed");
            released_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("release should finish after signal");
            release
                .join()
                .expect("release thread should finish")
                .expect("release should succeed");
        });
    }
}
