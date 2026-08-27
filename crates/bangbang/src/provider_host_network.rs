//! Narrow source view of the system vmnet backend for the provider process.

#[cfg(test)]
#[allow(dead_code, unused_imports)]
#[path = "host_network/virtio_vmnet.rs"]
pub mod virtio_vmnet;

#[cfg_attr(test, allow(dead_code))]
#[path = "host_network/vmnet.rs"]
pub mod vmnet;

// The full VMM view calls this bounded clone from `virtio_vmnet`; keep the
// shared backend's crate-private method live without linking that adapter into
// the provider-only library view.
const _: fn(
    &vmnet::VmnetInterfaceConfig,
) -> Result<vmnet::VmnetInterfaceConfig, std::collections::TryReserveError> =
    vmnet::VmnetInterfaceConfig::try_clone;
