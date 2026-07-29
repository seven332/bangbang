use std::io::Cursor;

use bangbang_runtime::machine::MachineConfigInput;
use bangbang_runtime::memory::{GuestMemory, aarch64};
use bangbang_runtime::pci::{PCI_BAR64_START, PCI_FIRST_ENDPOINT_DEVICE, PCI_LAST_ENDPOINT_DEVICE};
use bangbang_runtime::snapshot_balloon_v2_9::{
    NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, NATIVE_V2_BALLOON_STATE_HEADER_BYTES,
    NATIVE_V2_BALLOON_STATE_SECTION_ENTRY_BYTES,
};
use bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceTransportKind;
use bangbang_runtime::snapshot_device_v2_5::NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_BYTES;
use bangbang_runtime::snapshot_device_v2_6::{
    NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    NATIVE_V2_STORAGE_DEVICE_GRAPH_SECTION_ENTRY_BYTES, SnapshotV2StorageDeviceGraph,
};
use bangbang_runtime::snapshot_entropy_v2_8::{
    NATIVE_V2_ENTROPY_STATE_HEADER_BYTES, NATIVE_V2_ENTROPY_STATE_SECTION_ENTRY_BYTES,
    SnapshotV2EntropyStateDecodeError,
};
use bangbang_runtime::snapshot_format_v2::{
    NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES, NATIVE_V2_LEGACY_PLATFORM_VERSION,
    NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES, NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES,
    NATIVE_V2_VCPU_COMPONENT_KIND, SnapshotV2ComponentKey, SnapshotV2DecodeError,
    decode_snapshot_v2_state, decode_snapshot_v2_state_with_compatibility_version,
    encode_snapshot_v2_state_with_compatibility_version,
};
use bangbang_runtime::snapshot_memory_v2::{
    decode_snapshot_v2_memory_binding, write_snapshot_v2_memory_image_with_compatibility_version,
};
use bangbang_runtime::snapshot_serial_v2_7::{
    NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION, SnapshotV2SerialState,
    SnapshotV2SerialStateDecodeError,
};
use bangbang_runtime::virtio_pci::VIRTIO_PCI_CAPABILITY_BAR_SIZE;

use super::*;
use crate::snapshot_bundle::tests::fixture as native_v1_fixture;

const FIXTURE_MEMORY_MIB: u64 = 4;
pub(crate) const MMIO_GRAPH_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_device_v2/fixtures/mmio.hex");
pub(crate) const PCI_GRAPH_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_device_v2/fixtures/pci.hex");
const MULTI_BLOCK_MMIO_GRAPH_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_device_v2_5/fixtures/root-mmio.hex");
const MULTI_BLOCK_PCI_GRAPH_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_device_v2_5/fixtures/rootless-pci.hex");
const STORAGE_MMIO_GRAPH_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_device_v2_6/fixtures/pmem-root-mmio.hex");
const STORAGE_PCI_GRAPH_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_device_v2_6/fixtures/mixed-pmem-root-pci.hex");
const SERIAL_DEFAULT_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_serial_v2_7/fixtures/default.hex");
const SERIAL_CONFIGURED_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_serial_v2_7/fixtures/configured.hex");
const ENTROPY_INACTIVE_MMIO_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_entropy_v2_8/fixtures/inactive-mmio.hex");
const ENTROPY_ACTIVE_PCI_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_entropy_v2_8/fixtures/active-pci.hex");
const BALLOON_INACTIVE_MMIO_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_balloon_v2_9/fixtures/inactive-mmio.hex");
const BALLOON_ACTIVE_PCI_FIXTURE_HEX: &str =
    include_str!("../../../runtime/src/snapshot_balloon_v2_9/fixtures/active-pci.hex");
const DETERMINISTIC_MEMORY_IMAGE_ID: [u8; 16] = *b"v2.4-fixture-id!";
const DETERMINISTIC_MULTI_BLOCK_MEMORY_IMAGE_ID: [u8; 16] = *b"v2.5-fixture-id!";
const DETERMINISTIC_STORAGE_MEMORY_IMAGE_ID: [u8; 16] = *b"v2.6-fixture-id!";
const DETERMINISTIC_SERIAL_MEMORY_IMAGE_ID: [u8; 16] = *b"v2.7-fixture-id!";
const DETERMINISTIC_ENTROPY_MEMORY_IMAGE_ID: [u8; 16] = *b"v2.8-fixture-id!";
const DETERMINISTIC_BALLOON_MEMORY_IMAGE_ID: [u8; 16] = *b"v2.9-fixture-id!";
const COMPLETE_STATE_FINGERPRINTS: [(usize, u64); 2] = [
    (4_887, 10_136_861_786_457_474_800),
    (4_983, 7_169_128_621_506_763_529),
];

pub(crate) fn platform_fixture(with_sme: bool) -> HvfSnapshotV2PlatformState {
    platform_fixture_with_count(2, with_sme)
}

fn platform_fixture_with_count(vcpu_count: u8, with_sme: bool) -> HvfSnapshotV2PlatformState {
    let (_, compatibility, mandatory, interrupts, devices) = native_v1_fixture().into_parts();
    let source_identification = compatibility.identification();
    let pfr0 = if with_sme {
        source_identification.id_aa64pfr0_el1() & !(0xf << 32)
    } else {
        source_identification.id_aa64pfr0_el1()
    };
    let pfr1 = if with_sme {
        (source_identification.id_aa64pfr1_el1() & !(0xf << 24)) | (1 << 24)
    } else {
        source_identification.id_aa64pfr1_el1()
    };
    let identification = HvfArm64VcpuIdentificationRegisterState::new([
        source_identification.midr_el1(),
        0,
        pfr0,
        pfr1,
        source_identification.id_aa64dfr0_el1(),
        source_identification.id_aa64dfr1_el1(),
        source_identification.id_aa64isar0_el1(),
        source_identification.id_aa64isar1_el1(),
        source_identification.id_aa64mmfr0_el1(),
        source_identification.id_aa64mmfr1_el1(),
        source_identification.id_aa64mmfr2_el1(),
    ]);
    let optional_identification =
        with_sme.then(|| HvfArm64VcpuSveSmeIdentificationRegisterState::new(0x1234, 0x5678));
    let mut gic_metadata = compatibility.gic_metadata();
    gic_metadata.redistributor.region.size =
        u64::from(vcpu_count) * gic_metadata.redistributor.single_redistributor_size;
    let compatibility = HvfSnapshotV1CompatibilityState::new(
        identification,
        optional_identification,
        compatibility.cache_manifest(),
        0,
        gic_metadata,
        compatibility.rtc_mmio_layout(),
    );

    let machine = MachineConfigInput::new(vcpu_count, FIXTURE_MEMORY_MIB)
        .validate()
        .expect("fixture machine should validate");
    let memory_bytes = FIXTURE_MEMORY_MIB * MIB;
    let layout = aarch64::dram_layout(memory_bytes).expect("fixture memory layout should validate");
    let memory = GuestMemory::allocate(&layout).expect("fixture guest memory should allocate");
    let memory_binding = write_snapshot_v2_memory_image_with_compatibility_version(
        &memory,
        &mut Cursor::new(Vec::new()),
        NATIVE_V2_LEGACY_PLATFORM_VERSION,
    )
    .expect("fixture memory image should encode");
    let boot = HvfSnapshotV2BootState::try_new(
        HvfSnapshotV2NativePath::try_from_bytes(b"/fixture/kernel")
            .expect("fixture kernel path should validate"),
        Some(
            HvfSnapshotV2NativePath::try_from_bytes(b"/fixture/initrd")
                .expect("fixture initrd path should validate"),
        ),
        Some("console=ttyAMA0 secret=fixture"),
    )
    .expect("fixture boot metadata should validate");
    let fdt = HvfSnapshotV2FdtState::try_new(
        aarch64::fdt_address(&layout).expect("fixture FDT placement should validate"),
        4096,
        0xfeed_face_dead_beef,
    )
    .expect("fixture FDT should validate");
    let machine = HvfSnapshotV2MachineState::try_new(machine, boot, fdt, None)
        .expect("fixture machine state should validate");
    let rtc_mmio_layout = compatibility.rtc_mmio_layout();
    let global = HvfSnapshotV2GlobalState::try_new(compatibility, interrupts.gic_device.clone())
        .expect("fixture global state should validate");
    let topology_members = (0..usize::from(vcpu_count))
        .map(|index| {
            let disposition = match index {
                0 => HvfArm64StableVcpuDisposition::Runnable,
                1 => HvfArm64StableVcpuDisposition::Suspended(
                    HvfArm64StableCpuSuspendState::new(
                        HvfArm64CpuSuspendConvention::Call64,
                        [0x100, 0x200, 0x300],
                        0x400,
                    )
                    .expect("fixture suspend state should validate"),
                ),
                _ => HvfArm64StableVcpuDisposition::Offline,
            };
            HvfArm64StablePausedTopologyMember::new(index, index as u64, disposition)
        })
        .collect();
    let topology = HvfArm64StablePausedTopologyState::new(
        gic_metadata.timer_interrupts.el1_virtual_timer_intid,
        topology_members,
    )
    .expect("fixture topology should validate");

    let mut vcpus = Vec::new();
    for index in 0..u32::from(vcpu_count) {
        let optional = optional_fixture(
            identification.id_aa64dfr0_el1(),
            optional_identification,
            &mandatory.simd_fp,
        );
        vcpus.push(
            HvfSnapshotV2VcpuState::try_new(
                index,
                u64::from(index),
                mandatory.clone(),
                interrupts.timer,
                interrupts.pending_interrupts,
                interrupts.gic_icc,
                optional,
            )
            .expect("fixture vCPU should validate"),
        );
    }
    let vmclock_base = aarch64::SYSTEM_MEM_START
        .checked_add(aarch64::SYSTEM_MEM_SIZE)
        .and_then(|end| end.checked_sub(ARM64_FDT_VMCLOCK_SIZE))
        .expect("fixture VMClock placement should validate");
    let vmgenid_base = vmclock_base
        .checked_sub(ARM64_FDT_VMGENID_SIZE)
        .expect("fixture VMGenID placement should validate");
    let platform_metadata = |base: u64, size: u64, interrupt_line: GuestInterruptLine| {
        SnapshotV1PlatformDeviceMetadata::new(
            GuestMemoryRange::new(GuestAddress::new(base), size)
                .expect("fixture platform range should validate"),
            Arm64FdtRegion { base, size },
            interrupt_line,
        )
    };
    let vmgenid = platform_metadata(
        vmgenid_base,
        ARM64_FDT_VMGENID_SIZE,
        devices.vmgenid().interrupt_line(),
    );
    let vmclock = platform_metadata(
        vmclock_base,
        ARM64_FDT_VMCLOCK_SIZE,
        devices.vmclock().interrupt_line(),
    );
    let arena_size = vmgenid
        .range()
        .start()
        .raw_value()
        .checked_sub(aarch64::SYSTEM_MEM_START)
        .expect("fixture PVTime arena should validate");
    let arena = GuestMemoryRange::new(GuestAddress::new(aarch64::SYSTEM_MEM_START), arena_size)
        .expect("fixture PVTime arena should validate");
    let pvtime_layout =
        Arm64PvTimeLayout::plan(vcpu_count, arena).expect("fixture PVTime layout should validate");
    let pvtime_vcpus = pvtime_layout
        .records()
        .iter()
        .enumerate()
        .map(|(index, record)| {
            HvfSnapshotV2PvTimeVcpuState::try_new(
                u32::try_from(index).expect("fixture index should fit"),
                record.start(),
                100 + u64::try_from(index).expect("fixture index should fit"),
            )
            .expect("fixture PVTime state should validate")
        })
        .collect();
    let time = HvfSnapshotV2TimeState::try_new(
        rtc_mmio_layout,
        vmgenid,
        vmclock,
        devices.vmclock_abi().unwrap_or_else(VmClockAbi::initial),
        pvtime_vcpus,
    )
    .expect("fixture time state should validate");
    HvfSnapshotV2PlatformState::try_new(memory_binding, machine, global, topology, vcpus, time)
        .expect("fixture platform should validate")
}

fn optional_fixture(
    expected_dfr0: u64,
    optional_identification: Option<HvfArm64VcpuSveSmeIdentificationRegisterState>,
    simd_fp: &HvfArm64VcpuSimdFpState,
) -> HvfArm64ReviewedOptionalStateRestore {
    let breakpoint_count = (((expected_dfr0 >> 12) & 0xf) as u8) + 1;
    let watchpoint_count = (((expected_dfr0 >> 20) & 0xf) as u8) + 1;
    let mut breakpoint_values = [HvfArm64OptionalStateValue::DestinationDefault; 16];
    let mut breakpoint_controls = [HvfArm64OptionalStateValue::DestinationDefault; 16];
    breakpoint_values[0] = HvfArm64OptionalStateValue::Explicit(0x1111);
    breakpoint_controls[0] = HvfArm64OptionalStateValue::Explicit(0);
    let breakpoints = HvfArm64DebugRegisterRestoreState::try_new(
        breakpoint_count,
        breakpoint_values,
        breakpoint_controls,
    )
    .expect("fixture breakpoint state should validate");
    let watchpoints = HvfArm64DebugRegisterRestoreState::try_new(
        watchpoint_count,
        [HvfArm64OptionalStateValue::DestinationDefault; 16],
        [HvfArm64OptionalStateValue::DestinationDefault; 16],
    )
    .expect("fixture watchpoint state should validate");

    let sme = optional_identification.map(|identification| {
        let maximum_svl_bytes = HVF_SNAPSHOT_V2_MAX_SME_SVL_BYTES;
        let z_registers = simd_fp
            .q_registers()
            .iter()
            .enumerate()
            .map(|(index, q)| {
                let mut bytes = vec![index as u8; maximum_svl_bytes];
                bytes[..q.len()].copy_from_slice(q);
                HvfArm64OptionalStateValue::Explicit(bytes.into_boxed_slice())
            })
            .collect();
        let p_registers = (0..OPTIONAL_SME_P_COUNT)
            .map(|index| {
                HvfArm64OptionalStateValue::Explicit(
                    vec![index as u8; maximum_svl_bytes / 8].into_boxed_slice(),
                )
            })
            .collect();
        let input = HvfArm64SmeRestoreStateInput::new(
            OPTIONAL_SME_VERSION_SME2,
            identification,
            maximum_svl_bytes,
            HvfArm64OptionalStateValue::Explicit(HvfArm64VcpuSmePstate::new(true, true)),
            [
                HvfArm64OptionalStateValue::Explicit(0x10),
                HvfArm64OptionalStateValue::DestinationDefault,
                HvfArm64OptionalStateValue::Explicit(0x30),
            ],
        )
        .with_streaming_registers(z_registers, p_registers)
        .with_za_register(
            HvfArm64OptionalStateValue::Explicit(
                vec![0x5a; maximum_svl_bytes * maximum_svl_bytes].into_boxed_slice(),
            ),
            Some(HvfArm64OptionalStateValue::Explicit([0x6a; 64])),
        );
        HvfArm64SmeRestoreState::try_new(input, simd_fp).expect("fixture SME state should validate")
    });
    HvfArm64ReviewedOptionalStateRestore::try_new(
        expected_dfr0,
        optional_identification.map(|_| OPTIONAL_SME_VERSION_SME2),
        breakpoints,
        watchpoints,
        sme,
        simd_fp.clone(),
    )
    .expect("fixture reviewed optional state should validate")
}

