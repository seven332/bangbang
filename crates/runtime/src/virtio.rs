//! Transport-neutral virtio device state and lifecycle primitives.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex};

use crate::virtio_mmio::{
    NoopVirtioMmioDeviceActivation, UnsupportedVirtioMmioDeviceConfig,
    VirtioMmioDeviceActivationHandler, VirtioMmioDeviceConfigHandler, VirtioMmioDeviceRegisters,
    VirtioMmioQueueNotificationRegisters, VirtioMmioQueueRegisters,
};

pub use crate::virtio_mmio::{
    NoopVirtioMmioDeviceActivation as NoopVirtioDeviceActivation,
    UnsupportedVirtioMmioDeviceConfig as UnsupportedVirtioDeviceConfig,
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DEVICE_NEEDS_RESET,
    VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FAILED,
    VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT, VIRTIO_MMIO_VERSION_1_FEATURE,
    VirtioMmioDeviceActivation as VirtioDeviceActivation,
    VirtioMmioDeviceActivationError as VirtioDeviceActivationError,
    VirtioMmioDeviceActivationHandler as VirtioDeviceActivationHandler,
    VirtioMmioDeviceConfigAccess as VirtioDeviceConfigAccess,
    VirtioMmioDeviceConfigError as VirtioDeviceConfigError,
    VirtioMmioDeviceConfigHandler as VirtioDeviceConfigHandler,
    VirtioMmioDeviceRegisters as VirtioDeviceRegisters,
    VirtioMmioDeviceResetError as VirtioDeviceResetError,
    VirtioMmioDeviceResetOutcome as VirtioDeviceResetOutcome,
    VirtioMmioQueueNotificationError as VirtioQueueNotificationError,
    VirtioMmioQueueNotificationRegisters as VirtioQueueNotificationState,
    VirtioMmioQueueRegisterError as VirtioQueueError, VirtioMmioQueueRegisters as VirtioQueues,
    VirtioMmioQueueState as VirtioQueueState,
};

/// First modern virtio PCI device identifier assigned to device type zero.
pub const VIRTIO_PCI_MODERN_DEVICE_ID_BASE: u16 = 0x1040;

/// Stable virtio device type shared by all transport adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VirtioDeviceType(u32, u16);

impl VirtioDeviceType {
    /// Validates a nonzero virtio device type whose modern PCI identifier fits.
    pub fn new(value: u32) -> Result<Self, VirtioDeviceTypeError> {
        if value == 0 {
            return Err(VirtioDeviceTypeError::Zero);
        }
        let pci_id = u32::from(VIRTIO_PCI_MODERN_DEVICE_ID_BASE)
            .checked_add(value)
            .ok_or(VirtioDeviceTypeError::PciDeviceIdOverflow { value })?;
        let pci_id = u16::try_from(pci_id)
            .map_err(|_| VirtioDeviceTypeError::PciDeviceIdOverflow { value })?;
        Ok(Self(value, pci_id))
    }

    /// Returns the virtio specification device type.
    pub const fn raw_value(self) -> u32 {
        self.0
    }

    /// Returns the modern, non-transitional PCI device identifier.
    pub fn modern_pci_device_id(self) -> u16 {
        self.1
    }
}

/// Failure while validating a stable virtio device type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioDeviceTypeError {
    /// Device type zero is reserved.
    Zero,
    /// Adding the modern PCI base would exceed a PCI device identifier.
    PciDeviceIdOverflow { value: u32 },
}

impl fmt::Display for VirtioDeviceTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => f.write_str("virtio device type zero is reserved"),
            Self::PciDeviceIdOverflow { value } => {
                write!(
                    f,
                    "virtio device type {value} has no modern PCI device identifier"
                )
            }
        }
    }
}

impl std::error::Error for VirtioDeviceTypeError {}

/// Transport-neutral interrupt intent emitted by device work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VirtioInterruptIntent {
    /// Completion on one concrete virtqueue.
    Queue { queue_index: u16 },
    /// Device configuration changed.
    Configuration,
}

#[derive(Debug, Default)]
struct VirtioDeviceWorkState {
    accepting: bool,
    in_flight: usize,
}

#[derive(Debug, Default)]
struct VirtioDeviceWorkGateInner {
    state: Mutex<VirtioDeviceWorkState>,
    drained: Condvar,
}

/// Shared admission and drain gate for device-owned queue and update work.
#[derive(Debug, Clone)]
pub struct VirtioDeviceWorkGate {
    inner: Arc<VirtioDeviceWorkGateInner>,
}

