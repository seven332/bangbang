//! Test-only source view of the product host-network modules.

#[path = "../../src/host_network/virtio_vmnet.rs"]
#[allow(dead_code, unused_imports)]
pub mod virtio_vmnet;
#[path = "../../src/host_network/vmnet.rs"]
#[allow(clippy::enum_variant_names, dead_code, unused_imports)]
pub mod vmnet;