fn encoded_fixture(with_sme: bool) -> Vec<u8> {
    encode_hvf_snapshot_v2_platform_state(&platform_fixture(with_sme))
        .expect("fixture platform should encode")
}

pub(crate) fn fixture_bytes(hex: &str) -> Vec<u8> {
    let hex = hex.split_ascii_whitespace().collect::<String>();
    assert!(hex.len().is_multiple_of(2));
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).expect("fixture hex should be UTF-8");
            u8::from_str_radix(pair, 16).expect("fixture hex should decode")
        })
        .collect()
}

pub(crate) fn deterministic_minor_four_platform_fixture() -> HvfSnapshotV2PlatformState {
    deterministic_device_graph_platform_fixture(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        DETERMINISTIC_MEMORY_IMAGE_ID,
    )
}

fn deterministic_minor_five_platform_fixture() -> HvfSnapshotV2PlatformState {
    deterministic_device_graph_platform_fixture(
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        DETERMINISTIC_MULTI_BLOCK_MEMORY_IMAGE_ID,
    )
}

fn deterministic_minor_six_platform_fixture() -> HvfSnapshotV2PlatformState {
    deterministic_device_graph_platform_fixture(
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        DETERMINISTIC_STORAGE_MEMORY_IMAGE_ID,
    )
}

fn deterministic_minor_seven_platform_fixture() -> HvfSnapshotV2PlatformState {
    deterministic_device_graph_platform_fixture(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        DETERMINISTIC_SERIAL_MEMORY_IMAGE_ID,
    )
}

fn deterministic_minor_eight_platform_fixture() -> HvfSnapshotV2PlatformState {
    deterministic_device_graph_platform_fixture(
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
        DETERMINISTIC_ENTROPY_MEMORY_IMAGE_ID,
    )
}

fn deterministic_minor_nine_platform_fixture() -> HvfSnapshotV2PlatformState {
    deterministic_device_graph_platform_fixture(
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
        DETERMINISTIC_BALLOON_MEMORY_IMAGE_ID,
    )
}

fn deterministic_device_graph_platform_fixture(
    version: SnapshotFormatVersion,
    image_id: [u8; 16],
) -> HvfSnapshotV2PlatformState {
    let mut platform = platform_fixture(false);
    let legacy_fdt = platform.machine.fdt();
    platform.machine.fdt = HvfSnapshotV2FdtState::try_new_product_process_profile(
        legacy_fdt.address(),
        usize::try_from(legacy_fdt.size()).expect("fixture FDT size should fit"),
        legacy_fdt.checksum(),
    )
    .expect("minor-four product FDT profile should validate");
    let mut binding = platform
        .memory()
        .encode()
        .expect("fixture memory binding should encode");
    binding[10..12].copy_from_slice(&version.minor().to_le_bytes());
    binding[12..14].copy_from_slice(&version.patch().to_le_bytes());
    binding[32..48].copy_from_slice(&image_id);
    binding[48..56].fill(0);
    let checksum = crc64::crc64(0, &binding);
    binding[48..56].copy_from_slice(&checksum.to_le_bytes());

    let component = SnapshotV2Component::new(
        NATIVE_V2_MEMORY_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &binding,
    );
    let encoded = encode_snapshot_v2_state_with_compatibility_version(version, &[], &[component])
        .expect("deterministic device-graph memory state should encode");
    let structural = decode_snapshot_v2_state_with_compatibility_version(&encoded, version)
        .expect("deterministic device-graph memory state should decode");
    platform.memory = decode_snapshot_v2_memory_binding(&structural)
        .expect("deterministic device-graph binding should decode");
    platform
}

pub(crate) fn complete_state_fixture(graph_hex: &str) -> HvfSnapshotV2State {
    let graph = SnapshotV2DeviceGraph::decode(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &fixture_bytes(graph_hex),
    )
    .expect("immutable graph payload should decode");
    HvfSnapshotV2State::try_new(deterministic_minor_four_platform_fixture(), graph)
        .expect("complete minor-four fixture should validate")
}

fn complete_multi_block_state_fixture(graph_hex: &str) -> HvfSnapshotV2MultiBlockState {
    let graph = SnapshotV2MultiBlockDeviceGraph::decode(
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &fixture_bytes(graph_hex),
    )
    .expect("immutable profile-2 graph payload should decode");
    HvfSnapshotV2MultiBlockState::try_new(deterministic_minor_five_platform_fixture(), graph)
        .expect("complete minor-five fixture should validate")
}

fn complete_storage_state_fixture(graph_hex: &str) -> HvfSnapshotV2StorageState {
    let graph = SnapshotV2StorageDeviceGraph::decode(
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &fixture_bytes(graph_hex),
    )
    .expect("immutable profile-3 graph payload should decode");
    HvfSnapshotV2StorageState::try_new(deterministic_minor_six_platform_fixture(), graph)
        .expect("complete minor-six fixture should validate")
}

fn complete_serial_state_fixture(
    graph_hex: Option<&str>,
    serial_hex: &str,
) -> HvfSnapshotV2SerialState {
    let device_graph = graph_hex.map(|graph_hex| {
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &fixture_bytes(graph_hex),
        )
        .expect("immutable profile-3 graph payload should decode")
    });
    let serial = SnapshotV2SerialState::decode(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        &fixture_bytes(serial_hex),
    )
    .expect("immutable serial payload should decode");
    HvfSnapshotV2SerialState::try_new(
        deterministic_minor_seven_platform_fixture(),
        device_graph,
        serial,
    )
    .expect("complete minor-seven fixture should validate")
}

fn complete_entropy_state_fixture(
    graph_hex: Option<&str>,
    serial_hex: &str,
    entropy_hex: Option<&str>,
    relocate_pci_entropy: bool,
) -> HvfSnapshotV2EntropyState {
    let device_graph = graph_hex.map(|graph_hex| {
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &fixture_bytes(graph_hex),
        )
        .expect("immutable profile-3 graph payload should decode")
    });
    let serial = SnapshotV2SerialState::decode(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        &fixture_bytes(serial_hex),
    )
    .expect("immutable serial payload should decode");
    let entropy = entropy_hex.map(|entropy_hex| {
        let mut bytes = fixture_bytes(entropy_hex);
        if relocate_pci_entropy {
            relocate_entropy_pci_fixture(&mut bytes);
        }
        SnapshotV2EntropyState::decode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION, &bytes)
            .expect("immutable entropy payload should decode")
    });
    HvfSnapshotV2EntropyState::try_new(
        deterministic_minor_eight_platform_fixture(),
        device_graph,
        serial,
        entropy,
    )
    .expect("complete minor-eight fixture should validate")
}

fn relocate_entropy_pci_fixture(bytes: &mut [u8]) {
    let transport_directory_offset =
        NATIVE_V2_ENTROPY_STATE_HEADER_BYTES + 2 * NATIVE_V2_ENTROPY_STATE_SECTION_ENTRY_BYTES;
    let payload_offset = transport_directory_offset + 8;
    let transport_offset = usize::try_from(u64::from_le_bytes(
        bytes[payload_offset..payload_offset + 8]
            .try_into()
            .expect("transport directory offset should fit"),
    ))
    .expect("transport directory offset should fit usize");
    bytes_set_pci_endpoint(bytes, transport_offset, PCI_LAST_ENDPOINT_DEVICE);
}

fn bytes_set_pci_endpoint(bytes: &mut [u8], transport_offset: usize, device: u8) {
    bytes[transport_offset + 11] = device;
    let endpoint_index = u64::from(
        device
            .checked_sub(PCI_FIRST_ENDPOINT_DEVICE)
            .expect("fixture endpoint should be in the endpoint range"),
    );
    let bar_start = PCI_BAR64_START
        .checked_add(
            endpoint_index
                .checked_mul(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
                .expect("fixture BAR offset should fit"),
        )
        .expect("fixture BAR address should fit");
    bytes[transport_offset + 16..transport_offset + 24].copy_from_slice(&bar_start.to_le_bytes());
}

const PRODUCT_STORAGE_QUEUE_BASE: u64 = 0x8004_0000;
const PRODUCT_ENTROPY_QUEUE_BASE: u64 = 0x8006_0000;
const PRODUCT_BALLOON_QUEUE_BASE: u64 = 0x8008_0000;
const PRODUCT_QUEUE_STRIDE: u64 = 0x1_0000;
const PRODUCT_DRIVER_RING_OFFSET: u64 = 0x2000;
const PRODUCT_DEVICE_RING_OFFSET: u64 = 0x4000;

fn read_wire_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("wire u16 should fit"),
    )
}

fn read_wire_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("wire u64 should fit"),
    )
}

fn write_wire_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn component_section_offset(bytes: &[u8], entry_offset: usize) -> usize {
    usize::try_from(read_wire_u64(bytes, entry_offset + 8))
        .expect("component section offset should fit usize")
}

fn storage_section_offset(bytes: &[u8], entry_offset: usize) -> usize {
    usize::try_from(read_wire_u64(bytes, entry_offset + 16))
        .expect("storage section offset should fit usize")
}

fn relocate_common_queues(bytes: &mut [u8], common_offset: usize, base: u64) {
    let queue_count = usize::from(read_wire_u16(bytes, common_offset + 26));
    for index in 0..queue_count {
        let queue_offset = common_offset + 32 + index * 32;
        if read_wire_u16(bytes, queue_offset + 2) == 0 {
            continue;
        }
        let queue_base = base
            .checked_add(
                u64::try_from(index)
                    .expect("queue index should fit")
                    .checked_mul(PRODUCT_QUEUE_STRIDE)
                    .expect("queue stride should fit"),
            )
            .expect("queue base should fit");
        write_wire_u64(bytes, queue_offset + 8, queue_base);
        write_wire_u64(
            bytes,
            queue_offset + 16,
            queue_base + PRODUCT_DRIVER_RING_OFFSET,
        );
        write_wire_u64(
            bytes,
            queue_offset + 24,
            queue_base + PRODUCT_DEVICE_RING_OFFSET,
        );
    }
}

fn relocate_storage_product_fixture(bytes: &mut [u8], relocate_pci: bool) {
    let record_count = usize::from(read_wire_u16(bytes, 14));
    let section_directory = usize::try_from(read_wire_u64(bytes, 48))
        .expect("storage section directory should fit usize");
    for record_index in 0..record_count {
        let common_entry = section_directory
            + (record_index * 4 + 2) * NATIVE_V2_STORAGE_DEVICE_GRAPH_SECTION_ENTRY_BYTES;
        let common_offset = storage_section_offset(bytes, common_entry);
        relocate_common_queues(
            bytes,
            common_offset,
            PRODUCT_STORAGE_QUEUE_BASE
                + u64::try_from(record_index).expect("record index should fit")
                    * PRODUCT_QUEUE_STRIDE,
        );

        if relocate_pci {
            let transport_entry = section_directory
                + (record_index * 4 + 3) * NATIVE_V2_STORAGE_DEVICE_GRAPH_SECTION_ENTRY_BYTES;
            let transport_offset = storage_section_offset(bytes, transport_entry);
            bytes[transport_offset + 11] = bytes[transport_offset + 11]
                .checked_add(1)
                .expect("shifted storage endpoint should fit");
            let bar_start = read_wire_u64(bytes, transport_offset + 16)
                .checked_add(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
                .expect("shifted storage BAR should fit");
            write_wire_u64(bytes, transport_offset + 16, bar_start);
        }
    }
}

fn relocate_entropy_product_fixture(bytes: &mut [u8], transport: SnapshotV2DeviceTransportKind) {
    let common_entry =
        NATIVE_V2_ENTROPY_STATE_HEADER_BYTES + NATIVE_V2_ENTROPY_STATE_SECTION_ENTRY_BYTES;
    let common_offset = component_section_offset(bytes, common_entry);
    relocate_common_queues(bytes, common_offset, PRODUCT_ENTROPY_QUEUE_BASE);

    let transport_entry =
        NATIVE_V2_ENTROPY_STATE_HEADER_BYTES + 2 * NATIVE_V2_ENTROPY_STATE_SECTION_ENTRY_BYTES;
    let transport_offset = component_section_offset(bytes, transport_entry);
    match transport {
        SnapshotV2DeviceTransportKind::Mmio => {
            bytes[transport_offset + 12..transport_offset + 16]
                .copy_from_slice(&41_u32.to_le_bytes());
            write_wire_u64(bytes, transport_offset + 16, 101);
            write_wire_u64(bytes, transport_offset + 24, 0xd001_0000);
        }
        SnapshotV2DeviceTransportKind::Pci => relocate_entropy_pci_fixture(bytes),
    }
}

fn relocate_balloon_product_fixture(bytes: &mut [u8]) {
    let common_entry =
        NATIVE_V2_BALLOON_STATE_HEADER_BYTES + NATIVE_V2_BALLOON_STATE_SECTION_ENTRY_BYTES;
    let common_offset = component_section_offset(bytes, common_entry);
    relocate_common_queues(bytes, common_offset, PRODUCT_BALLOON_QUEUE_BASE);
}

fn product_storage_fixture(
    transport: SnapshotV2DeviceTransportKind,
) -> SnapshotV2StorageDeviceGraph {
    let mut bytes = fixture_bytes(match transport {
        SnapshotV2DeviceTransportKind::Mmio => STORAGE_MMIO_GRAPH_FIXTURE_HEX,
        SnapshotV2DeviceTransportKind::Pci => STORAGE_PCI_GRAPH_FIXTURE_HEX,
    });
    relocate_storage_product_fixture(&mut bytes, transport == SnapshotV2DeviceTransportKind::Pci);
    SnapshotV2StorageDeviceGraph::decode(
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &bytes,
    )
    .expect("relocated storage fixture should decode")
}

fn product_entropy_fixture(transport: SnapshotV2DeviceTransportKind) -> SnapshotV2EntropyState {
    let mut bytes = fixture_bytes(match transport {
        SnapshotV2DeviceTransportKind::Mmio => ENTROPY_INACTIVE_MMIO_FIXTURE_HEX,
        SnapshotV2DeviceTransportKind::Pci => ENTROPY_ACTIVE_PCI_FIXTURE_HEX,
    });
    relocate_entropy_product_fixture(&mut bytes, transport);
    SnapshotV2EntropyState::decode(NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION, &bytes)
        .expect("relocated entropy fixture should decode")
}

fn product_balloon_fixture(transport: SnapshotV2DeviceTransportKind) -> SnapshotV2BalloonState {
    let mut bytes = fixture_bytes(match transport {
        SnapshotV2DeviceTransportKind::Mmio => BALLOON_INACTIVE_MMIO_FIXTURE_HEX,
        SnapshotV2DeviceTransportKind::Pci => BALLOON_ACTIVE_PCI_FIXTURE_HEX,
    });
    relocate_balloon_product_fixture(&mut bytes);
    SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &bytes)
        .expect("relocated balloon fixture should decode")
}

