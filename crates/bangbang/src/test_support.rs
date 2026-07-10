use bangbang_runtime::balloon::BalloonMmioLayout;
use bangbang_runtime::block::BlockMmioLayout;
use bangbang_runtime::fdt::{Arm64FdtGic, Arm64FdtRegion, Arm64FdtTimerInterrupts};
use bangbang_runtime::interrupt::GuestInterruptLine;
use bangbang_runtime::memory::GuestAddress;
use bangbang_runtime::mmio::MmioRegionId;
use bangbang_runtime::network::NetworkMmioLayout;
use bangbang_runtime::pmem::PmemMmioLayout;
use bangbang_runtime::startup::Arm64BootResourceConfig;
use bangbang_runtime::vsock::VsockMmioLayout;

pub(crate) fn minimal_arm64_boot_resource_config() -> Arm64BootResourceConfig<'static> {
    const VCPU_MPIDRS: &[u64] = &[0x8000_0000];
    const INTERRUPT_LINES: &[GuestInterruptLine] = &[];

    Arm64BootResourceConfig {
        vcpu_mpidrs: VCPU_MPIDRS,
        gic: Arm64FdtGic {
            distributor: Arm64FdtRegion {
                base: 0x0800_0000,
                size: 0x1_0000,
            },
            redistributor: Arm64FdtRegion {
                base: 0x080a_0000,
                size: 0xf6_0000,
            },
            compatibility: "arm,gic-v3",
            interrupt_cells: 3,
            maintenance_irq: 25,
            msi: None,
        },
        timer: Arm64FdtTimerInterrupts::firecracker_default(),
        rtc_device: None,
        serial_device: None,
        vmgenid_interrupt_line: GuestInterruptLine::new(127)
            .expect("test VMGenID interrupt line should be valid"),
        block_mmio_layout: BlockMmioLayout::new(
            GuestAddress::new(0x1000_0000),
            MmioRegionId::new(1000),
        ),
        block_interrupt_lines: INTERRUPT_LINES,
        pmem_mmio_layout: PmemMmioLayout::new(
            GuestAddress::new(0x2000_0000),
            MmioRegionId::new(2000),
        ),
        pmem_interrupt_lines: INTERRUPT_LINES,
        network_mmio_layout: NetworkMmioLayout::new(
            GuestAddress::new(0x3000_0000),
            MmioRegionId::new(3000),
        ),
        network_interrupt_lines: INTERRUPT_LINES,
        vsock_mmio_layout: VsockMmioLayout::new(
            GuestAddress::new(0x4000_0000),
            MmioRegionId::new(4000),
        ),
        vsock_interrupt_line: None,
        balloon_mmio_layout: BalloonMmioLayout::new(
            GuestAddress::new(0x5000_0000),
            MmioRegionId::new(5000),
        ),
        balloon_interrupt_line: None,
        memory_hotplug_device: None,
        entropy_device: None,
    }
}