impl Default for VirtioDeviceWorkGate {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtioDeviceWorkGate {
    /// Creates an active gate that admits work.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(VirtioDeviceWorkGateInner {
                state: Mutex::new(VirtioDeviceWorkState {
                    accepting: true,
                    in_flight: 0,
                }),
                drained: Condvar::new(),
            }),
        }
    }

    /// Admits one operation until its returned guard is dropped.
    pub fn admit(&self) -> Result<VirtioDeviceWorkGuard, VirtioDeviceWorkGateError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| VirtioDeviceWorkGateError::Poisoned)?;
        if !state.accepting {
            return Err(VirtioDeviceWorkGateError::Quiescing);
        }
        state.in_flight = state
            .in_flight
            .checked_add(1)
            .ok_or(VirtioDeviceWorkGateError::InFlightOverflow)?;
        drop(state);
        Ok(VirtioDeviceWorkGuard {
            inner: Arc::clone(&self.inner),
        })
    }

    /// Stops admission and blocks until every previously admitted operation drains.
    pub fn quiesce_and_wait(&self) -> Result<(), VirtioDeviceWorkGateError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| VirtioDeviceWorkGateError::Poisoned)?;
        state.accepting = false;
        while state.in_flight != 0 {
            state = self
                .inner
                .drained
                .wait(state)
                .map_err(|_| VirtioDeviceWorkGateError::Poisoned)?;
        }
        Ok(())
    }

    /// Reopens a fully drained gate after a recoverable teardown abort.
    pub fn resume(&self) -> Result<(), VirtioDeviceWorkGateError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| VirtioDeviceWorkGateError::Poisoned)?;
        if state.in_flight != 0 {
            return Err(VirtioDeviceWorkGateError::InFlightWorkRemains);
        }
        state.accepting = true;
        Ok(())
    }

    /// Returns whether new device work is currently accepted.
    pub fn is_accepting(&self) -> Result<bool, VirtioDeviceWorkGateError> {
        self.inner
            .state
            .lock()
            .map(|state| state.accepting)
            .map_err(|_| VirtioDeviceWorkGateError::Poisoned)
    }
}

/// One admitted device operation.
#[derive(Debug)]
pub struct VirtioDeviceWorkGuard {
    inner: Arc<VirtioDeviceWorkGateInner>,
}

impl Drop for VirtioDeviceWorkGuard {
    fn drop(&mut self) {
        let Ok(mut state) = self.inner.state.lock() else {
            return;
        };
        debug_assert!(state.in_flight > 0);
        state.in_flight = state.in_flight.saturating_sub(1);
        if state.in_flight == 0 {
            self.inner.drained.notify_all();
        }
    }
}

/// Failure while admitting or draining transport-neutral device work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioDeviceWorkGateError {
    Quiescing,
    InFlightOverflow,
    InFlightWorkRemains,
    Poisoned,
}

impl fmt::Display for VirtioDeviceWorkGateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Quiescing => f.write_str("virtio device work is quiescing"),
            Self::InFlightOverflow => f.write_str("virtio device in-flight work count overflowed"),
            Self::InFlightWorkRemains => f.write_str("virtio device still has in-flight work"),
            Self::Poisoned => f.write_str("virtio device work gate is poisoned"),
        }
    }
}

impl std::error::Error for VirtioDeviceWorkGateError {}

/// Canonical common state shared by virtio transport adapters.
#[derive(Debug)]
pub struct VirtioDeviceCore<
    C = UnsupportedVirtioMmioDeviceConfig,
    A = NoopVirtioMmioDeviceActivation,
> {
    pub(crate) device: VirtioMmioDeviceRegisters,
    pub(crate) queues: VirtioMmioQueueRegisters,
    pub(crate) queue_notifications: VirtioMmioQueueNotificationRegisters,
    pub(crate) device_config: C,
    pub(crate) activation: A,
    pub(crate) device_activated: bool,
    pub(crate) requires_device_config_write_status: bool,
    pub(crate) interrupt_intents: Vec<VirtioInterruptIntent>,
    pub(crate) work_gate: VirtioDeviceWorkGate,
}

impl<C: Clone, A: Clone> Clone for VirtioDeviceCore<C, A> {
    fn clone(&self) -> Self {
        Self {
            device: self.device,
            queues: self.queues.clone(),
            queue_notifications: self.queue_notifications.clone(),
            device_config: self.device_config.clone(),
            activation: self.activation.clone(),
            device_activated: self.device_activated,
            requires_device_config_write_status: self.requires_device_config_write_status,
            interrupt_intents: self.interrupt_intents.clone(),
            // A cloned device state is a distinct lifecycle owner, matching
            // the pre-extraction handler clone behavior.
            work_gate: VirtioDeviceWorkGate::new(),
        }
    }
}

impl<C: PartialEq, A: PartialEq> PartialEq for VirtioDeviceCore<C, A> {
    fn eq(&self, other: &Self) -> bool {
        self.device == other.device
            && self.queues == other.queues
            && self.queue_notifications == other.queue_notifications
            && self.device_config == other.device_config
            && self.activation == other.activation
            && self.device_activated == other.device_activated
            && self.requires_device_config_write_status == other.requires_device_config_write_status
            && self.interrupt_intents == other.interrupt_intents
    }
}