fn product_serial_fixture() -> SnapshotV2SerialState {
    SnapshotV2SerialState::decode(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        &fixture_bytes(SERIAL_CONFIGURED_FIXTURE_HEX),
    )
    .expect("serial fixture should decode")
}

fn complete_balloon_state_fixture(
    transport: SnapshotV2DeviceTransportKind,
    has_storage: bool,
    has_entropy: bool,
    has_balloon: bool,
) -> HvfSnapshotV2BalloonState {
    HvfSnapshotV2BalloonState::try_new(
        deterministic_minor_nine_platform_fixture(),
        has_storage.then(|| product_storage_fixture(transport)),
        product_serial_fixture(),
        has_entropy.then(|| product_entropy_fixture(transport)),
        has_balloon.then(|| product_balloon_fixture(transport)),
    )
    .expect("complete minor-nine fixture should validate")
}

fn decode_platform(bytes: &[u8]) -> Result<HvfSnapshotV2PlatformState, HvfSnapshotV2DecodeError> {
    let state = decode_snapshot_v2_state(bytes).expect("mutated fixture should remain structural");
    decode_hvf_snapshot_v2_platform_state(&state)
}

fn decode_complete_state(bytes: &[u8]) -> Result<HvfSnapshotV2State, HvfSnapshotV2DecodeError> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("mutated fixture should remain structurally compatible");
    decode_hvf_snapshot_v2_state(&state)
}

fn decode_serial_state(bytes: &[u8]) -> Result<HvfSnapshotV2SerialState, HvfSnapshotV2DecodeError> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
    )
    .expect("mutated serial fixture should remain structurally compatible");
    decode_hvf_snapshot_v2_serial_state(&state)
}

fn decode_entropy_state(
    bytes: &[u8],
) -> Result<HvfSnapshotV2EntropyState, HvfSnapshotV2DecodeError> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
    )
    .expect("mutated entropy fixture should remain structurally compatible");
    decode_hvf_snapshot_v2_entropy_state(&state)
}

fn decode_balloon_state(
    bytes: &[u8],
) -> Result<HvfSnapshotV2BalloonState, HvfSnapshotV2DecodeError> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        bytes,
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
    )
    .expect("mutated balloon fixture should remain structurally compatible");
    decode_hvf_snapshot_v2_balloon_state(&state)
}

fn rebuild_components<F>(encoded: &[u8], mut transform: F) -> Vec<u8>
where
    F: FnMut(&mut SnapshotV2ComponentKey, &mut SnapshotV2ComponentDisposition, &mut Vec<u8>),
{
    let state = decode_snapshot_v2_state(encoded).expect("fixture should decode structurally");
    let mut owned = state
        .components()
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    for (key, disposition, payload) in &mut owned {
        transform(key, disposition, payload);
    }
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_LEGACY_PLATFORM_VERSION,
        &[],
        &components,
    )
    .expect("mutated components should re-encode")
}

fn rebuild_without_component(encoded: &[u8], excluded: SnapshotV2ComponentKey) -> Vec<u8> {
    let state = decode_snapshot_v2_state(encoded).expect("fixture should decode structurally");
    let owned = state
        .components()
        .filter(|component| component.key() != excluded)
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_LEGACY_PLATFORM_VERSION,
        &[],
        &components,
    )
    .expect("reduced components should re-encode")
}

fn rebuild_complete_components<F>(encoded: &[u8], mut transform: F) -> Vec<u8>
where
    F: FnMut(&mut SnapshotV2ComponentKey, &mut SnapshotV2ComponentDisposition, &mut Vec<u8>),
{
    let state = decode_snapshot_v2_state_with_compatibility_version(
        encoded,
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("complete fixture should decode structurally");
    let mut owned = state
        .components()
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    for (key, disposition, payload) in &mut owned {
        transform(key, disposition, payload);
    }
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("mutated complete components should re-encode")
}

fn rebuild_complete_without_component(encoded: &[u8], excluded: SnapshotV2ComponentKey) -> Vec<u8> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        encoded,
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("complete fixture should decode structurally");
    let owned = state
        .components()
        .filter(|component| component.key() != excluded)
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("reduced complete components should re-encode")
}

fn rebuild_storage_components<F>(encoded: &[u8], mut transform: F) -> Vec<u8>
where
    F: FnMut(&mut SnapshotV2ComponentKey, &mut SnapshotV2ComponentDisposition, &mut Vec<u8>),
{
    let state = decode_snapshot_v2_state_with_compatibility_version(
        encoded,
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("storage fixture should decode structurally");
    let mut owned = state
        .components()
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    for (key, disposition, payload) in &mut owned {
        transform(key, disposition, payload);
    }
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("mutated storage components should re-encode")
}

fn rebuild_storage_without_component(encoded: &[u8], excluded: SnapshotV2ComponentKey) -> Vec<u8> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        encoded,
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("storage fixture should decode structurally");
    let owned = state
        .components()
        .filter(|component| component.key() != excluded)
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("reduced storage components should re-encode")
}

fn rebuild_serial_components<F>(encoded: &[u8], mut transform: F) -> Vec<u8>
where
    F: FnMut(&mut SnapshotV2ComponentKey, &mut SnapshotV2ComponentDisposition, &mut Vec<u8>),
{
    let state = decode_snapshot_v2_state_with_compatibility_version(
        encoded,
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
    )
    .expect("serial fixture should decode structurally");
    let mut owned = state
        .components()
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    for (key, disposition, payload) in &mut owned {
        transform(key, disposition, payload);
    }
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("mutated serial components should re-encode")
}

fn rebuild_serial_without_component(encoded: &[u8], excluded: SnapshotV2ComponentKey) -> Vec<u8> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        encoded,
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
    )
    .expect("serial fixture should decode structurally");
    let owned = state
        .components()
        .filter(|component| component.key() != excluded)
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("reduced serial components should re-encode")
}

fn rebuild_entropy_components<F>(encoded: &[u8], mut transform: F) -> Vec<u8>
where
    F: FnMut(&mut SnapshotV2ComponentKey, &mut SnapshotV2ComponentDisposition, &mut Vec<u8>),
{
    let state = decode_snapshot_v2_state_with_compatibility_version(
        encoded,
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
    )
    .expect("entropy fixture should decode structurally");
    let mut owned = state
        .components()
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    for (key, disposition, payload) in &mut owned {
        transform(key, disposition, payload);
    }
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("mutated entropy components should re-encode")
}

fn rebuild_entropy_without_component(encoded: &[u8], excluded: SnapshotV2ComponentKey) -> Vec<u8> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        encoded,
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
    )
    .expect("entropy fixture should decode structurally");
    let owned = state
        .components()
        .filter(|component| component.key() != excluded)
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("reduced entropy components should re-encode")
}

fn rebuild_balloon_components<F>(encoded: &[u8], mut transform: F) -> Vec<u8>
where
    F: FnMut(&mut SnapshotV2ComponentKey, &mut SnapshotV2ComponentDisposition, &mut Vec<u8>),
{
    let state = decode_snapshot_v2_state_with_compatibility_version(
        encoded,
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
    )
    .expect("balloon fixture should decode structurally");
    let mut owned = state
        .components()
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    for (key, disposition, payload) in &mut owned {
        transform(key, disposition, payload);
    }
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("mutated balloon components should re-encode")
}

fn rebuild_balloon_without_component(encoded: &[u8], excluded: SnapshotV2ComponentKey) -> Vec<u8> {
    let state = decode_snapshot_v2_state_with_compatibility_version(
        encoded,
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
    )
    .expect("balloon fixture should decode structurally");
    let owned = state
        .components()
        .filter(|component| component.key() != excluded)
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("reduced balloon components should re-encode")
}

fn replace_state_checksum(bytes: &mut [u8]) {
    let checksum_offset = bytes.len() - NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES;
    let checksum = crc64::crc64(0, &bytes[..checksum_offset]);
    bytes[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());
}

fn optional_payload_offset(vcpu_payload: &[u8]) -> usize {
    let mandatory_length =
        u32::from_le_bytes(vcpu_payload[32..36].try_into().expect("fixed vCPU header")) as usize;
    VCPU_HEADER_BYTES + mandatory_length + VCPU_INTERRUPT_BYTES
}

fn machine_cpu_entry_offset(machine_payload: &[u8]) -> usize {
    let kernel_length =
        u32::from_le_bytes(machine_payload[32..36].try_into().expect("kernel length")) as usize;
    let initrd_length =
        u32::from_le_bytes(machine_payload[36..40].try_into().expect("initrd length")) as usize;
    let argument_length =
        u32::from_le_bytes(machine_payload[40..44].try_into().expect("argument length")) as usize;
    MACHINE_HEADER_BYTES + kernel_length + initrd_length + argument_length
}

fn optional_record_offset(vcpu_payload: &[u8], expected_tag: u16) -> usize {
    let optional_start = optional_payload_offset(vcpu_payload);
    let registry_length = u32::from_le_bytes(
        vcpu_payload[optional_start + 36..optional_start + 40]
            .try_into()
            .expect("fixed optional header"),
    ) as usize;
    let mut position = optional_start + OPTIONAL_HEADER_BYTES;
    let end = position + registry_length;
    while position < end {
        let tag = u16::from_le_bytes(
            vcpu_payload[position..position + 2]
                .try_into()
                .expect("record tag"),
        );
        if tag == expected_tag {
            return position;
        }
        let disposition = vcpu_payload[position + 2];
        let width = u32::from_le_bytes(
            vcpu_payload[position + 4..position + 8]
                .try_into()
                .expect("record width"),
        ) as usize;
        position += OPTIONAL_RECORD_HEADER_BYTES;
        if disposition == OPTIONAL_DISPOSITION_EXPLICIT {
            position += width;
        }
    }
    panic!("fixture optional record tag {expected_tag} should exist")
}

#[test]
fn round_trips_complete_multi_vcpu_platform_state() {
    for vcpu_count in [1, 2, MAX_SUPPORTED_VCPUS] {
        let original = platform_fixture_with_count(vcpu_count, false);
        let encoded =
            encode_hvf_snapshot_v2_platform_state(&original).expect("platform should encode");
        let structural = decode_snapshot_v2_state(&encoded).expect("container should decode");
        let keys = structural
            .components()
            .map(SnapshotV2Component::key)
            .collect::<Vec<_>>();
        let mut expected_keys = vec![
            NATIVE_V2_MEMORY_COMPONENT_KEY,
            NATIVE_V2_MACHINE_COMPONENT_KEY,
            NATIVE_V2_GLOBAL_COMPONENT_KEY,
            NATIVE_V2_TOPOLOGY_COMPONENT_KEY,
        ];
        expected_keys.extend((0..u32::from(vcpu_count)).map(native_v2_vcpu_component_key));
        expected_keys.push(NATIVE_V2_TIME_COMPONENT_KEY);
        assert_eq!(keys, expected_keys);
        let decoded =
            decode_hvf_snapshot_v2_platform_state(&structural).expect("platform should decode");
        assert_eq!(decoded, original);
        assert_eq!(decoded.vcpus().len(), usize::from(vcpu_count));
    }
}

#[test]
fn exact_minor_four_mmio_and_pci_states_are_complete_immutable_fixtures() {
    let cases = [
        (MMIO_GRAPH_FIXTURE_HEX, SnapshotV2DeviceTransportKind::Mmio),
        (PCI_GRAPH_FIXTURE_HEX, SnapshotV2DeviceTransportKind::Pci),
    ];
    let mut fingerprints = Vec::new();
    for (graph_hex, expected_transport) in cases {
        let original = complete_state_fixture(graph_hex);
        let encoded = encode_hvf_snapshot_v2_state(&original)
            .expect("complete minor-four state should encode");
        let payload_end = encoded.len() - NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES;
        fingerprints.push((encoded.len(), crc64::crc64(0, &encoded[..payload_end])));

        let structural = decode_snapshot_v2_state_with_compatibility_version(
            &encoded,
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        )
        .expect("complete minor-four state should decode structurally");
        assert_eq!(
            structural.metadata().version(),
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION
        );
        let keys = structural
            .components()
            .map(SnapshotV2Component::key)
            .collect::<Vec<_>>();
        let mut expected_keys = vec![
            NATIVE_V2_MEMORY_COMPONENT_KEY,
            NATIVE_V2_MACHINE_COMPONENT_KEY,
            NATIVE_V2_GLOBAL_COMPONENT_KEY,
            NATIVE_V2_TOPOLOGY_COMPONENT_KEY,
        ];
        expected_keys.extend(
            (0..u32::try_from(original.platform().vcpus().len())
                .expect("fixture vCPU count should fit"))
                .map(native_v2_vcpu_component_key),
        );
        expected_keys.extend([
            NATIVE_V2_TIME_COMPONENT_KEY,
            NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY,
        ]);
        assert_eq!(keys, expected_keys);
        assert_eq!(
            structural
                .component(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY)
                .expect("complete fixture should contain the graph")
                .payload(),
            fixture_bytes(graph_hex)
        );
        assert!(decode_snapshot_v2_state(&encoded).is_ok());
        assert!(matches!(
            decode_hvf_snapshot_v2_platform_state(&structural),
            Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
        ));

        let decoded =
            decode_hvf_snapshot_v2_state(&structural).expect("complete state should decode");
        assert_eq!(decoded, original);
        assert_eq!(decoded.device_graph().transport_kind(), expected_transport);
        assert_eq!(
            encode_hvf_snapshot_v2_state(&decoded)
                .expect("decoded complete state should re-encode"),
            encoded
        );
        let (platform, graph) = decoded.into_parts();
        assert_eq!(
            platform.memory().version(),
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION
        );
        assert_eq!(graph.transport_kind(), expected_transport);
    }
    assert_eq!(fingerprints, COMPLETE_STATE_FINGERPRINTS);
}

#[test]
fn exact_minor_five_multi_block_mmio_and_pci_states_round_trip_separately() {
    let cases = [
        (
            MULTI_BLOCK_MMIO_GRAPH_FIXTURE_HEX,
            SnapshotV2DeviceTransportKind::Mmio,
            true,
        ),
        (
            MULTI_BLOCK_PCI_GRAPH_FIXTURE_HEX,
            SnapshotV2DeviceTransportKind::Pci,
            false,
        ),
    ];
    for (graph_hex, expected_transport, has_root) in cases {
        let original = complete_multi_block_state_fixture(graph_hex);
        let encoded = encode_hvf_snapshot_v2_multi_block_state(&original)
            .expect("complete minor-five state should encode");
        let structural = decode_snapshot_v2_state(&encoded)
            .expect("complete minor-five state should decode structurally");
        assert_eq!(
            structural.metadata().version(),
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION
        );
        assert!(matches!(
            decode_hvf_snapshot_v2_platform_state(&structural),
            Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
        ));
        assert!(matches!(
            decode_hvf_snapshot_v2_state(&structural),
            Err(HvfSnapshotV2DecodeError::UnsupportedProfile)
        ));

        let decoded = decode_hvf_snapshot_v2_multi_block_state(&structural)
            .expect("complete minor-five state should decode");
        assert_eq!(decoded, original);
        assert_eq!(decoded.device_graph().transport_kind(), expected_transport);
        assert_eq!(decoded.device_graph().root_key().is_some(), has_root);
        assert_eq!(
            encode_hvf_snapshot_v2_multi_block_state(&decoded)
                .expect("decoded minor-five state should re-encode"),
            encoded
        );
        let (platform, graph) = decoded.into_parts();
        assert_eq!(
            platform.memory().version(),
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION
        );
        assert_eq!(graph.transport_kind(), expected_transport);
    }
}

#[test]
fn current_exact_minor_six_storage_mmio_and_pci_states_round_trip_separately() {
    let cases = [
        (
            STORAGE_MMIO_GRAPH_FIXTURE_HEX,
            SnapshotV2DeviceTransportKind::Mmio,
            0,
            1,
        ),
        (
            STORAGE_PCI_GRAPH_FIXTURE_HEX,
            SnapshotV2DeviceTransportKind::Pci,
            1,
            1,
        ),
    ];
    for (graph_hex, expected_transport, block_count, pmem_count) in cases {
        let original = complete_storage_state_fixture(graph_hex);
        let encoded = encode_hvf_snapshot_v2_storage_state(&original)
            .expect("complete current minor-six state should encode");
        let structural =
            decode_snapshot_v2_state(&encoded).expect("current minor-six state should decode");
        assert_eq!(
            structural.metadata().version(),
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION
        );
        assert!(matches!(
            decode_hvf_snapshot_v2_platform_state(&structural),
            Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
        ));
        assert!(matches!(
            decode_hvf_snapshot_v2_multi_block_state(&structural),
            Err(HvfSnapshotV2DecodeError::UnsupportedProfile)
        ));

        let decoded = decode_hvf_snapshot_v2_storage_state(&structural)
            .expect("internal minor-six HVF state should decode");
        assert_eq!(decoded, original);
        assert_eq!(decoded.device_graph().transport_kind(), expected_transport);
        assert_eq!(decoded.device_graph().block_records().len(), block_count);
        assert_eq!(decoded.device_graph().pmem_records().len(), pmem_count);
        assert_eq!(
            encode_hvf_snapshot_v2_storage_state(&decoded)
                .expect("decoded minor-six state should re-encode"),
            encoded
        );
        let (platform, graph) = decoded.into_parts();
        assert_eq!(
            platform.memory().version(),
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION
        );
        assert_eq!(graph.transport_kind(), expected_transport);
    }
}