impl<C: Eq, A: Eq> Eq for VirtioDeviceCore<C, A> {}

impl<C: VirtioMmioDeviceConfigHandler, A: VirtioMmioDeviceActivationHandler>
    VirtioDeviceCore<C, A>
{
    pub(crate) fn from_parts(
        device: VirtioMmioDeviceRegisters,
        queues: VirtioMmioQueueRegisters,
        queue_notifications: VirtioMmioQueueNotificationRegisters,
        device_config: C,
        activation: A,
        requires_device_config_write_status: bool,
    ) -> Self {
        Self {
            device,
            queues,
            queue_notifications,
            device_config,
            activation,
            device_activated: false,
            requires_device_config_write_status,
            interrupt_intents: Vec::new(),
            work_gate: VirtioDeviceWorkGate::new(),
        }
    }

    pub const fn device_registers(&self) -> &VirtioMmioDeviceRegisters {
        &self.device
    }

    pub const fn queue_registers(&self) -> &VirtioMmioQueueRegisters {
        &self.queues
    }

    pub const fn queue_notification_state(&self) -> &VirtioMmioQueueNotificationRegisters {
        &self.queue_notifications
    }

    pub const fn device_config_handler(&self) -> &C {
        &self.device_config
    }

    pub const fn activation_handler(&self) -> &A {
        &self.activation
    }

    pub const fn is_device_activated(&self) -> bool {
        self.device_activated
    }

    pub fn record_interrupt_intent(&mut self, intent: VirtioInterruptIntent) {
        if !self.interrupt_intents.contains(&intent) {
            self.interrupt_intents.push(intent);
        }
    }

    pub fn take_interrupt_intents(&mut self) -> Vec<VirtioInterruptIntent> {
        std::mem::take(&mut self.interrupt_intents)
    }

    pub fn work_gate(&self) -> &VirtioDeviceWorkGate {
        &self.work_gate
    }

    pub(crate) fn replace_common_state(
        &mut self,
        device: VirtioMmioDeviceRegisters,
        queues: VirtioMmioQueueRegisters,
        queue_notifications: VirtioMmioQueueNotificationRegisters,
        activated: bool,
    ) {
        self.device = device;
        self.queues = queues;
        self.queue_notifications = queue_notifications;
        self.device_activated = activated;
        self.interrupt_intents.clear();
    }

    pub(crate) fn reset_common_state(&mut self) {
        self.queues.reset();
        self.queue_notifications.reset();
        self.device_activated = false;
        self.interrupt_intents.clear();
        self.activation.reset();
    }

    pub(crate) fn reset_common_state_with_outcome(
        &mut self,
    ) -> Result<VirtioDeviceResetOutcome, VirtioDeviceResetError> {
        let outcome = self.activation.reset_outcome()?;
        if outcome == VirtioDeviceResetOutcome::Reset {
            self.queues.reset();
            self.queue_notifications.reset();
            self.device_activated = false;
            self.interrupt_intents.clear();
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::{
        VIRTIO_PCI_MODERN_DEVICE_ID_BASE, VirtioDeviceType, VirtioDeviceTypeError,
        VirtioDeviceWorkGate, VirtioDeviceWorkGateError,
    };

    #[test]
    fn device_type_derives_modern_pci_identifier() {
        let device_type = VirtioDeviceType::new(4).expect("virtio-rng type should validate");
        assert_eq!(device_type.raw_value(), 4);
        assert_eq!(
            device_type.modern_pci_device_id(),
            VIRTIO_PCI_MODERN_DEVICE_ID_BASE + 4
        );
        assert_eq!(VirtioDeviceType::new(0), Err(VirtioDeviceTypeError::Zero));
        assert!(matches!(
            VirtioDeviceType::new(u32::MAX),
            Err(VirtioDeviceTypeError::PciDeviceIdOverflow { .. })
        ));
    }

    #[test]
    fn work_gate_rejects_new_work_after_draining_admitted_work() {
        let gate = Arc::new(VirtioDeviceWorkGate::new());
        let guard = gate.admit().expect("active gate should admit work");
        let other = Arc::clone(&gate);
        let waiter = thread::spawn(move || other.quiesce_and_wait());

        while gate
            .is_accepting()
            .expect("gate state should remain readable")
        {
            thread::yield_now();
        }
        assert!(matches!(
            gate.admit(),
            Err(VirtioDeviceWorkGateError::Quiescing)
        ));
        assert_eq!(
            gate.resume(),
            Err(VirtioDeviceWorkGateError::InFlightWorkRemains)
        );
        drop(guard);
        waiter
            .join()
            .expect("quiesce thread should join")
            .expect("admitted work should drain");
        gate.resume().expect("drained gate should resume");
        assert!(gate.admit().is_ok());
    }
}