#[test]
fn internal_exact_minor_seven_serial_only_mmio_and_pci_states_round_trip() {
    let cases = [
        (None, SERIAL_DEFAULT_FIXTURE_HEX, None, 0_usize, 0_usize),
        (
            Some(STORAGE_MMIO_GRAPH_FIXTURE_HEX),
            SERIAL_CONFIGURED_FIXTURE_HEX,
            Some(SnapshotV2DeviceTransportKind::Mmio),
            0,
            1,
        ),
        (
            Some(STORAGE_PCI_GRAPH_FIXTURE_HEX),
            SERIAL_CONFIGURED_FIXTURE_HEX,
            Some(SnapshotV2DeviceTransportKind::Pci),
            1,
            1,
        ),
    ];
    for (graph_hex, serial_hex, expected_transport, block_count, pmem_count) in cases {
        let original = complete_serial_state_fixture(graph_hex, serial_hex);
        let encoded = encode_hvf_snapshot_v2_serial_state(&original)
            .expect("complete minor-seven serial state should encode");
        let structural =
            decode_snapshot_v2_state(&encoded).expect("current minor-seven state should decode");
        assert_eq!(
            structural.metadata().version(),
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION
        );

        let keys = structural
            .components()
            .map(SnapshotV2Component::key)
            .collect::<Vec<_>>();
        let mut expected_keys = vec![
            NATIVE_V2_MEMORY_COMPONENT_KEY,
            NATIVE_V2_MACHINE_COMPONENT_KEY,
            NATIVE_V2_GLOBAL_COMPONENT_KEY,
            NATIVE_V2_TOPOLOGY_COMPONENT_KEY,
        ];
        expected_keys.extend(
            (0..u32::try_from(original.platform().vcpus().len())
                .expect("fixture vCPU count should fit"))
                .map(native_v2_vcpu_component_key),
        );
        expected_keys.push(NATIVE_V2_TIME_COMPONENT_KEY);
        if graph_hex.is_some() {
            expected_keys.push(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY);
        }
        expected_keys.push(NATIVE_V2_SERIAL_COMPONENT_KEY);
        assert_eq!(keys, expected_keys);
        assert_eq!(
            structural
                .component(NATIVE_V2_SERIAL_COMPONENT_KEY)
                .expect("minor-seven fixture should contain serial")
                .payload(),
            fixture_bytes(serial_hex)
        );
        if let Some(graph_hex) = graph_hex {
            assert_eq!(
                structural
                    .component(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY)
                    .expect("storage-bearing fixture should contain a graph")
                    .payload(),
                fixture_bytes(graph_hex)
            );
        } else {
            assert!(
                structural
                    .component(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY)
                    .is_none()
            );
        }

        assert!(matches!(
            decode_hvf_snapshot_v2_platform_state(&structural),
            Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
        ));
        assert!(matches!(
            decode_hvf_snapshot_v2_state(&structural),
            Err(HvfSnapshotV2DecodeError::UnsupportedProfile)
        ));
        assert!(matches!(
            decode_hvf_snapshot_v2_multi_block_state(&structural),
            Err(HvfSnapshotV2DecodeError::UnsupportedProfile)
        ));
        assert!(matches!(
            decode_hvf_snapshot_v2_storage_state(&structural),
            Err(HvfSnapshotV2DecodeError::UnsupportedProfile)
        ));

        let decoded = decode_hvf_snapshot_v2_serial_state(&structural)
            .expect("minor-seven serial composition should decode");
        assert_eq!(decoded, original);
        assert_eq!(
            decoded.device_graph().map(|graph| graph.transport_kind()),
            expected_transport
        );
        assert_eq!(
            decoded
                .device_graph()
                .map_or(0, |graph| graph.block_records().len()),
            block_count
        );
        assert_eq!(
            decoded
                .device_graph()
                .map_or(0, |graph| graph.pmem_records().len()),
            pmem_count
        );
        assert_eq!(
            encode_hvf_snapshot_v2_serial_state(&decoded)
                .expect("decoded minor-seven state should re-encode"),
            encoded
        );
        let debug = format!("{decoded:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("serial-log"));
        let (platform, graph, serial) = decoded.into_parts();
        assert_eq!(
            platform.memory().version(),
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION
        );
        assert_eq!(graph.is_some(), expected_transport.is_some());
        assert_eq!(
            serial.compatibility_version(),
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION
        );
    }
}

#[test]
fn internal_exact_minor_eight_entropy_compositions_encode_nested_versions_canonically() {
    let cases = [
        (
            None,
            SERIAL_DEFAULT_FIXTURE_HEX,
            None,
            None,
            false,
            false,
            false,
        ),
        (
            Some(STORAGE_MMIO_GRAPH_FIXTURE_HEX),
            SERIAL_CONFIGURED_FIXTURE_HEX,
            None,
            Some(SnapshotV2DeviceTransportKind::Mmio),
            true,
            false,
            false,
        ),
        (
            None,
            SERIAL_DEFAULT_FIXTURE_HEX,
            Some(ENTROPY_INACTIVE_MMIO_FIXTURE_HEX),
            Some(SnapshotV2DeviceTransportKind::Mmio),
            false,
            true,
            false,
        ),
        (
            Some(STORAGE_PCI_GRAPH_FIXTURE_HEX),
            SERIAL_CONFIGURED_FIXTURE_HEX,
            Some(ENTROPY_ACTIVE_PCI_FIXTURE_HEX),
            Some(SnapshotV2DeviceTransportKind::Pci),
            true,
            true,
            true,
        ),
    ];

    for (
        graph_hex,
        serial_hex,
        entropy_hex,
        expected_transport,
        has_storage,
        has_entropy,
        relocate_pci_entropy,
    ) in cases
    {
        let original = complete_entropy_state_fixture(
            graph_hex,
            serial_hex,
            entropy_hex,
            relocate_pci_entropy,
        );
        let encoded = encode_hvf_snapshot_v2_entropy_state(&original)
            .expect("complete minor-eight entropy state should encode");
        let structural = decode_snapshot_v2_state_with_compatibility_version(
            &encoded,
            NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
        )
        .expect("minor-eight entropy composition should decode structurally");
        let decoded = decode_hvf_snapshot_v2_entropy_state(&structural)
            .expect("minor-eight entropy composition should decode");
        assert_eq!(decoded, original);

        let keys = structural
            .components()
            .map(SnapshotV2Component::key)
            .collect::<Vec<_>>();
        let mut expected_keys = vec![
            NATIVE_V2_MEMORY_COMPONENT_KEY,
            NATIVE_V2_MACHINE_COMPONENT_KEY,
            NATIVE_V2_GLOBAL_COMPONENT_KEY,
            NATIVE_V2_TOPOLOGY_COMPONENT_KEY,
        ];
        expected_keys.extend(
            (0..u32::try_from(original.platform().vcpus().len())
                .expect("fixture vCPU count should fit"))
                .map(native_v2_vcpu_component_key),
        );
        expected_keys.push(NATIVE_V2_TIME_COMPONENT_KEY);
        if has_storage {
            expected_keys.push(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY);
        }
        expected_keys.push(NATIVE_V2_SERIAL_COMPONENT_KEY);
        if has_entropy {
            expected_keys.push(NATIVE_V2_ENTROPY_COMPONENT_KEY);
        }
        assert_eq!(keys, expected_keys);

        let serial = SnapshotV2SerialState::decode(
            NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            structural
                .component(NATIVE_V2_SERIAL_COMPONENT_KEY)
                .expect("minor-eight composition should contain serial")
                .payload(),
        )
        .expect("nested exact-2.7 serial should decode");
        assert_eq!(&serial, original.serial());

        let graph = structural
            .component(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY)
            .map(|component| {
                SnapshotV2StorageDeviceGraph::decode(
                    NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                    component.payload(),
                )
                .expect("nested exact-2.6 storage graph should decode")
            });
        assert_eq!(graph.as_ref(), original.device_graph());

        let entropy = structural
            .component(NATIVE_V2_ENTROPY_COMPONENT_KEY)
            .map(|component| {
                SnapshotV2EntropyState::decode(
                    NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                    component.payload(),
                )
                .expect("nested exact-2.8 entropy state should decode")
            });
        assert_eq!(entropy.as_ref(), original.entropy());
        assert_eq!(
            graph
                .as_ref()
                .map(SnapshotV2StorageDeviceGraph::transport_kind)
                .or_else(|| { entropy.as_ref().map(|state| state.transport().kind()) }),
            expected_transport
        );
        assert_eq!(
            encode_hvf_snapshot_v2_entropy_state(&decoded)
                .expect("minor-eight composition should re-encode"),
            encoded
        );
        let debug = format!("{decoded:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("serial-log"));
    }
}

#[test]
fn exact_minor_nine_balloon_round_trips_every_mmio_and_pci_product_canonically() {
    for transport in [
        SnapshotV2DeviceTransportKind::Mmio,
        SnapshotV2DeviceTransportKind::Pci,
    ] {
        for mask in 0_u8..8 {
            let has_storage = mask & 1 != 0;
            let has_entropy = mask & 2 != 0;
            let has_balloon = mask & 4 != 0;
            let original =
                complete_balloon_state_fixture(transport, has_storage, has_entropy, has_balloon);
            let encoded = encode_hvf_snapshot_v2_balloon_state(&original)
                .expect("complete minor-nine balloon state should encode");
            assert_eq!(
                encode_hvf_snapshot_v2_balloon_state(&original)
                    .expect("minor-nine encoding should be deterministic"),
                encoded
            );

            let structural = decode_snapshot_v2_state_with_compatibility_version(
                &encoded,
                NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
            )
            .expect("minor-nine balloon composition should decode structurally");
            let decoded = decode_hvf_snapshot_v2_balloon_state(&structural)
                .expect("minor-nine balloon composition should decode independently");
            assert_eq!(decoded, original);
            assert_eq!(
                encode_hvf_snapshot_v2_balloon_state(&decoded)
                    .expect("decoded minor-nine composition should re-encode"),
                encoded
            );
            let platform = decode_hvf_snapshot_v2_platform_components(
                &structural,
                original.platform().vcpus().len(),
                true,
            )
            .expect("minor-nine platform components should decode");
            assert_eq!(&platform, original.platform());

            let keys = structural
                .components()
                .map(SnapshotV2Component::key)
                .collect::<Vec<_>>();
            let mut expected_keys = vec![
                NATIVE_V2_MEMORY_COMPONENT_KEY,
                NATIVE_V2_MACHINE_COMPONENT_KEY,
                NATIVE_V2_GLOBAL_COMPONENT_KEY,
                NATIVE_V2_TOPOLOGY_COMPONENT_KEY,
            ];
            expected_keys.extend(
                (0..u32::try_from(original.platform().vcpus().len())
                    .expect("fixture vCPU count should fit"))
                    .map(native_v2_vcpu_component_key),
            );
            expected_keys.push(NATIVE_V2_TIME_COMPONENT_KEY);
            if has_storage {
                expected_keys.push(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY);
            }
            expected_keys.push(NATIVE_V2_SERIAL_COMPONENT_KEY);
            if has_entropy {
                expected_keys.push(NATIVE_V2_ENTROPY_COMPONENT_KEY);
            }
            if has_balloon {
                expected_keys.push(NATIVE_V2_BALLOON_COMPONENT_KEY);
            }
            assert_eq!(keys, expected_keys);

            let graph = structural
                .component(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY)
                .map(|component| {
                    SnapshotV2StorageDeviceGraph::decode(
                        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                        component.payload(),
                    )
                    .expect("nested exact-2.6 storage graph should decode")
                });
            assert_eq!(graph.as_ref(), original.device_graph());

            let serial = SnapshotV2SerialState::decode(
                NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
                structural
                    .component(NATIVE_V2_SERIAL_COMPONENT_KEY)
                    .expect("minor-nine composition should contain serial")
                    .payload(),
            )
            .expect("nested exact-2.7 serial should decode");
            assert_eq!(&serial, original.serial());

            let entropy = structural
                .component(NATIVE_V2_ENTROPY_COMPONENT_KEY)
                .map(|component| {
                    SnapshotV2EntropyState::decode(
                        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
                        component.payload(),
                    )
                    .expect("nested exact-2.8 entropy state should decode")
                });
            assert_eq!(entropy.as_ref(), original.entropy());

            let balloon = structural
                .component(NATIVE_V2_BALLOON_COMPONENT_KEY)
                .map(|component| {
                    SnapshotV2BalloonState::decode(
                        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
                        component.payload(),
                    )
                    .expect("nested exact-2.9 balloon state should decode")
                });
            assert_eq!(balloon.as_ref(), original.balloon());

            let captured_transport = graph
                .as_ref()
                .map(SnapshotV2StorageDeviceGraph::transport_kind)
                .or_else(|| entropy.as_ref().map(|state| state.transport().kind()))
                .or_else(|| balloon.as_ref().map(|state| state.transport().kind()));
            assert_eq!(
                captured_transport,
                (has_storage || has_entropy || has_balloon).then_some(transport)
            );

            let debug = format!("{original:?}");
            assert!(debug.contains("<redacted>"));
            assert!(!debug.contains("serial-log"));
        }
    }
}

#[test]
fn exact_minor_nine_decoder_rejects_malformed_profile_and_nested_balloon() {
    let original =
        complete_balloon_state_fixture(SnapshotV2DeviceTransportKind::Pci, true, true, true);
    let encoded = encode_hvf_snapshot_v2_balloon_state(&original)
        .expect("complete minor-nine state should encode");

    let missing_serial =
        rebuild_balloon_without_component(&encoded, NATIVE_V2_SERIAL_COMPONENT_KEY);
    assert!(matches!(
        decode_balloon_state(&missing_serial),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let wrong_instance = rebuild_balloon_components(&encoded, |key, _, _| {
        if *key == NATIVE_V2_BALLOON_COMPONENT_KEY {
            *key = SnapshotV2ComponentKey::new(NATIVE_V2_BALLOON_COMPONENT_KEY.kind(), 1);
        }
    });
    assert!(matches!(
        decode_balloon_state(&wrong_instance),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let nonsemantic = rebuild_balloon_components(&encoded, |key, disposition, _| {
        if *key == NATIVE_V2_ENTROPY_COMPONENT_KEY {
            *disposition = SnapshotV2ComponentDisposition::NonSemantic;
        }
    });
    assert!(matches!(
        decode_balloon_state(&nonsemantic),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let invalid_balloon = rebuild_balloon_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_BALLOON_COMPONENT_KEY {
            payload[0] ^= 0xff;
        }
    });
    assert!(matches!(
        decode_balloon_state(&invalid_balloon),
        Err(HvfSnapshotV2DecodeError::BalloonState(
            SnapshotV2BalloonStateDecodeError::InvalidMagic
        ))
    ));

    let entropy_state = encode_hvf_snapshot_v2_entropy_state(&complete_entropy_state_fixture(
        None,
        SERIAL_DEFAULT_FIXTURE_HEX,
        None,
        false,
    ))
    .expect("minor-eight entropy state should encode");
    let structural = decode_snapshot_v2_state_with_compatibility_version(
        &entropy_state,
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
    )
    .expect("minor-eight entropy state should decode structurally");
    assert!(matches!(
        decode_hvf_snapshot_v2_balloon_state(&structural),
        Err(HvfSnapshotV2DecodeError::UnsupportedProfile)
    ));
}

#[test]
fn exact_minor_nine_composition_rejects_transport_aperture_and_pmem_conflicts() {
    assert!(matches!(
        HvfSnapshotV2BalloonState::try_new(
            deterministic_minor_nine_platform_fixture(),
            Some(product_storage_fixture(SnapshotV2DeviceTransportKind::Mmio)),
            product_serial_fixture(),
            None,
            Some(product_balloon_fixture(SnapshotV2DeviceTransportKind::Pci)),
        ),
        Err(HvfSnapshotV2BuildError::CrossComponent)
    ));

    let mut entropy_bytes = fixture_bytes(ENTROPY_ACTIVE_PCI_FIXTURE_HEX);
    let entropy_common_entry =
        NATIVE_V2_ENTROPY_STATE_HEADER_BYTES + NATIVE_V2_ENTROPY_STATE_SECTION_ENTRY_BYTES;
    let entropy_common_offset = component_section_offset(&entropy_bytes, entropy_common_entry);
    relocate_common_queues(
        &mut entropy_bytes,
        entropy_common_offset,
        PRODUCT_ENTROPY_QUEUE_BASE,
    );
    let colliding_entropy = SnapshotV2EntropyState::decode(
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
        &entropy_bytes,
    )
    .expect("PCI entropy with relocated queues should decode");
    assert!(matches!(
        HvfSnapshotV2BalloonState::try_new(
            deterministic_minor_nine_platform_fixture(),
            None,
            product_serial_fixture(),
            Some(colliding_entropy),
            Some(product_balloon_fixture(SnapshotV2DeviceTransportKind::Pci)),
        ),
        Err(HvfSnapshotV2BuildError::CrossComponent)
    ));

    let mut aperture_bytes = fixture_bytes(BALLOON_INACTIVE_MMIO_FIXTURE_HEX);
    let balloon_transport_entry =
        NATIVE_V2_BALLOON_STATE_HEADER_BYTES + 3 * NATIVE_V2_BALLOON_STATE_SECTION_ENTRY_BYTES;
    let balloon_transport_offset =
        component_section_offset(&aperture_bytes, balloon_transport_entry);
    write_wire_u64(
        &mut aperture_bytes,
        balloon_transport_offset + 24,
        0x8001_0000,
    );
    let memory_overlapping_balloon = SnapshotV2BalloonState::decode(
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
        &aperture_bytes,
    )
    .expect("memory-overlapping MMIO placement should remain a valid device artifact");
    assert!(matches!(
        HvfSnapshotV2BalloonState::try_new(
            deterministic_minor_nine_platform_fixture(),
            None,
            product_serial_fixture(),
            None,
            Some(memory_overlapping_balloon),
        ),
        Err(HvfSnapshotV2BuildError::CrossComponent)
    ));

    let graph = product_storage_fixture(SnapshotV2DeviceTransportKind::Mmio);
    let pmem_start = graph
        .pmem_records()
        .first()
        .expect("storage fixture should contain pmem")
        .pmem()
        .guest_range()
        .start()
        .raw_value();
    let mut pmem_bytes = fixture_bytes(BALLOON_INACTIVE_MMIO_FIXTURE_HEX);
    let balloon_transport_offset = component_section_offset(&pmem_bytes, balloon_transport_entry);
    write_wire_u64(&mut pmem_bytes, balloon_transport_offset + 24, pmem_start);
    let pmem_overlapping_balloon =
        SnapshotV2BalloonState::decode(NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION, &pmem_bytes)
            .expect("pmem-overlapping MMIO placement should remain a valid device artifact");
    assert!(matches!(
        HvfSnapshotV2BalloonState::try_new(
            deterministic_minor_nine_platform_fixture(),
            Some(graph),
            product_serial_fixture(),
            None,
            Some(pmem_overlapping_balloon),
        ),
        Err(HvfSnapshotV2BuildError::CrossComponent)
    ));
}

#[test]
fn exact_minor_nine_composition_rejects_queue_platform_and_pci_order_conflicts() {
    let graph = product_storage_fixture(SnapshotV2DeviceTransportKind::Pci);
    let mut colliding_queue_bytes = fixture_bytes(BALLOON_ACTIVE_PCI_FIXTURE_HEX);
    let balloon_common_entry =
        NATIVE_V2_BALLOON_STATE_HEADER_BYTES + NATIVE_V2_BALLOON_STATE_SECTION_ENTRY_BYTES;
    let balloon_common_offset =
        component_section_offset(&colliding_queue_bytes, balloon_common_entry);
    relocate_common_queues(
        &mut colliding_queue_bytes,
        balloon_common_offset,
        PRODUCT_STORAGE_QUEUE_BASE,
    );
    let colliding_queue_balloon = SnapshotV2BalloonState::decode(
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
        &colliding_queue_bytes,
    )
    .expect("cross-device queue collision should remain a valid balloon artifact");
    assert!(matches!(
        HvfSnapshotV2BalloonState::try_new(
            deterministic_minor_nine_platform_fixture(),
            Some(graph),
            product_serial_fixture(),
            None,
            Some(colliding_queue_balloon),
        ),
        Err(HvfSnapshotV2BuildError::CrossComponent)
    ));

    let platform = deterministic_minor_nine_platform_fixture();
    let vmclock_start = platform.time().vmclock().range().start().raw_value();
    let mut platform_queue_bytes = fixture_bytes(BALLOON_ACTIVE_PCI_FIXTURE_HEX);
    let balloon_common_offset =
        component_section_offset(&platform_queue_bytes, balloon_common_entry);
    relocate_common_queues(
        &mut platform_queue_bytes,
        balloon_common_offset,
        PRODUCT_BALLOON_QUEUE_BASE,
    );
    write_wire_u64(
        &mut platform_queue_bytes,
        balloon_common_offset + 32 + 8,
        vmclock_start,
    );
    let platform_queue_balloon = SnapshotV2BalloonState::decode(
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
        &platform_queue_bytes,
    )
    .expect("platform-overlapping queue should remain a valid device artifact");
    assert!(matches!(
        HvfSnapshotV2BalloonState::try_new(
            platform,
            None,
            product_serial_fixture(),
            None,
            Some(platform_queue_balloon),
        ),
        Err(HvfSnapshotV2BuildError::CrossComponent)
    ));

    let graph = product_storage_fixture(SnapshotV2DeviceTransportKind::Pci);
    let mut ordered_bytes = fixture_bytes(BALLOON_ACTIVE_PCI_FIXTURE_HEX);
    let balloon_common_offset = component_section_offset(&ordered_bytes, balloon_common_entry);
    relocate_common_queues(
        &mut ordered_bytes,
        balloon_common_offset,
        PRODUCT_BALLOON_QUEUE_BASE,
    );
    let balloon_transport_entry =
        NATIVE_V2_BALLOON_STATE_HEADER_BYTES + 3 * NATIVE_V2_BALLOON_STATE_SECTION_ENTRY_BYTES;
    let balloon_transport_offset =
        component_section_offset(&ordered_bytes, balloon_transport_entry);
    bytes_set_pci_endpoint(&mut ordered_bytes, balloon_transport_offset, 4);
    let out_of_order_balloon = SnapshotV2BalloonState::decode(
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION,
        &ordered_bytes,
    )
    .expect("out-of-order PCI balloon should remain a valid device artifact");
    assert!(matches!(
        HvfSnapshotV2BalloonState::try_new(
            deterministic_minor_nine_platform_fixture(),
            Some(graph),
            product_serial_fixture(),
            None,
            Some(out_of_order_balloon),
        ),
        Err(HvfSnapshotV2BuildError::CrossComponent)
    ));
}

#[test]
fn exact_minor_eight_composition_rejects_transport_and_placement_conflicts() {
    let graph = SnapshotV2StorageDeviceGraph::decode(
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &fixture_bytes(STORAGE_MMIO_GRAPH_FIXTURE_HEX),
    )
    .expect("MMIO storage fixture should decode");
    let serial = SnapshotV2SerialState::decode(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        &fixture_bytes(SERIAL_CONFIGURED_FIXTURE_HEX),
    )
    .expect("serial fixture should decode");
    let entropy = SnapshotV2EntropyState::decode(
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
        &fixture_bytes(ENTROPY_ACTIVE_PCI_FIXTURE_HEX),
    )
    .expect("PCI entropy fixture should decode");

    assert!(matches!(
        HvfSnapshotV2EntropyState::try_new(
            deterministic_minor_eight_platform_fixture(),
            Some(graph),
            serial.clone(),
            Some(entropy.clone()),
        ),
        Err(HvfSnapshotV2BuildError::CrossComponent)
    ));

    let pci_graph = SnapshotV2StorageDeviceGraph::decode(
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &fixture_bytes(STORAGE_PCI_GRAPH_FIXTURE_HEX),
    )
    .expect("PCI storage fixture should decode");
    assert!(matches!(
        HvfSnapshotV2EntropyState::try_new(
            deterministic_minor_eight_platform_fixture(),
            Some(pci_graph),
            serial,
            Some(entropy),
        ),
        Err(HvfSnapshotV2BuildError::CrossComponent)
    ));
}

#[test]
fn exact_minor_eight_decoder_rejects_malformed_profile_and_nested_entropy() {
    let original = complete_entropy_state_fixture(
        Some(STORAGE_PCI_GRAPH_FIXTURE_HEX),
        SERIAL_CONFIGURED_FIXTURE_HEX,
        Some(ENTROPY_ACTIVE_PCI_FIXTURE_HEX),
        true,
    );
    let encoded = encode_hvf_snapshot_v2_entropy_state(&original)
        .expect("complete minor-eight state should encode");

    let missing_serial =
        rebuild_entropy_without_component(&encoded, NATIVE_V2_SERIAL_COMPONENT_KEY);
    assert!(matches!(
        decode_entropy_state(&missing_serial),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let wrong_instance = rebuild_entropy_components(&encoded, |key, _, _| {
        if *key == NATIVE_V2_ENTROPY_COMPONENT_KEY {
            *key = SnapshotV2ComponentKey::new(NATIVE_V2_ENTROPY_COMPONENT_KEY.kind(), 1);
        }
    });
    assert!(matches!(
        decode_entropy_state(&wrong_instance),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let nonsemantic = rebuild_entropy_components(&encoded, |key, disposition, _| {
        if *key == NATIVE_V2_ENTROPY_COMPONENT_KEY {
            *disposition = SnapshotV2ComponentDisposition::NonSemantic;
        }
    });
    assert!(matches!(
        decode_entropy_state(&nonsemantic),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let structural = decode_snapshot_v2_state_with_compatibility_version(
        &encoded,
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
    )
    .expect("entropy fixture should decode structurally");
    let mut owned = structural
        .components()
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let duplicate_payload = structural
        .component(NATIVE_V2_ENTROPY_COMPONENT_KEY)
        .expect("entropy fixture should contain entropy")
        .payload()
        .to_vec();
    owned.push((
        SnapshotV2ComponentKey::new(NATIVE_V2_ENTROPY_COMPONENT_KEY.kind(), 1),
        SnapshotV2ComponentDisposition::Semantic,
        duplicate_payload,
    ));
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    let duplicate = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("duplicate entropy instance should encode structurally");
    assert!(matches!(
        decode_entropy_state(&duplicate),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let invalid_entropy = rebuild_entropy_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_ENTROPY_COMPONENT_KEY {
            payload[0] ^= 0xff;
        }
    });
    assert!(matches!(
        decode_entropy_state(&invalid_entropy),
        Err(HvfSnapshotV2DecodeError::EntropyState(
            SnapshotV2EntropyStateDecodeError::InvalidMagic
        ))
    ));

    let serial_only = encode_hvf_snapshot_v2_serial_state(&complete_serial_state_fixture(
        None,
        SERIAL_DEFAULT_FIXTURE_HEX,
    ))
    .expect("minor-seven serial state should encode");
    let structural = decode_snapshot_v2_state_with_compatibility_version(
        &serial_only,
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
    )
    .expect("minor-seven serial state should decode structurally");
    assert!(matches!(
        decode_hvf_snapshot_v2_entropy_state(&structural),
        Err(HvfSnapshotV2DecodeError::UnsupportedProfile)
    ));
}

#[test]
fn exact_minor_seven_profile_rejects_missing_duplicate_nonsemantic_and_invalid_state() {
    let original = complete_serial_state_fixture(
        Some(STORAGE_MMIO_GRAPH_FIXTURE_HEX),
        SERIAL_CONFIGURED_FIXTURE_HEX,
    );
    let encoded = encode_hvf_snapshot_v2_serial_state(&original)
        .expect("complete minor-seven state should encode");

    let missing = rebuild_serial_without_component(&encoded, NATIVE_V2_SERIAL_COMPONENT_KEY);
    assert!(matches!(
        decode_serial_state(&missing),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let wrong_instance = rebuild_serial_components(&encoded, |key, _, _| {
        if *key == NATIVE_V2_SERIAL_COMPONENT_KEY {
            *key = SnapshotV2ComponentKey::new(NATIVE_V2_SERIAL_COMPONENT_KEY.kind(), 1);
        }
    });
    assert!(matches!(
        decode_serial_state(&wrong_instance),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let nonsemantic = rebuild_serial_components(&encoded, |key, disposition, _| {
        if *key == NATIVE_V2_SERIAL_COMPONENT_KEY {
            *disposition = SnapshotV2ComponentDisposition::NonSemantic;
        }
    });
    assert!(matches!(
        decode_serial_state(&nonsemantic),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let structural = decode_snapshot_v2_state_with_compatibility_version(
        &encoded,
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
    )
    .expect("serial fixture should decode structurally");
    let mut owned = structural
        .components()
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let duplicate_payload = structural
        .component(NATIVE_V2_SERIAL_COMPONENT_KEY)
        .expect("serial fixture should contain serial")
        .payload()
        .to_vec();
    owned.push((
        SnapshotV2ComponentKey::new(NATIVE_V2_SERIAL_COMPONENT_KEY.kind(), 1),
        SnapshotV2ComponentDisposition::Semantic,
        duplicate_payload,
    ));
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    let duplicate = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("duplicate serial instance should encode structurally");
    assert!(matches!(
        decode_serial_state(&duplicate),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let invalid_serial = rebuild_serial_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_SERIAL_COMPONENT_KEY {
            payload[0] ^= 0xff;
        }
    });
    assert!(matches!(
        decode_serial_state(&invalid_serial),
        Err(HvfSnapshotV2DecodeError::SerialState(
            SnapshotV2SerialStateDecodeError::InvalidHeader
        ))
    ));

    let invalid_storage = rebuild_serial_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY {
            payload[10..12].copy_from_slice(&2_u16.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_serial_state(&invalid_storage),
        Err(HvfSnapshotV2DecodeError::StorageDeviceGraph(
            SnapshotV2StorageDeviceGraphDecodeError::UnsupportedProfile
        ))
    ));

    let (wrong_platform, graph) =
        complete_storage_state_fixture(STORAGE_MMIO_GRAPH_FIXTURE_HEX).into_parts();
    let serial = SnapshotV2SerialState::decode(
        NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
        &fixture_bytes(SERIAL_CONFIGURED_FIXTURE_HEX),
    )
    .expect("serial fixture should decode");
    assert_eq!(
        HvfSnapshotV2SerialState::try_new(wrong_platform, Some(graph), serial),
        Err(HvfSnapshotV2BuildError::Version)
    );
}

#[test]
fn internal_exact_minor_six_wrapper_rejects_missing_nonsemantic_and_invalid_graphs() {
    let original = complete_storage_state_fixture(STORAGE_MMIO_GRAPH_FIXTURE_HEX);
    let encoded = encode_hvf_snapshot_v2_storage_state(&original)
        .expect("complete internal minor-six state should encode");

    let missing = rebuild_storage_without_component(&encoded, NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY);
    let structural = decode_snapshot_v2_state_with_compatibility_version(
        &missing,
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("missing graph mutation should remain structural");
    assert!(matches!(
        decode_hvf_snapshot_v2_storage_state(&structural),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let structural = decode_snapshot_v2_state_with_compatibility_version(
        &encoded,
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("storage fixture should decode structurally");
    let mut owned = structural
        .components()
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let graph_payload = structural
        .component(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY)
        .expect("storage fixture should contain a graph")
        .payload()
        .to_vec();
    owned.push((
        SnapshotV2ComponentKey::new(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY.kind(), 1),
        SnapshotV2ComponentDisposition::Semantic,
        graph_payload,
    ));
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    let duplicate = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("duplicate graph mutation should encode structurally");
    let structural = decode_snapshot_v2_state_with_compatibility_version(
        &duplicate,
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("duplicate graph mutation should decode structurally");
    assert!(matches!(
        decode_hvf_snapshot_v2_storage_state(&structural),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let nonsemantic = rebuild_storage_components(&encoded, |key, disposition, _| {
        if *key == NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY {
            *disposition = SnapshotV2ComponentDisposition::NonSemantic;
        }
    });
    let structural = decode_snapshot_v2_state_with_compatibility_version(
        &nonsemantic,
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("nonsemantic mutation should remain structural");
    assert!(matches!(
        decode_hvf_snapshot_v2_storage_state(&structural),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let invalid_graph = rebuild_storage_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY {
            payload[10..12].copy_from_slice(&2_u16.to_le_bytes());
        }
    });
    let structural = decode_snapshot_v2_state_with_compatibility_version(
        &invalid_graph,
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("invalid graph mutation should remain structural");
    assert!(matches!(
        decode_hvf_snapshot_v2_storage_state(&structural),
        Err(HvfSnapshotV2DecodeError::StorageDeviceGraph(
            SnapshotV2StorageDeviceGraphDecodeError::UnsupportedProfile
        ))
    ));
}

#[test]
fn exact_minor_four_profile_rejects_missing_duplicate_wrong_and_disagreeing_graphs() {
    let original = complete_state_fixture(MMIO_GRAPH_FIXTURE_HEX);
    let encoded =
        encode_hvf_snapshot_v2_state(&original).expect("complete minor-four state should encode");

    let missing_product_fdt_profile = rebuild_complete_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            payload[72..80].fill(0);
        }
    });
    assert!(matches!(
        decode_complete_state(&missing_product_fdt_profile),
        Err(HvfSnapshotV2DecodeError::InvalidMachine)
    ));

    let missing =
        rebuild_complete_without_component(&encoded, NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY);
    assert!(matches!(
        decode_complete_state(&missing),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let wrong_instance = rebuild_complete_components(&encoded, |key, _, _| {
        if *key == NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY {
            *key = SnapshotV2ComponentKey::new(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY.kind(), 1);
        }
    });
    assert!(matches!(
        decode_complete_state(&wrong_instance),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let nonsemantic = rebuild_complete_components(&encoded, |key, disposition, _| {
        if *key == NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY {
            *disposition = SnapshotV2ComponentDisposition::NonSemantic;
        }
    });
    assert!(matches!(
        decode_complete_state(&nonsemantic),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let minor_three_memory = rebuild_complete_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MEMORY_COMPONENT_KEY {
            payload[10..12].copy_from_slice(&3_u16.to_le_bytes());
            payload[48..56].fill(0);
            let checksum = crc64::crc64(0, payload);
            payload[48..56].copy_from_slice(&checksum.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_complete_state(&minor_three_memory),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::Version
        ))
    ));

    let structural = decode_snapshot_v2_state_with_compatibility_version(
        &encoded,
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
    )
    .expect("complete fixture should decode structurally");
    let mut owned = structural
        .components()
        .map(|component| {
            (
                component.key(),
                component.disposition(),
                component.payload().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    let graph_payload = structural
        .component(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY)
        .expect("complete fixture should contain graph")
        .payload()
        .to_vec();
    owned.push((
        SnapshotV2ComponentKey::new(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY.kind(), 1),
        SnapshotV2ComponentDisposition::Semantic,
        graph_payload,
    ));
    let components = owned
        .iter()
        .map(|(key, disposition, payload)| SnapshotV2Component::new(*key, *disposition, payload))
        .collect::<Vec<_>>();
    let duplicate = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &[],
        &components,
    )
    .expect("second canonical kind-seven instance should encode structurally");
    assert!(matches!(
        decode_complete_state(&duplicate),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let mut wrong_order = encoded.clone();
    let component_count = usize::try_from(structural.metadata().component_count())
        .expect("fixture component count should fit");
    let time_entry = 64 + (component_count - 2) * NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES;
    let graph_entry = time_entry + NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES;
    let time_bytes =
        wrong_order[time_entry..time_entry + NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES].to_vec();
    let graph_bytes =
        wrong_order[graph_entry..graph_entry + NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES].to_vec();
    wrong_order[time_entry..time_entry + NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES]
        .copy_from_slice(&graph_bytes);
    wrong_order[graph_entry..graph_entry + NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES]
        .copy_from_slice(&time_bytes);
    replace_state_checksum(&mut wrong_order);
    assert_eq!(
        decode_snapshot_v2_state_with_compatibility_version(
            &wrong_order,
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        ),
        Err(SnapshotV2DecodeError::InvalidComponentDirectory)
    );

    let mut downgraded = encoded.clone();
    downgraded[10..12].copy_from_slice(&NATIVE_V2_LEGACY_PLATFORM_VERSION.minor().to_le_bytes());
    replace_state_checksum(&mut downgraded);
    assert_eq!(
        decode_snapshot_v2_state_with_compatibility_version(
            &downgraded,
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        ),
        Err(SnapshotV2DecodeError::UnknownSemanticComponent)
    );
    assert_eq!(
        decode_snapshot_v2_state(&downgraded),
        Err(SnapshotV2DecodeError::UnknownSemanticComponent)
    );

    let graph_payload = structural
        .component(NATIVE_V2_DEVICE_GRAPH_COMPONENT_KEY)
        .expect("complete fixture should contain graph")
        .payload();
    assert!(
        SnapshotV2DeviceGraph::decode(NATIVE_V2_LEGACY_PLATFORM_VERSION, graph_payload).is_err()
    );

    let components = structural.components().collect::<Vec<_>>();
    assert!(matches!(
        encode_snapshot_v2_state_with_compatibility_version(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &[1],
            &components,
        ),
        Err(SnapshotV2EncodeError::UnknownRequiredFeature)
    ));

    let graph = SnapshotV2DeviceGraph::decode(
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        &fixture_bytes(MMIO_GRAPH_FIXTURE_HEX),
    )
    .expect("immutable graph fixture should decode");
    assert!(matches!(
        HvfSnapshotV2State::try_new(platform_fixture(false), graph),
        Err(HvfSnapshotV2BuildError::Version)
    ));
}

#[test]
fn round_trips_maximum_svl_active_sme_state() {
    let original = platform_fixture(true);
    let encoded =
        encode_hvf_snapshot_v2_platform_state(&original).expect("SME platform should encode");
    let structural = decode_snapshot_v2_state(&encoded).expect("container should decode");
    let decoded =
        decode_hvf_snapshot_v2_platform_state(&structural).expect("SME platform should decode");
    assert_eq!(decoded, original);
    let sme = decoded.vcpus()[0]
        .reviewed_optional()
        .sme()
        .expect("SME state should be present");
    assert_eq!(sme.maximum_svl_bytes(), HVF_SNAPSHOT_V2_MAX_SME_SVL_BYTES);
    assert_eq!(
        sme.za_register().and_then(|value| match value {
            HvfArm64OptionalStateValue::Explicit(bytes) => Some(bytes.len()),
            HvfArm64OptionalStateValue::DestinationDefault => None,
        }),
        Some(HVF_SNAPSHOT_V2_MAX_SME_SVL_BYTES * HVF_SNAPSHOT_V2_MAX_SME_SVL_BYTES)
    );
}

#[test]
fn round_trips_redacted_cpu_template_application_evidence_at_every_width() {
    let values = [
        (1, HvfArm64CpuTemplateValueWidth::U32, 0xff, 0x12, 0x1234),
        (
            3,
            HvfArm64CpuTemplateValueWidth::U64,
            0xff00,
            0x3400,
            0xabcd,
        ),
        (
            52,
            HvfArm64CpuTemplateValueWidth::U128,
            0xffff,
            0x5678,
            (1_u128 << 100) | 0xabcd,
        ),
    ];
    let entries = values
        .into_iter()
        .map(|(tag, width, filter, logical_value, baseline)| {
            HvfArm64CpuTemplateApplicationEntry::try_from_stable_values(
                tag,
                width,
                filter,
                logical_value,
                baseline,
                (baseline & !filter) | logical_value,
            )
            .expect("fixture CPU application entry should validate")
        })
        .collect();
    let application = HvfArm64CpuTemplateApplicationState::try_new(entries)
        .expect("fixture CPU application should validate");
    let mut original = platform_fixture(false);
    original.machine.cpu_template = Some(application);

    let encoded = encode_hvf_snapshot_v2_platform_state(&original).expect("platform should encode");
    let structural = decode_snapshot_v2_state(&encoded).expect("container should decode");
    let decoded =
        decode_hvf_snapshot_v2_platform_state(&structural).expect("platform should decode");
    assert_eq!(decoded, original);
    let application = decoded
        .machine()
        .cpu_template()
        .expect("CPU application should be retained");
    assert_eq!(application.entries().len(), values.len());
    let debug = format!("{application:?}");
    assert!(debug.contains(REDACTED));
    assert!(!debug.contains("1234"));
    assert!(!debug.contains("abcd"));

    let invalid_equation = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            let first_entry = machine_cpu_entry_offset(payload);
            payload[first_entry + 56] ^= 1;
        }
    });
    assert!(matches!(
        decode_platform(&invalid_equation),
        Err(HvfSnapshotV2DecodeError::InvalidMachine)
    ));

    for (relative_offset, value, expected) in [
        (0, 4, HvfSnapshotV2DecodeError::InvalidMachine),
        (2, 2, HvfSnapshotV2DecodeError::InvalidMachine),
        (3, 1, HvfSnapshotV2DecodeError::NonzeroReserved),
    ] {
        let malformed = rebuild_components(&encoded, |key, _, payload| {
            if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
                let first_entry = machine_cpu_entry_offset(payload);
                payload[first_entry + relative_offset] = value;
            }
        });
        assert_eq!(
            decode_platform(&malformed)
                .expect_err("malformed CPU entry should fail")
                .to_string(),
            expected.to_string()
        );
    }

    let noncanonical_mask = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            let first_entry = machine_cpu_entry_offset(payload);
            payload[first_entry + 24..first_entry + 40].copy_from_slice(&0x100_u128.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&noncanonical_mask),
        Err(HvfSnapshotV2DecodeError::InvalidMachine)
    ));

    let duplicate = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            let first_entry = machine_cpu_entry_offset(payload);
            let first = payload[first_entry..first_entry + MACHINE_CPU_ENTRY_BYTES].to_vec();
            payload
                [first_entry + MACHINE_CPU_ENTRY_BYTES..first_entry + 2 * MACHINE_CPU_ENTRY_BYTES]
                .copy_from_slice(&first);
        }
    });
    assert!(matches!(
        decode_platform(&duplicate),
        Err(HvfSnapshotV2DecodeError::InvalidMachine)
    ));
}

#[test]
fn round_trips_maximum_boot_fdt_and_complete_cpu_tag_inventory() {
    let mut original = platform_fixture(false);
    original.machine.boot = HvfSnapshotV2BootState::try_new(
        HvfSnapshotV2NativePath::try_from_bytes(&vec![b'k'; HVF_SNAPSHOT_V2_MAX_PATH_BYTES])
            .expect("maximum kernel path should validate"),
        Some(
            HvfSnapshotV2NativePath::try_from_bytes(&vec![b'i'; HVF_SNAPSHOT_V2_MAX_PATH_BYTES])
                .expect("maximum initrd path should validate"),
        ),
        Some(&"a".repeat(HVF_SNAPSHOT_V2_MAX_BOOT_ARGUMENT_BYTES)),
    )
    .expect("maximum boot metadata should validate");
    original.machine.fdt = HvfSnapshotV2FdtState::try_new(
        original.machine.fdt().address(),
        usize::try_from(aarch64::FDT_MAX_SIZE).expect("FDT maximum should fit"),
        0x1234,
    )
    .expect("maximum FDT should validate");

    let entries = (1..=83_u16)
        .filter_map(|tag| {
            [
                HvfArm64CpuTemplateValueWidth::U32,
                HvfArm64CpuTemplateValueWidth::U64,
                HvfArm64CpuTemplateValueWidth::U128,
            ]
            .into_iter()
            .find_map(|width| {
                HvfArm64CpuTemplateApplicationEntry::try_from_stable_values(tag, width, 0, 0, 0, 0)
                    .ok()
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 80);
    original.machine.cpu_template = Some(
        HvfArm64CpuTemplateApplicationState::try_new(entries)
            .expect("complete CPU tag inventory should validate"),
    );

    let encoded = encode_hvf_snapshot_v2_platform_state(&original).expect("platform should encode");
    let structural = decode_snapshot_v2_state(&encoded).expect("container should decode");
    assert_eq!(
        decode_hvf_snapshot_v2_platform_state(&structural).expect("platform should decode"),
        original
    );
}

#[test]
fn round_trips_maximum_global_gic_payload() {
    let platform = platform_fixture(false);
    let original = HvfSnapshotV2GlobalState::try_new(
        platform.global().compatibility().clone(),
        HvfGicDeviceState::new(vec![0xa5; HVF_SNAPSHOT_V2_GIC_DEVICE_STATE_MAX_BYTES]),
    )
    .expect("maximum GIC payload should validate");
    let encoded = encode_global(&original).expect("maximum GIC payload should encode");
    assert_eq!(
        decode_global(&encoded).expect("maximum GIC payload should decode"),
        original
    );
}

#[test]
fn rejects_nonsemantic_component_before_typed_decode() {
    let encoded = encoded_fixture(false);
    let mutated = rebuild_components(&encoded, |key, disposition, _| {
        if *key == NATIVE_V2_GLOBAL_COMPONENT_KEY {
            *disposition = SnapshotV2ComponentDisposition::NonSemantic;
        }
    });
    assert!(matches!(
        decode_platform(&mutated),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let missing = rebuild_without_component(&encoded, NATIVE_V2_GLOBAL_COMPONENT_KEY);
    assert!(matches!(
        decode_platform(&missing),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let instance_gap = rebuild_components(&encoded, |key, _, _| {
        if *key == native_v2_vcpu_component_key(1) {
            *key = native_v2_vcpu_component_key(2);
        }
    });
    assert!(matches!(
        decode_platform(&instance_gap),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));

    let no_vcpus = rebuild_without_component(
        &rebuild_without_component(&encoded, native_v2_vcpu_component_key(1)),
        native_v2_vcpu_component_key(0),
    );
    assert!(matches!(
        decode_platform(&no_vcpus),
        Err(HvfSnapshotV2DecodeError::InvalidComponentProfile)
    ));
}

#[test]
fn rejects_minor_one_memory_only_state_as_hvf_profile_before_payload_decode() {
    let encoded = encoded_fixture(false);
    let state = decode_snapshot_v2_state(&encoded).expect("fixture should decode structurally");
    let memory = state
        .component(NATIVE_V2_MEMORY_COMPONENT_KEY)
        .expect("fixture memory component should exist");
    let mut memory_only = encode_snapshot_v2_state_with_compatibility_version(
        NATIVE_V2_LEGACY_PLATFORM_VERSION,
        &[],
        &[memory],
    )
    .expect("minor-two memory-only state should encode");
    memory_only[10..12].copy_from_slice(&1_u16.to_le_bytes());
    let checksum_offset = memory_only.len()
        - bangbang_runtime::snapshot_format_v2::NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES;
    let checksum = crc64::crc64(0, &memory_only[..checksum_offset]);
    memory_only[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());

    let state =
        decode_snapshot_v2_state(&memory_only).expect("minor-one structure should remain readable");
    assert!(matches!(
        decode_hvf_snapshot_v2_platform_state(&state),
        Err(HvfSnapshotV2DecodeError::UnsupportedProfile)
    ));
}

#[test]
fn keeps_minor_two_platform_structure_readable_but_requires_time_for_typed_profile() {
    let encoded = rebuild_without_component(&encoded_fixture(false), NATIVE_V2_TIME_COMPONENT_KEY);
    let mut minor_two = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MEMORY_COMPONENT_KEY {
            payload[10..12].copy_from_slice(&2_u16.to_le_bytes());
            payload[12..14].copy_from_slice(&0_u16.to_le_bytes());
            payload[48..56].fill(0);
            let checksum = crc64::crc64(0, payload);
            payload[48..56].copy_from_slice(&checksum.to_le_bytes());
        }
    });
    minor_two[10..12].copy_from_slice(&2_u16.to_le_bytes());
    let checksum_offset =
        minor_two.len() - bangbang_runtime::snapshot_format_v2::NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES;
    let checksum = crc64::crc64(0, &minor_two[..checksum_offset]);
    minor_two[checksum_offset..].copy_from_slice(&checksum.to_le_bytes());

    let state =
        decode_snapshot_v2_state(&minor_two).expect("minor-two structure should remain readable");
    assert_eq!(state.metadata().version().minor(), 2);
    assert!(state.component(NATIVE_V2_TIME_COMPONENT_KEY).is_none());
    assert_eq!(
        decode_snapshot_v2_memory_binding(&state)
            .expect("minor-two memory binding should remain readable")
            .version()
            .minor(),
        2
    );
    assert!(matches!(
        decode_hvf_snapshot_v2_platform_state(&state),
        Err(HvfSnapshotV2DecodeError::UnsupportedProfile)
    ));
}

#[test]
fn rejects_component_reserved_bytes_and_invalid_topology_disposition() {
    let encoded = encoded_fixture(false);
    let reserved = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            payload[60] = 1;
        }
    });
    assert!(matches!(
        decode_platform(&reserved),
        Err(HvfSnapshotV2DecodeError::NonzeroReserved)
    ));

    let disposition = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_TOPOLOGY_COMPONENT_KEY {
            payload[TOPOLOGY_HEADER_BYTES + 4] = 9;
        }
    });
    assert!(matches!(
        decode_platform(&disposition),
        Err(HvfSnapshotV2DecodeError::InvalidTopology)
    ));

    for (offset, value) in [
        (TOPOLOGY_HEADER_BYTES + 16, 1),
        (TOPOLOGY_HEADER_BYTES + TOPOLOGY_MEMBER_BYTES + 5, 9),
        (TOPOLOGY_HEADER_BYTES + TOPOLOGY_MEMBER_BYTES + 40, 1),
    ] {
        let continuation = rebuild_components(&encoded, |key, _, payload| {
            if *key == NATIVE_V2_TOPOLOGY_COMPONENT_KEY {
                payload[offset] = value;
            }
        });
        assert!(matches!(
            decode_platform(&continuation),
            Err(HvfSnapshotV2DecodeError::InvalidTopology)
        ));
    }

    let member_reserved = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_TOPOLOGY_COMPONENT_KEY {
            payload[TOPOLOGY_HEADER_BYTES + 6] = 1;
        }
    });
    assert!(matches!(
        decode_platform(&member_reserved),
        Err(HvfSnapshotV2DecodeError::NonzeroReserved)
    ));
}

#[test]
fn rejects_every_platform_component_header_flag_and_reserved_family() {
    let encoded = encoded_fixture(false);
    for key in [
        NATIVE_V2_MACHINE_COMPONENT_KEY,
        NATIVE_V2_GLOBAL_COMPONENT_KEY,
        NATIVE_V2_TOPOLOGY_COMPONENT_KEY,
        native_v2_vcpu_component_key(0),
        NATIVE_V2_TIME_COMPONENT_KEY,
    ] {
        let bad_magic = rebuild_components(&encoded, |candidate, _, payload| {
            if *candidate == key {
                payload[0] ^= 0xff;
            }
        });
        assert!(matches!(
            decode_platform(&bad_magic),
            Err(HvfSnapshotV2DecodeError::InvalidHeader)
        ));

        let bad_flags = rebuild_components(&encoded, |candidate, _, payload| {
            if *candidate == key {
                payload[12] = 1;
            }
        });
        assert!(matches!(
            decode_platform(&bad_flags),
            Err(HvfSnapshotV2DecodeError::InvalidHeader)
        ));

        for offset in [8, 10] {
            let bad_profile = rebuild_components(&encoded, |candidate, _, payload| {
                if *candidate == key {
                    payload[offset] = 0xff;
                }
            });
            assert!(matches!(
                decode_platform(&bad_profile),
                Err(HvfSnapshotV2DecodeError::InvalidHeader)
            ));
        }
    }

    for (key, offset) in [
        (NATIVE_V2_MACHINE_COMPONENT_KEY, 60),
        (NATIVE_V2_GLOBAL_COMPONENT_KEY, 20),
        (NATIVE_V2_TOPOLOGY_COMPONENT_KEY, 24),
        (native_v2_vcpu_component_key(0), 20),
        (NATIVE_V2_TIME_COMPONENT_KEY, 20),
    ] {
        let bad_reserved = rebuild_components(&encoded, |candidate, _, payload| {
            if *candidate == key {
                payload[offset] = 1;
            }
        });
        assert!(matches!(
            decode_platform(&bad_reserved),
            Err(HvfSnapshotV2DecodeError::NonzeroReserved)
        ));
    }

    for relative_offset in [0, 8, 10, 12, 56] {
        let bad_optional_header = rebuild_components(&encoded, |key, _, payload| {
            if *key == native_v2_vcpu_component_key(0) {
                let optional_start = optional_payload_offset(payload);
                payload[optional_start + relative_offset] =
                    if relative_offset == 56 { 1 } else { 0xff };
            }
        });
        let expected_nonzero_reserved = relative_offset == 56;
        assert!(
            matches!(
                decode_platform(&bad_optional_header),
                Err(HvfSnapshotV2DecodeError::NonzeroReserved)
            ) == expected_nonzero_reserved
        );
        assert!(
            expected_nonzero_reserved
                || matches!(
                    decode_platform(&bad_optional_header),
                    Err(HvfSnapshotV2DecodeError::InvalidHeader)
                )
        );
    }
}

#[test]
fn rejects_component_counts_and_lengths_before_dependent_allocation() {
    let encoded = encoded_fixture(false);
    let machine_path = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            payload[32..36].copy_from_slice(
                &u32::try_from(HVF_SNAPSHOT_V2_MAX_PATH_BYTES + 1)
                    .expect("path bound should fit")
                    .to_le_bytes(),
            );
        }
    });
    assert!(matches!(
        decode_platform(&machine_path),
        Err(HvfSnapshotV2DecodeError::InvalidLength)
    ));

    let machine_cpu = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            payload[44..48].copy_from_slice(
                &u32::try_from(HVF_ARM64_CPU_TEMPLATE_APPLICATION_MAX_ENTRIES + 1)
                    .expect("CPU entry bound should fit")
                    .to_le_bytes(),
            );
        }
    });
    assert!(matches!(
        decode_platform(&machine_cpu),
        Err(HvfSnapshotV2DecodeError::InvalidMachine)
    ));

    let global = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_GLOBAL_COMPONENT_KEY {
            payload[16..20].copy_from_slice(
                &u32::try_from(HVF_SNAPSHOT_V2_GIC_DEVICE_STATE_MAX_BYTES + 1)
                    .expect("GIC bound should fit")
                    .to_le_bytes(),
            );
        }
    });
    assert!(matches!(
        decode_platform(&global),
        Err(HvfSnapshotV2DecodeError::InvalidGlobal)
    ));

    let topology = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_TOPOLOGY_COMPONENT_KEY {
            payload[20..24].copy_from_slice(&0_u32.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&topology),
        Err(HvfSnapshotV2DecodeError::InvalidTopology)
    ));

    let mandatory = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            payload[32..36].copy_from_slice(&0_u32.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&mandatory),
        Err(HvfSnapshotV2DecodeError::InvalidLength)
    ));

    for (offset, value) in [
        (24, 0_u32),
        (
            28,
            u32::try_from(TIME_PVTIME_ENTRY_BYTES - 1).expect("entry size should fit"),
        ),
    ] {
        let time = rebuild_components(&encoded, |key, _, payload| {
            if *key == NATIVE_V2_TIME_COMPONENT_KEY {
                payload[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
        });
        assert!(matches!(
            decode_platform(&time),
            Err(HvfSnapshotV2DecodeError::InvalidLength)
        ));
    }

    for (relative_offset, value) in [
        (24, 0_u32),
        (
            32,
            u32::try_from(OPTIONAL_MAX_RECORDS + 1).expect("record bound should fit"),
        ),
        (
            36,
            u32::try_from(OPTIONAL_MAX_REGISTRY_BYTES + 1).expect("registry bound should fit"),
        ),
    ] {
        let optional = rebuild_components(&encoded, |key, _, payload| {
            if *key == native_v2_vcpu_component_key(0) {
                let optional_start = optional_payload_offset(payload);
                if relative_offset == 24 {
                    payload[optional_start + relative_offset] =
                        u8::try_from(value).expect("breakpoint count should fit");
                } else {
                    payload[optional_start + relative_offset..optional_start + relative_offset + 4]
                        .copy_from_slice(&value.to_le_bytes());
                }
            }
        });
        assert!(matches!(
            decode_platform(&optional),
            Err(HvfSnapshotV2DecodeError::InvalidOptional)
        ));
    }
}

#[test]
fn rejects_duplicate_unknown_malformed_and_out_of_order_optional_records() {
    let encoded = encoded_fixture(false);
    let duplicate = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            let first_record = optional_record_offset(payload, OPTIONAL_TAG_BREAKPOINT_VALUE);
            payload[first_record..first_record + 2].copy_from_slice(&2_u16.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&duplicate),
        Err(HvfSnapshotV2DecodeError::InvalidOptional)
    ));

    let unknown = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            let first_record = optional_record_offset(payload, OPTIONAL_TAG_BREAKPOINT_VALUE);
            payload[first_record..first_record + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&unknown),
        Err(HvfSnapshotV2DecodeError::InvalidOptional)
    ));

    let wrong_width = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            let first_record = optional_record_offset(payload, OPTIONAL_TAG_BREAKPOINT_VALUE);
            payload[first_record + 4..first_record + 8].copy_from_slice(&7_u32.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&wrong_width),
        Err(HvfSnapshotV2DecodeError::InvalidOptional)
    ));

    let bad_disposition = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            let first_record = optional_record_offset(payload, OPTIONAL_TAG_BREAKPOINT_VALUE);
            payload[first_record + 2] = 9;
        }
    });
    assert!(matches!(
        decode_platform(&bad_disposition),
        Err(HvfSnapshotV2DecodeError::InvalidOptional)
    ));

    let bad_reserved = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            let first_record = optional_record_offset(payload, OPTIONAL_TAG_BREAKPOINT_VALUE);
            payload[first_record + 3] = 1;
        }
    });
    assert!(matches!(
        decode_platform(&bad_reserved),
        Err(HvfSnapshotV2DecodeError::NonzeroReserved)
    ));
}

#[test]
fn rejects_unknown_time_policies_and_malformed_time_records() {
    let encoded = encoded_fixture(false);
    for offset in 16..20 {
        let unknown_policy = rebuild_components(&encoded, |key, _, payload| {
            if *key == NATIVE_V2_TIME_COMPONENT_KEY {
                payload[offset] = u8::MAX;
            }
        });
        assert!(matches!(
            decode_platform(&unknown_policy),
            Err(HvfSnapshotV2DecodeError::InvalidTime)
        ));
    }

    let entry_reserved = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_TIME_COMPONENT_KEY {
            payload[TIME_HEADER_BYTES + 4] = 1;
        }
    });
    assert!(matches!(
        decode_platform(&entry_reserved),
        Err(HvfSnapshotV2DecodeError::NonzeroReserved)
    ));

    let out_of_order_index = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_TIME_COMPONENT_KEY {
            payload[TIME_HEADER_BYTES..TIME_HEADER_BYTES + 4].copy_from_slice(&1_u32.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&out_of_order_index),
        Err(HvfSnapshotV2DecodeError::InvalidTime)
    ));

    let invalid_vmclock = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_TIME_COMPONENT_KEY {
            payload[128] ^= u8::MAX;
        }
    });
    assert!(matches!(
        decode_platform(&invalid_vmclock),
        Err(HvfSnapshotV2DecodeError::InvalidTime)
    ));
}

#[test]
fn rejects_sme_size_feature_dependency_and_simd_alias_mutations() {
    let encoded = encoded_fixture(true);
    let oversized_svl = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            let optional_start = optional_payload_offset(payload);
            payload[optional_start + 28..optional_start + 32].copy_from_slice(
                &u32::try_from(HVF_SNAPSHOT_V2_MAX_SME_SVL_BYTES + 1)
                    .expect("SME bound should fit")
                    .to_le_bytes(),
            );
        }
    });
    assert!(matches!(
        decode_platform(&oversized_svl),
        Err(HvfSnapshotV2DecodeError::InvalidOptional)
    ));

    let feature_dependency = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            let pstate = optional_record_offset(payload, OPTIONAL_TAG_SME_PSTATE);
            payload[pstate + OPTIONAL_RECORD_HEADER_BYTES] = 0;
            payload[pstate + OPTIONAL_RECORD_HEADER_BYTES + 1] = 0;
        }
    });
    assert!(matches!(
        decode_platform(&feature_dependency),
        Err(HvfSnapshotV2DecodeError::InvalidOptional | HvfSnapshotV2DecodeError::TrailingData)
    ));

    let alias = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            let z0 = optional_record_offset(payload, OPTIONAL_TAG_SME_Z);
            payload[z0 + OPTIONAL_RECORD_HEADER_BYTES] ^= 1;
        }
    });
    assert!(matches!(
        decode_platform(&alias),
        Err(HvfSnapshotV2DecodeError::InvalidOptional)
    ));

    let common_identity = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            let optional_start = optional_payload_offset(payload);
            payload[optional_start + 40] ^= 1;
        }
    });
    assert!(matches!(
        decode_platform(&common_identity),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::CrossComponent
        ))
    ));
}

#[test]
fn rejects_locally_valid_cross_component_mismatches_in_stable_order() {
    let encoded = encoded_fixture(false);
    let vcpu_count = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            payload[16] = 1;
        }
    });
    assert!(matches!(
        decode_platform(&vcpu_count),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::CrossComponent
        ))
    ));

    let vcpu_position = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            payload[16..20].copy_from_slice(&1_u32.to_le_bytes());
            payload[24..32].copy_from_slice(&1_u64.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&vcpu_position),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::CrossComponent
        ))
    ));

    let primary_mpidr = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_GLOBAL_COMPONENT_KEY {
            payload[GLOBAL_HEADER_BYTES + 8..GLOBAL_HEADER_BYTES + 16]
                .copy_from_slice(&1_u64.to_le_bytes());
            payload[288..296].copy_from_slice(&1_u64.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&primary_mpidr),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::CrossComponent
        ))
    ));

    let memory = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            payload[24..32].copy_from_slice(&(FIXTURE_MEMORY_MIB + 1).to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&memory),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::Memory
        ))
    ));

    let fdt = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            let current =
                u64::from_le_bytes(payload[48..56].try_into().expect("fixed machine FDT field"));
            payload[48..56].copy_from_slice(&(current + 4096).to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&fdt),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::Fdt
        ))
    ));

    let timer = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_TOPOLOGY_COMPONENT_KEY {
            payload[16..20].copy_from_slice(&28_u32.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&timer),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::CrossComponent
        ))
    ));

    let optional_identity = rebuild_components(&encoded, |key, _, payload| {
        if *key == native_v2_vcpu_component_key(0) {
            let optional_start = optional_payload_offset(payload);
            let current = u64::from_le_bytes(
                payload[optional_start + 16..optional_start + 24]
                    .try_into()
                    .expect("fixed optional identity field"),
            );
            payload[optional_start + 16..optional_start + 24]
                .copy_from_slice(&(current ^ 1).to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&optional_identity),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::CrossComponent
        ))
    ));

    let redistributor_capacity = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_GLOBAL_COMPONENT_KEY {
            let redistributor_region_size = GLOBAL_HEADER_BYTES
                + 11 * size_of::<u64>()
                + 24
                + 3 * size_of::<u64>()
                + 16 * size_of::<u64>()
                + size_of::<u64>()
                + 16
                + size_of::<u64>();
            payload[redistributor_region_size..redistributor_region_size + 8]
                .copy_from_slice(&0x2_0000_u64.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&redistributor_capacity),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::CrossComponent
        ))
    ));

    let rtc_identity = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_TIME_COMPONENT_KEY {
            payload[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&rtc_identity),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::CrossComponent
        ))
    ));

    let vmgenid_line = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_TIME_COMPONENT_KEY {
            payload[80..84].copy_from_slice(&u32::MAX.to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&vmgenid_line),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::CrossComponent
        ))
    ));

    let pvtime_record = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_TIME_COMPONENT_KEY {
            let current = u64::from_le_bytes(
                payload[TIME_HEADER_BYTES + 8..TIME_HEADER_BYTES + 16]
                    .try_into()
                    .expect("fixed PVTime address"),
            );
            payload[TIME_HEADER_BYTES + 8..TIME_HEADER_BYTES + 16]
                .copy_from_slice(&(current + ARM64_PVTIME_STRUCTURE_ALIGNMENT).to_le_bytes());
        }
    });
    assert!(matches!(
        decode_platform(&pvtime_record),
        Err(HvfSnapshotV2DecodeError::Build(
            HvfSnapshotV2BuildError::CrossComponent
        ))
    ));
}

#[test]
fn topology_codec_preserves_every_lifecycle_disposition() {
    let topology = HvfArm64StablePausedTopologyState::new(
        27,
        vec![
            HvfArm64StablePausedTopologyMember::new(0, 0, HvfArm64StableVcpuDisposition::Runnable),
            HvfArm64StablePausedTopologyMember::new(1, 1, HvfArm64StableVcpuDisposition::Offline),
            HvfArm64StablePausedTopologyMember::new(
                2,
                2,
                HvfArm64StableVcpuDisposition::Suspended(
                    HvfArm64StableCpuSuspendState::new(
                        HvfArm64CpuSuspendConvention::Call32,
                        [1, 2, 3],
                        4,
                    )
                    .expect("Call32 state should validate"),
                ),
            ),
            HvfArm64StablePausedTopologyMember::new(
                3,
                3,
                HvfArm64StableVcpuDisposition::Suspended(
                    HvfArm64StableCpuSuspendState::new(
                        HvfArm64CpuSuspendConvention::Call64,
                        [5, 6, 7],
                        8,
                    )
                    .expect("Call64 state should validate"),
                ),
            ),
        ],
    )
    .expect("topology should validate");
    let encoded = encode_topology(&topology).expect("topology should encode");
    assert_eq!(
        decode_topology(&encoded).expect("topology should decode"),
        topology
    );
}

#[test]
fn bounds_inert_metadata_global_state_and_debug_output() {
    assert!(HvfSnapshotV2NativePath::try_from_bytes(b"").is_err());
    assert!(HvfSnapshotV2NativePath::try_from_bytes(b"bad\0path").is_err());
    assert!(
        HvfSnapshotV2NativePath::try_from_bytes(&vec![b'x'; HVF_SNAPSHOT_V2_MAX_PATH_BYTES + 1])
            .is_err()
    );
    let kernel = HvfSnapshotV2NativePath::try_from_bytes(b"/sensitive/kernel")
        .expect("path should validate");
    assert!(
        HvfSnapshotV2BootState::try_new(
            kernel.clone(),
            None,
            Some(&"x".repeat(HVF_SNAPSHOT_V2_MAX_BOOT_ARGUMENT_BYTES + 1))
        )
        .is_err()
    );
    assert!(HvfSnapshotV2BootState::try_new(kernel.clone(), None, Some("bad\0arg")).is_err());
    assert!(!format!("{kernel:?}").contains("sensitive"));

    let platform = platform_fixture(false);
    let debug = format!("{platform:?} {:?}", platform.machine().boot());
    assert!(!debug.contains("/fixture/kernel"));
    assert!(!debug.contains("secret=fixture"));
    assert!(!debug.contains("sensitive-gic-state"));

    let encoded =
        encode_hvf_snapshot_v2_platform_state(&platform).expect("redaction fixture should encode");
    let nul_path = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            payload[MACHINE_HEADER_BYTES] = 0;
        }
    });
    assert!(matches!(
        decode_platform(&nul_path),
        Err(HvfSnapshotV2DecodeError::InvalidMachine)
    ));

    let invalid_arguments = rebuild_components(&encoded, |key, _, payload| {
        if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
            let kernel_length =
                u32::from_le_bytes(payload[32..36].try_into().expect("kernel length")) as usize;
            let initrd_length =
                u32::from_le_bytes(payload[36..40].try_into().expect("initrd length")) as usize;
            payload[MACHINE_HEADER_BYTES + kernel_length + initrd_length] = 0xff;
        }
    });
    assert!(matches!(
        decode_platform(&invalid_arguments),
        Err(HvfSnapshotV2DecodeError::InvalidMachine)
    ));

    for presence_offset in [21, 22] {
        let presence_mismatch = rebuild_components(&encoded, |key, _, payload| {
            if *key == NATIVE_V2_MACHINE_COMPONENT_KEY {
                payload[presence_offset] = 0;
            }
        });
        assert!(matches!(
            decode_platform(&presence_mismatch),
            Err(HvfSnapshotV2DecodeError::InvalidLength)
        ));
    }

    let compatibility = platform.global().compatibility().clone();
    assert!(
        HvfSnapshotV2GlobalState::try_new(
            compatibility.clone(),
            HvfGicDeviceState::new(Vec::new())
        )
        .is_err()
    );
    assert!(
        HvfSnapshotV2GlobalState::try_new(
            compatibility,
            HvfGicDeviceState::new(vec![0; HVF_SNAPSHOT_V2_GIC_DEVICE_STATE_MAX_BYTES + 1])
        )
        .is_err()
    );
}

#[test]
fn admitted_component_maxima_fit_the_structural_file_budget() {
    let mandatory_bytes =
        encode_vcpu(platform_fixture(false).vcpus()[0].mandatory()).expect("vCPU should encode");
    let maximum_vcpu_count = usize::from(MAX_SUPPORTED_VCPUS);
    let maximum_component_count = 6_usize
        .checked_add(maximum_vcpu_count)
        .expect("component count should fit");
    let maximum_paths = HVF_SNAPSHOT_V2_MAX_PATH_BYTES
        .checked_mul(2)
        .expect("maximum path pair should fit");
    let maximum_cpu_entries = HVF_ARM64_CPU_TEMPLATE_APPLICATION_MAX_ENTRIES
        .checked_mul(MACHINE_CPU_ENTRY_BYTES)
        .expect("maximum CPU template entries should fit");
    let maximum_machine = [
        MACHINE_HEADER_BYTES,
        maximum_paths,
        HVF_SNAPSHOT_V2_MAX_BOOT_ARGUMENT_BYTES,
        maximum_cpu_entries,
    ]
    .into_iter()
    .try_fold(0_usize, usize::checked_add)
    .expect("maximum machine should fit");
    let maximum_global = [
        GLOBAL_HEADER_BYTES,
        GLOBAL_COMPATIBILITY_BYTES,
        HVF_SNAPSHOT_V2_GIC_DEVICE_STATE_MAX_BYTES,
    ]
    .into_iter()
    .try_fold(0_usize, usize::checked_add)
    .expect("maximum global state should fit");
    let maximum_topology = maximum_vcpu_count
        .checked_mul(TOPOLOGY_MEMBER_BYTES)
        .and_then(|members| TOPOLOGY_HEADER_BYTES.checked_add(members))
        .expect("maximum topology should fit");
    let maximum_vcpu = [
        VCPU_HEADER_BYTES,
        mandatory_bytes.len(),
        VCPU_INTERRUPT_BYTES,
        OPTIONAL_HEADER_BYTES,
        OPTIONAL_MAX_REGISTRY_BYTES,
    ]
    .into_iter()
    .try_fold(0_usize, usize::checked_add)
    .expect("maximum vCPU should fit");
    let maximum_vcpus = maximum_vcpu_count
        .checked_mul(maximum_vcpu)
        .expect("maximum vCPU vector should fit");
    let maximum_time = maximum_vcpu_count
        .checked_mul(TIME_PVTIME_ENTRY_BYTES)
        .and_then(|entries| TIME_HEADER_BYTES.checked_add(entries))
        .expect("maximum time state should fit");
    let maximum_directories = maximum_component_count
        .checked_mul(NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES)
        .expect("maximum component directory should fit");
    let maximum_memory_extents = bangbang_runtime::snapshot_memory_v2::NATIVE_V2_MEMORY_MAX_EXTENTS
        .checked_mul(bangbang_runtime::snapshot_memory_v2::NATIVE_V2_MEMORY_EXTENT_BYTES)
        .expect("maximum memory extents should fit");
    let maximum_file = [
        bangbang_runtime::snapshot_format_v2::NATIVE_V2_SNAPSHOT_HEADER_BYTES,
        maximum_directories,
        maximum_machine,
        maximum_global,
        maximum_topology,
        maximum_vcpus,
        maximum_time,
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_BYTES,
        NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES,
        bangbang_runtime::snapshot_memory_v2::NATIVE_V2_MEMORY_HEADER_BYTES,
        maximum_memory_extents,
    ]
    .into_iter()
    .try_fold(0_usize, usize::checked_add)
    .expect("complete maximum snapshot profile should fit usize");
    assert_eq!(maximum_file, 16_427_575);
    assert!(
        maximum_file <= NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES,
        "profile maxima require {maximum_file} bytes"
    );
    assert_eq!(NATIVE_V2_VCPU_COMPONENT_KIND, 5);
}
