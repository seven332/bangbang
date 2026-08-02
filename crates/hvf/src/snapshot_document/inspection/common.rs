use bangbang_runtime::machine::{MachineConfig, MachineConfigCpuTemplate, MachineConfigHugePages};
use bangbang_runtime::memory::GuestMemoryRange;
use bangbang_runtime::snapshot_artifact::{
    NativeSnapshotArtifactFamily, NativeV2SnapshotArtifactProfile,
};
use bangbang_runtime::snapshot_format::{SnapshotArchitecture, SnapshotFormatVersion};
use bangbang_runtime::snapshot_memory::{SnapshotMemoryBinding, SnapshotMemoryRangeBinding};
use bangbang_runtime::snapshot_memory_v2::{SnapshotV2MemoryBinding, SnapshotV2MemoryExtent};
use serde::Serialize;
use serde::ser::{Error as _, SerializeSeq, SerializeStruct};

use crate::gic::{HvfArm64GicIccRegisterState, HvfGicDeviceState};
use crate::optional_state::{
    HvfArm64DebugRegisterRestoreState, HvfArm64OptionalStateValue,
    HvfArm64ReviewedOptionalStateRestore, HvfArm64SmeRestoreState,
};
use crate::snapshot::HvfArm64SnapshotTimerState;
use crate::snapshot_bundle::{
    HvfSnapshotV1CompatibilityState, HvfSnapshotV1InterruptState, HvfSnapshotV1VcpuState,
};
use crate::snapshot_v2::{
    HvfSnapshotV2MachineState, HvfSnapshotV2PlatformState, HvfSnapshotV2PvTimeRestorePolicy,
    HvfSnapshotV2PvTimeVcpuState, HvfSnapshotV2RtcRestorePolicy, HvfSnapshotV2VcpuState,
    HvfSnapshotV2VmClockRestorePolicy, HvfSnapshotV2VmGenIdRestorePolicy,
};
use crate::vcpu::{
    HvfArm64VcpuCacheSelectionRegisterState, HvfArm64VcpuCoreSystemRegisterState,
    HvfArm64VcpuDebugControlRegisterState, HvfArm64VcpuDebugTrapState,
    HvfArm64VcpuExceptionRegisterState, HvfArm64VcpuExecutionControlRegisterState,
    HvfArm64VcpuGeneralRegisterState, HvfArm64VcpuPendingInterruptState,
    HvfArm64VcpuPointerAuthenticationKeyState, HvfArm64VcpuSimdFpState,
    HvfArm64VcpuSystemContextRegisterState, HvfArm64VcpuThreadContextRegisterState,
    HvfArm64VcpuTranslationRegisterState,
};

use super::fingerprint::{
    Fingerprint, FingerprintBuilder, HexU16, HexU32, HexU64, HexU128, Redacted, RedactedOption,
    confidential_bytes,
};
use super::{HvfNativeSnapshotDocument, HvfNativeSnapshotDocumentState, platform_v2};
use crate::snapshot_document::HvfNativeSnapshotDocumentProfile;

pub(super) struct Family(pub(super) NativeSnapshotArtifactFamily);

impl Serialize for Family {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            NativeSnapshotArtifactFamily::V1 => "native-v1",
            NativeSnapshotArtifactFamily::V2 => "native-v2",
        })
    }
}

pub(super) struct Profile(pub(super) HvfNativeSnapshotDocumentProfile);

impl Serialize for Profile {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = match self.0 {
            HvfNativeSnapshotDocumentProfile::V1 => "v1",
            HvfNativeSnapshotDocumentProfile::V2(
                NativeV2SnapshotArtifactProfile::LegacyPlatformV2_3,
            ) => "legacy-platform-v2.3",
            HvfNativeSnapshotDocumentProfile::V2(
                NativeV2SnapshotArtifactProfile::DeviceGraphV2_4,
            ) => "device-graph-v2.4",
            HvfNativeSnapshotDocumentProfile::V2(
                NativeV2SnapshotArtifactProfile::MultiBlockDeviceGraphV2_5,
            ) => "multi-block-device-graph-v2.5",
            HvfNativeSnapshotDocumentProfile::V2(
                NativeV2SnapshotArtifactProfile::StorageDeviceGraphV2_6,
            ) => "storage-device-graph-v2.6",
            HvfNativeSnapshotDocumentProfile::V2(
                NativeV2SnapshotArtifactProfile::SerialStateV2_7,
            ) => "serial-state-v2.7",
            HvfNativeSnapshotDocumentProfile::V2(
                NativeV2SnapshotArtifactProfile::EntropyStateV2_8,
            ) => "entropy-state-v2.8",
            HvfNativeSnapshotDocumentProfile::V2(
                NativeV2SnapshotArtifactProfile::BalloonStateV2_9,
            ) => "balloon-state-v2.9",
            HvfNativeSnapshotDocumentProfile::V2(
                NativeV2SnapshotArtifactProfile::MemoryHotplugStateV2_10,
            ) => "memory-hotplug-state-v2.10",
            HvfNativeSnapshotDocumentProfile::V2(
                NativeV2SnapshotArtifactProfile::NetworkStateV2_11,
            ) => "network-state-v2.11",
            HvfNativeSnapshotDocumentProfile::V2(
                NativeV2SnapshotArtifactProfile::VsockStateV2_12,
            ) => "vsock-state-v2.12",
            HvfNativeSnapshotDocumentProfile::V2(
                NativeV2SnapshotArtifactProfile::DiffStateV2_13,
            ) => "diff-state-v2.13",
        };
        serializer.serialize_str(value)
    }
}

pub(super) struct Version(pub(super) SnapshotFormatVersion);

impl Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("SnapshotVersion", 3)?;
        state.serialize_field("major", &self.0.major())?;
        state.serialize_field("minor", &self.0.minor())?;
        state.serialize_field("patch", &self.0.patch())?;
        state.end()
    }
}

pub(super) struct Memory<'a>(pub(super) &'a HvfNativeSnapshotDocument);

impl Serialize for Memory<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0.state {
            HvfNativeSnapshotDocumentState::V1(bundle) => {
                V1Memory(bundle.commit_record().memory_binding()).serialize(serializer)
            }
            state => platform_v2(state)
                .ok_or_else(|| S::Error::custom("native-v2 inspection platform is missing"))
                .and_then(|platform| V2Memory(platform.memory()).serialize(serializer)),
        }
    }
}

struct V1Memory<'a>(&'a SnapshotMemoryBinding);

impl Serialize for V1Memory<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let binding = self.0;
        let mut identity = FingerprintBuilder::new("memory.v1.binding-identity");
        identity.bytes(binding.image_id().as_bytes());
        identity.u64(binding.checksum());

        let mut state = serializer.serialize_struct("NativeV1Memory", 10)?;
        state.serialize_field("version", &Version(binding.version()))?;
        state.serialize_field(
            "architecture",
            match binding.architecture() {
                SnapshotArchitecture::Arm64 => "arm64",
            },
        )?;
        state.serialize_field("guest_page_size", &binding.guest_page_size())?;
        state.serialize_field("data_length", &binding.data_length())?;
        state.serialize_field("file_length", &binding.file_length())?;
        state.serialize_field("integrity", "crc64-jones")?;
        state.serialize_field("binding_identity", &identity.finish())?;
        state.serialize_field("range_count", &binding.ranges().len())?;
        state.serialize_field("ranges", &V1MemoryRanges(binding.ranges()))?;
        state.serialize_field("sparse", &false)?;
        state.end()
    }
}

struct V1MemoryRanges<'a>(&'a [SnapshotMemoryRangeBinding]);

impl Serialize for V1MemoryRanges<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for binding in self.0 {
            sequence.serialize_element(&MemoryRange {
                range: binding.range(),
                file_offset: binding.file_offset(),
            })?;
        }
        sequence.end()
    }
}

pub(super) struct V2Memory<'a>(pub(super) &'a SnapshotV2MemoryBinding);

impl Serialize for V2Memory<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let binding = self.0;
        let mut identity = FingerprintBuilder::new("memory.v2.binding-identity");
        identity.bytes(binding.image_id().as_bytes());
        identity.u64(binding.metadata_checksum());

        let mut state = serializer.serialize_struct("NativeV2Memory", 8)?;
        state.serialize_field("version", &Version(binding.version()))?;
        state.serialize_field(
            "guest_granule",
            &bangbang_runtime::snapshot_memory_v2::NATIVE_V2_MEMORY_GUEST_GRANULE,
        )?;
        state.serialize_field("file_length", &binding.file_length())?;
        state.serialize_field("binding_identity", &identity.finish())?;
        state.serialize_field("extent_count", &binding.extents().len())?;
        state.serialize_field("extents", &V2MemoryExtents(binding.extents()))?;
        state.serialize_field(
            "alignment",
            &bangbang_runtime::snapshot_memory_v2::NATIVE_V2_MEMORY_ALIGNMENT,
        )?;
        state.serialize_field("sparse", &true)?;
        state.end()
    }
}

struct V2MemoryExtents<'a>(&'a [SnapshotV2MemoryExtent]);

impl Serialize for V2MemoryExtents<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for extent in self.0 {
            sequence.serialize_element(&MemoryRange {
                range: extent.range(),
                file_offset: extent.file_offset(),
            })?;
        }
        sequence.end()
    }
}

pub(super) struct MemoryRange {
    pub(super) range: GuestMemoryRange,
    pub(super) file_offset: u64,
}

impl Serialize for MemoryRange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("MemoryRange", 3)?;
        state.serialize_field("start", &HexU64(self.range.start().raw_value()))?;
        state.serialize_field("size", &self.range.size())?;
        state.serialize_field("file_offset", &self.file_offset)?;
        state.end()
    }
}

pub(super) struct Machine<'a>(pub(super) &'a HvfNativeSnapshotDocument);

impl Serialize for Machine<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0.state {
            HvfNativeSnapshotDocumentState::V1(bundle) => {
                V1Machine(bundle.state().machine()).serialize(serializer)
            }
            state => platform_v2(state)
                .ok_or_else(|| S::Error::custom("native-v2 inspection platform is missing"))
                .and_then(|platform| V2Machine(platform.machine()).serialize(serializer)),
        }
    }
}

struct V1Machine(MachineConfig);

impl Serialize for V1Machine {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("NativeV1Machine", 5)?;
        state.serialize_field("config", &MachineConfigView(self.0))?;
        state.serialize_field("boot", &Option::<()>::None)?;
        state.serialize_field("fdt", &Option::<()>::None)?;
        state.serialize_field("cpu_template_application", &Option::<()>::None)?;
        state.serialize_field("profile", "native-v1")?;
        state.end()
    }
}

struct V2Machine<'a>(&'a HvfSnapshotV2MachineState);

impl Serialize for V2Machine<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let machine = self.0;
        let boot = machine.boot();
        let fdt = machine.fdt();
        let mut state = serializer.serialize_struct("NativeV2Machine", 5)?;
        state.serialize_field("config", &MachineConfigView(machine.machine()))?;
        state.serialize_field(
            "boot",
            &Boot {
                has_initrd: boot.initrd_path().is_some(),
                has_arguments: boot.boot_arguments().is_some(),
            },
        )?;
        state.serialize_field(
            "fdt",
            &Fdt {
                address: fdt.address().raw_value(),
                size: fdt.size(),
                product_process_profile: fdt.is_product_process_profile(),
            },
        )?;
        state.serialize_field(
            "cpu_template_application",
            &machine.cpu_template().map(CpuTemplateApplication),
        )?;
        state.serialize_field("profile", "native-v2")?;
        state.end()
    }
}

struct MachineConfigView(MachineConfig);

impl Serialize for MachineConfigView {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let machine = self.0;
        let mut state = serializer.serialize_struct("MachineConfig", 6)?;
        state.serialize_field("vcpu_count", &machine.vcpu_count())?;
        state.serialize_field("mem_size_mib", &machine.mem_size_mib())?;
        state.serialize_field("smt", &machine.smt())?;
        state.serialize_field("cpu_template", &machine.cpu_template().map(CpuTemplate))?;
        state.serialize_field("track_dirty_pages", &machine.track_dirty_pages())?;
        state.serialize_field("huge_pages", &HugePages(machine.huge_pages()))?;
        state.end()
    }
}

struct CpuTemplate(MachineConfigCpuTemplate);

impl Serialize for CpuTemplate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            MachineConfigCpuTemplate::C3 => "c3",
            MachineConfigCpuTemplate::T2 => "t2",
            MachineConfigCpuTemplate::T2S => "t2s",
            MachineConfigCpuTemplate::T2CL => "t2cl",
            MachineConfigCpuTemplate::T2A => "t2a",
            MachineConfigCpuTemplate::V1N1 => "v1n1",
            MachineConfigCpuTemplate::None => "none",
        })
    }
}

struct HugePages(MachineConfigHugePages);

impl Serialize for HugePages {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self.0 {
            MachineConfigHugePages::None => "none",
            MachineConfigHugePages::TwoM => "2m",
        })
    }
}

struct Boot {
    has_initrd: bool,
    has_arguments: bool,
}

impl Serialize for Boot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Boot", 3)?;
        state.serialize_field("kernel_path", &Redacted)?;
        state.serialize_field("initrd_path", &RedactedOption(self.has_initrd))?;
        state.serialize_field("arguments", &RedactedOption(self.has_arguments))?;
        state.end()
    }
}

struct Fdt {
    address: u64,
    size: u32,
    product_process_profile: bool,
}

impl Serialize for Fdt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Fdt", 4)?;
        state.serialize_field("address", &HexU64(self.address))?;
        state.serialize_field("size", &self.size)?;
        // The checksum covers low-entropy boot choices, including arguments;
        // exposing it raw or hashing it would create a guessable oracle.
        state.serialize_field("checksum", &Redacted)?;
        state.serialize_field("product_process_profile", &self.product_process_profile)?;
        state.end()
    }
}

struct CpuTemplateApplication<'a>(&'a crate::HvfArm64CpuTemplateApplicationState);

impl Serialize for CpuTemplateApplication<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("CpuTemplateApplication", 2)?;
        state.serialize_field("entry_count", &self.0.entries().len())?;
        state.serialize_field("entries", &CpuTemplateEntries(self.0.entries()))?;
        state.end()
    }
}

struct CpuTemplateEntries<'a>(&'a [crate::HvfArm64CpuTemplateApplicationEntry]);

impl Serialize for CpuTemplateEntries<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for entry in self.0 {
            sequence.serialize_element(&CpuTemplateEntry(entry))?;
        }
        sequence.end()
    }
}

struct CpuTemplateEntry<'a>(&'a crate::HvfArm64CpuTemplateApplicationEntry);

impl Serialize for CpuTemplateEntry<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use crate::HvfArm64CpuTemplateValueWidth;

        let entry = self.0;
        let mut state = serializer.serialize_struct("CpuTemplateEntry", 6)?;
        state.serialize_field("tag", &HexU16(entry.tag().raw()))?;
        state.serialize_field(
            "width",
            match entry.width() {
                HvfArm64CpuTemplateValueWidth::U32 => "u32",
                HvfArm64CpuTemplateValueWidth::U64 => "u64",
                HvfArm64CpuTemplateValueWidth::U128 => "u128",
            },
        )?;
        state.serialize_field("filter", &CpuTemplateValue(entry.filter()))?;
        state.serialize_field("logical_value", &CpuTemplateValue(entry.logical_value()))?;
        state.serialize_field(
            "common_baseline",
            &CpuTemplateValue(entry.common_baseline()),
        )?;
        state.serialize_field(
            "effective_value",
            &CpuTemplateValue(entry.effective_value()),
        )?;
        state.end()
    }
}

struct CpuTemplateValue(crate::HvfArm64CpuTemplateValue);

impl Serialize for CpuTemplateValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            crate::HvfArm64CpuTemplateValue::U32(value) => HexU32(value).serialize(serializer),
            crate::HvfArm64CpuTemplateValue::U64(value) => HexU64(value).serialize(serializer),
            crate::HvfArm64CpuTemplateValue::U128(value) => HexU128(value).serialize(serializer),
        }
    }
}

pub(super) struct Vcpus<'a>(pub(super) &'a HvfNativeSnapshotDocument);

impl Serialize for Vcpus<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0.state {
            HvfNativeSnapshotDocumentState::V1(bundle) => {
                let mut sequence = serializer.serialize_seq(Some(1))?;
                sequence.serialize_element(&Vcpu::V1 {
                    index: 0,
                    mpidr: bundle.state().compatibility().primary_mpidr(),
                    state: bundle.state().vcpu(),
                    interrupts: bundle.state().interrupts(),
                })?;
                sequence.end()
            }
            state => {
                let platform = platform_v2(state)
                    .ok_or_else(|| S::Error::custom("native-v2 inspection platform is missing"))?;
                let mut sequence = serializer.serialize_seq(Some(platform.vcpus().len()))?;
                for vcpu in platform.vcpus() {
                    sequence.serialize_element(&Vcpu::V2(vcpu))?;
                }
                sequence.end()
            }
        }
    }
}

enum Vcpu<'a> {
    V1 {
        index: u32,
        mpidr: u64,
        state: &'a HvfSnapshotV1VcpuState,
        interrupts: &'a HvfSnapshotV1InterruptState,
    },
    V2(&'a HvfSnapshotV2VcpuState),
}

impl Serialize for Vcpu<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let (index, mpidr, mandatory, timer, pending, gic_icc, reviewed) = match self {
            Self::V1 {
                index,
                mpidr,
                state,
                interrupts,
            } => (
                *index,
                *mpidr,
                *state,
                interrupts.timer,
                interrupts.pending_interrupts,
                interrupts.gic_icc,
                None,
            ),
            Self::V2(state) => (
                state.index(),
                state.mpidr(),
                state.mandatory(),
                *state.timer(),
                state.pending_interrupts(),
                state.gic_icc(),
                Some(state.reviewed_optional()),
            ),
        };

        let mut output = serializer.serialize_struct("Vcpu", 17)?;
        output.serialize_field("index", &index)?;
        output.serialize_field("mpidr", &HexU64(mpidr))?;
        output.serialize_field("general", &General(&mandatory.general))?;
        output.serialize_field("core", &Core(mandatory.core))?;
        output.serialize_field("exception", &Exception(mandatory.exception))?;
        output.serialize_field("execution", &Execution(mandatory.execution))?;
        output.serialize_field(
            "cache_selection",
            &CacheSelection(mandatory.cache_selection),
        )?;
        output.serialize_field(
            "debug",
            &DebugState {
                control: mandatory.debug_control,
                trap: mandatory.debug_trap,
                reviewed,
            },
        )?;
        output.serialize_field("system_context", &SystemContext(mandatory.system_context))?;
        output.serialize_field("translation", &Translation(mandatory.translation))?;
        output.serialize_field(
            "pointer_authentication",
            &pointer_authentication_fingerprint(&mandatory.pointer_authentication),
        )?;
        output.serialize_field("thread_context", &ThreadContext(mandatory.thread_context))?;
        output.serialize_field("simd_fp", &simd_fp_fingerprint(&mandatory.simd_fp))?;
        output.serialize_field("timer", &Timer(timer))?;
        output.serialize_field("pending_interrupts", &PendingInterrupts(pending))?;
        output.serialize_field("gic_icc", &gic_icc_fingerprint(gic_icc))?;
        output.serialize_field(
            "reviewed_sme",
            &reviewed.and_then(|value| value.sme()).map(Sme),
        )?;
        output.end()
    }
}

struct HexU64Slice<'a>(&'a [u64]);

impl Serialize for HexU64Slice<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for value in self.0 {
            sequence.serialize_element(&HexU64(*value))?;
        }
        sequence.end()
    }
}

struct General<'a>(&'a HvfArm64VcpuGeneralRegisterState);

impl Serialize for General<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("GeneralRegisters", 3)?;
        state.serialize_field("x", &HexU64Slice(self.0.general_purpose_registers()))?;
        state.serialize_field("pc", &HexU64(self.0.pc()))?;
        state.serialize_field("cpsr", &HexU64(self.0.cpsr()))?;
        state.end()
    }
}

struct Core(HvfArm64VcpuCoreSystemRegisterState);

impl Serialize for Core {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("CoreRegisters", 4)?;
        state.serialize_field("sp_el0", &HexU64(self.0.sp_el0()))?;
        state.serialize_field("sp_el1", &HexU64(self.0.sp_el1()))?;
        state.serialize_field("elr_el1", &HexU64(self.0.elr_el1()))?;
        state.serialize_field("spsr_el1", &HexU64(self.0.spsr_el1()))?;
        state.end()
    }
}

struct Exception(HvfArm64VcpuExceptionRegisterState);

impl Serialize for Exception {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("ExceptionRegisters", 6)?;
        state.serialize_field("afsr0_el1", &HexU64(self.0.afsr0_el1()))?;
        state.serialize_field("afsr1_el1", &HexU64(self.0.afsr1_el1()))?;
        state.serialize_field("esr_el1", &HexU64(self.0.esr_el1()))?;
        state.serialize_field("far_el1", &HexU64(self.0.far_el1()))?;
        state.serialize_field("par_el1", &HexU64(self.0.par_el1()))?;
        state.serialize_field("vbar_el1", &HexU64(self.0.vbar_el1()))?;
        state.end()
    }
}

struct Execution(HvfArm64VcpuExecutionControlRegisterState);

impl Serialize for Execution {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("ExecutionRegisters", 2)?;
        state.serialize_field("actlr_el1", &HexU64(self.0.actlr_el1()))?;
        state.serialize_field("cpacr_el1", &HexU64(self.0.cpacr_el1()))?;
        state.end()
    }
}

struct CacheSelection(HvfArm64VcpuCacheSelectionRegisterState);

impl Serialize for CacheSelection {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("CacheSelection", 1)?;
        state.serialize_field("csselr_el1", &HexU64(self.0.csselr_el1()))?;
        state.end()
    }
}

struct SystemContext(HvfArm64VcpuSystemContextRegisterState);

impl Serialize for SystemContext {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("SystemContext", 2)?;
        state.serialize_field("scxtnum_el0", &HexU64(self.0.scxtnum_el0()))?;
        state.serialize_field("scxtnum_el1", &HexU64(self.0.scxtnum_el1()))?;
        state.end()
    }
}

struct Translation(HvfArm64VcpuTranslationRegisterState);

impl Serialize for Translation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("TranslationRegisters", 7)?;
        state.serialize_field("sctlr_el1", &HexU64(self.0.sctlr_el1()))?;
        state.serialize_field("ttbr0_el1", &HexU64(self.0.ttbr0_el1()))?;
        state.serialize_field("ttbr1_el1", &HexU64(self.0.ttbr1_el1()))?;
        state.serialize_field("tcr_el1", &HexU64(self.0.tcr_el1()))?;
        state.serialize_field("mair_el1", &HexU64(self.0.mair_el1()))?;
        state.serialize_field("amair_el1", &HexU64(self.0.amair_el1()))?;
        state.serialize_field("contextidr_el1", &HexU64(self.0.contextidr_el1()))?;
        state.end()
    }
}

struct ThreadContext(HvfArm64VcpuThreadContextRegisterState);

impl Serialize for ThreadContext {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("ThreadContext", 3)?;
        state.serialize_field("tpidr_el0", &HexU64(self.0.tpidr_el0()))?;
        state.serialize_field("tpidrro_el0", &HexU64(self.0.tpidrro_el0()))?;
        state.serialize_field("tpidr_el1", &HexU64(self.0.tpidr_el1()))?;
        state.end()
    }
}

struct Timer(HvfArm64SnapshotTimerState);

impl Serialize for Timer {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Timer", 7)?;
        state.serialize_field(
            "virtual_timer_exit_masked",
            &self.0.virtual_timer_exit_masked(),
        )?;
        state.serialize_field("cntkctl_el1", &HexU64(self.0.cntkctl_el1()))?;
        state.serialize_field("virtual_count", &HexU64(self.0.virtual_count()))?;
        state.serialize_field("virtual_control", &HexU64(self.0.virtual_control()))?;
        state.serialize_field(
            "virtual_compare_value",
            &HexU64(self.0.virtual_compare_value()),
        )?;
        state.serialize_field("physical_control", &HexU64(self.0.physical_control()))?;
        state.serialize_field(
            "physical_compare_delta",
            &HexU64(self.0.physical_compare_delta()),
        )?;
        state.end()
    }
}

struct PendingInterrupts(HvfArm64VcpuPendingInterruptState);

impl Serialize for PendingInterrupts {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("PendingInterrupts", 2)?;
        state.serialize_field("irq", &self.0.irq_pending())?;
        state.serialize_field("fiq", &self.0.fiq_pending())?;
        state.end()
    }
}

struct DebugState<'a> {
    control: HvfArm64VcpuDebugControlRegisterState,
    trap: HvfArm64VcpuDebugTrapState,
    reviewed: Option<&'a HvfArm64ReviewedOptionalStateRestore>,
}

impl Serialize for DebugState<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut mandatory = FingerprintBuilder::new("vcpu.debug.mandatory");
        mandatory.u64(self.control.mdccint_el1());
        mandatory.u64(self.control.mdscr_el1());
        mandatory.bool(self.trap.trap_debug_exceptions());
        mandatory.bool(self.trap.trap_debug_reg_accesses());

        let (breakpoint_count, watchpoint_count, reviewed) =
            self.reviewed.map_or((None, None, None), |value| {
                (
                    Some(value.breakpoints().implemented_count()),
                    Some(value.watchpoints().implemented_count()),
                    Some(reviewed_debug_fingerprint(value)),
                )
            });

        let mut state = serializer.serialize_struct("DebugState", 5)?;
        state.serialize_field("mandatory", &mandatory.finish())?;
        state.serialize_field("breakpoint_count", &breakpoint_count)?;
        state.serialize_field("watchpoint_count", &watchpoint_count)?;
        state.serialize_field("reviewed", &reviewed)?;
        state.serialize_field("policy", "confidential-equality-only")?;
        state.end()
    }
}

struct Sme<'a>(&'a HvfArm64SmeRestoreState);

impl Serialize for Sme<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let sme = self.0;
        let pstate_source = optional_source(&sme.pstate());
        let system_sources = OptionalSources(sme.system_registers());
        let z_count = sme
            .z_registers()
            .map(<[HvfArm64OptionalStateValue<Box<[u8]>>]>::len);
        let p_count = sme
            .p_registers()
            .map(<[HvfArm64OptionalStateValue<Box<[u8]>>]>::len);
        let mut state = serializer.serialize_struct("SmeState", 10)?;
        state.serialize_field("version", &sme.version())?;
        state.serialize_field("maximum_svl_bytes", &sme.maximum_svl_bytes())?;
        state.serialize_field("pstate_source", pstate_source)?;
        state.serialize_field("system_register_sources", &system_sources)?;
        state.serialize_field("z_register_count", &z_count)?;
        state.serialize_field("p_register_count", &p_count)?;
        state.serialize_field("za_present", &sme.za_register().is_some())?;
        state.serialize_field("zt0_present", &sme.zt0_register().is_some())?;
        state.serialize_field("state", &sme_fingerprint(sme))?;
        state.serialize_field("policy", "confidential-equality-only")?;
        state.end()
    }
}

struct OptionalSources<'a>(&'a [HvfArm64OptionalStateValue<u64>]);

impl Serialize for OptionalSources<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for value in self.0 {
            sequence.serialize_element(optional_source(value))?;
        }
        sequence.end()
    }
}

fn optional_source<T>(value: &HvfArm64OptionalStateValue<T>) -> &'static str {
    match value {
        HvfArm64OptionalStateValue::Explicit(_) => "explicit",
        HvfArm64OptionalStateValue::DestinationDefault => "destination-default",
    }
}

fn pointer_authentication_fingerprint(
    state: &HvfArm64VcpuPointerAuthenticationKeyState,
) -> Fingerprint {
    let mut builder = FingerprintBuilder::new("vcpu.pointer-authentication");
    builder.u128(state.apia_key());
    builder.u128(state.apib_key());
    builder.u128(state.apda_key());
    builder.u128(state.apdb_key());
    builder.u128(state.apga_key());
    builder.finish()
}

fn simd_fp_fingerprint(state: &HvfArm64VcpuSimdFpState) -> Fingerprint {
    let mut builder = FingerprintBuilder::new("vcpu.simd-fp");
    builder.sequence_len(state.q_registers().len());
    for register in state.q_registers() {
        builder.bytes(register);
    }
    builder.u64(state.fpcr());
    builder.u64(state.fpsr());
    builder.finish()
}

fn gic_icc_fingerprint(state: HvfArm64GicIccRegisterState) -> Fingerprint {
    let mut builder = FingerprintBuilder::new("vcpu.gic-icc");
    builder.u64(state.pmr_el1());
    builder.u64(state.bpr0_el1());
    builder.u64(state.ap0r0_el1());
    builder.u64(state.ap1r0_el1());
    builder.u64(state.rpr_el1());
    builder.u64(state.bpr1_el1());
    builder.u64(state.ctlr_el1());
    builder.u64(state.sre_el1());
    builder.u64(state.igrpen0_el1());
    builder.u64(state.igrpen1_el1());
    builder.finish()
}

fn reviewed_debug_fingerprint(state: &HvfArm64ReviewedOptionalStateRestore) -> Fingerprint {
    let mut builder = FingerprintBuilder::new("vcpu.debug.reviewed");
    builder.u64(state.expected_id_aa64dfr0_el1());
    append_debug_inventory(&mut builder, state.breakpoints());
    append_debug_inventory(&mut builder, state.watchpoints());
    builder.finish()
}

fn append_debug_inventory(
    builder: &mut FingerprintBuilder,
    state: &HvfArm64DebugRegisterRestoreState,
) {
    builder.u8(state.implemented_count());
    for value in state.values().iter().chain(state.controls()) {
        append_optional_u64(builder, value);
    }
}

fn append_optional_u64(builder: &mut FingerprintBuilder, value: &HvfArm64OptionalStateValue<u64>) {
    match value {
        HvfArm64OptionalStateValue::Explicit(value) => {
            builder.tag(1);
            builder.u64(*value);
        }
        HvfArm64OptionalStateValue::DestinationDefault => builder.tag(0),
    }
}

fn sme_fingerprint(state: &HvfArm64SmeRestoreState) -> Fingerprint {
    let mut builder = FingerprintBuilder::new("vcpu.sme");
    builder.u8(state.version());
    let identification = state.identification();
    builder.u64(identification.id_aa64zfr0_el1());
    builder.u64(identification.id_aa64smfr0_el1());
    builder.u64(state.maximum_svl_bytes() as u64);
    match state.pstate() {
        HvfArm64OptionalStateValue::Explicit(value) => {
            builder.tag(1);
            builder.bool(value.streaming_sve_mode_enabled());
            builder.bool(value.za_storage_enabled());
        }
        HvfArm64OptionalStateValue::DestinationDefault => builder.tag(0),
    }
    for value in state.system_registers() {
        append_optional_u64(&mut builder, value);
    }
    append_optional_byte_inventory(&mut builder, state.z_registers());
    append_optional_byte_inventory(&mut builder, state.p_registers());
    append_optional_bytes(
        &mut builder,
        state.za_register().map(|value| match value {
            HvfArm64OptionalStateValue::Explicit(bytes) => {
                HvfArm64OptionalStateValue::Explicit(bytes.as_ref())
            }
            HvfArm64OptionalStateValue::DestinationDefault => {
                HvfArm64OptionalStateValue::DestinationDefault
            }
        }),
    );
    append_optional_bytes(
        &mut builder,
        state.zt0_register().map(|value| match value {
            HvfArm64OptionalStateValue::Explicit(bytes) => {
                HvfArm64OptionalStateValue::Explicit(bytes.as_slice())
            }
            HvfArm64OptionalStateValue::DestinationDefault => {
                HvfArm64OptionalStateValue::DestinationDefault
            }
        }),
    );
    builder.finish()
}

fn append_optional_byte_inventory(
    builder: &mut FingerprintBuilder,
    inventory: Option<&[HvfArm64OptionalStateValue<Box<[u8]>>]>,
) {
    match inventory {
        Some(values) => {
            builder.tag(1);
            builder.sequence_len(values.len());
            for value in values {
                match value {
                    HvfArm64OptionalStateValue::Explicit(bytes) => {
                        builder.tag(1);
                        builder.bytes(bytes);
                    }
                    HvfArm64OptionalStateValue::DestinationDefault => builder.tag(0),
                }
            }
        }
        None => builder.tag(0),
    }
}

fn append_optional_bytes(
    builder: &mut FingerprintBuilder,
    value: Option<HvfArm64OptionalStateValue<&[u8]>>,
) {
    match value {
        Some(HvfArm64OptionalStateValue::Explicit(bytes)) => {
            builder.tag(2);
            builder.bytes(bytes);
        }
        Some(HvfArm64OptionalStateValue::DestinationDefault) => builder.tag(1),
        None => builder.tag(0),
    }
}

pub(super) fn gic_device_fingerprint(state: &HvfGicDeviceState) -> Fingerprint {
    confidential_bytes("global.gic-device", state.as_bytes())
}

// Implemented below in the platform section.
pub(super) struct Global<'a>(pub(super) &'a HvfNativeSnapshotDocument);
pub(super) struct Topology<'a>(pub(super) &'a HvfNativeSnapshotDocument);
pub(super) struct Time<'a>(pub(super) &'a HvfNativeSnapshotDocument);

impl Serialize for Global<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        GlobalState(self.0).serialize(serializer)
    }
}

impl Serialize for Topology<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        TopologyState(self.0).serialize(serializer)
    }
}

impl Serialize for Time<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        TimeState(self.0).serialize(serializer)
    }
}

struct GlobalState<'a>(&'a HvfNativeSnapshotDocument);
struct TopologyState<'a>(&'a HvfNativeSnapshotDocument);
struct TimeState<'a>(&'a HvfNativeSnapshotDocument);

impl Serialize for GlobalState<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0.state {
            HvfNativeSnapshotDocumentState::V1(bundle) => GlobalParts {
                compatibility: bundle.state().compatibility(),
                gic_device: &bundle.state().interrupts().gic_device,
            }
            .serialize(serializer),
            state => platform_v2(state)
                .ok_or_else(|| S::Error::custom("native-v2 inspection platform is missing"))
                .and_then(|platform| {
                    GlobalParts {
                        compatibility: platform.global().compatibility(),
                        gic_device: platform.global().gic_device(),
                    }
                    .serialize(serializer)
                }),
        }
    }
}

struct GlobalParts<'a> {
    compatibility: &'a HvfSnapshotV1CompatibilityState,
    gic_device: &'a HvfGicDeviceState,
}

impl Serialize for GlobalParts<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Global", 2)?;
        state.serialize_field("compatibility", &Compatibility(self.compatibility))?;
        state.serialize_field("gic_device", &gic_device_fingerprint(self.gic_device))?;
        state.end()
    }
}

struct Compatibility<'a>(&'a HvfSnapshotV1CompatibilityState);

impl Serialize for Compatibility<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let compatibility = self.0;
        let identification = compatibility.identification();
        let cache = compatibility.cache_manifest();

        let mut state = serializer.serialize_struct("Compatibility", 6)?;
        state.serialize_field("identification", &Identification(identification))?;
        state.serialize_field(
            "sve_sme_identification",
            &compatibility
                .optional_sve_sme_identification()
                .map(SveSmeIdentification),
        )?;
        state.serialize_field("cache_manifest", &CacheManifest(cache))?;
        state.serialize_field("primary_mpidr", &HexU64(compatibility.primary_mpidr()))?;
        state.serialize_field("gic_metadata", &GicMetadata(compatibility.gic_metadata()))?;
        state.serialize_field("rtc_layout", &RtcLayout(compatibility.rtc_mmio_layout()))?;
        state.end()
    }
}

struct SveSmeIdentification(crate::HvfArm64VcpuSveSmeIdentificationRegisterState);

impl Serialize for SveSmeIdentification {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("SveSmeIdentification", 2)?;
        state.serialize_field("id_aa64zfr0_el1", &HexU64(self.0.id_aa64zfr0_el1()))?;
        state.serialize_field("id_aa64smfr0_el1", &HexU64(self.0.id_aa64smfr0_el1()))?;
        state.end()
    }
}

struct CacheManifest(crate::HvfArm64VcpuCacheManifest);

impl Serialize for CacheManifest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let configuration = self.0.configuration();
        let geometry = self.0.geometry();
        let mut state = serializer.serialize_struct("CacheManifest", 5)?;
        state.serialize_field("ctr_el0", &HexU64(configuration.ctr_el0()))?;
        state.serialize_field("clidr_el1", &HexU64(configuration.clidr_el1()))?;
        state.serialize_field("dczid_el0", &HexU64(configuration.dczid_el0()))?;
        state.serialize_field(
            "data_or_unified_ccsidr_el1",
            &HexU64Slice(geometry.data_or_unified_ccsidr_el1()),
        )?;
        state.serialize_field(
            "instruction_ccsidr_el1",
            &HexU64Slice(geometry.instruction_ccsidr_el1()),
        )?;
        state.end()
    }
}

struct Identification(crate::HvfArm64VcpuIdentificationRegisterState);

impl Serialize for Identification {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("Identification", 11)?;
        state.serialize_field("midr_el1", &HexU64(value.midr_el1()))?;
        state.serialize_field("mpidr_el1", &HexU64(value.mpidr_el1()))?;
        state.serialize_field("id_aa64pfr0_el1", &HexU64(value.id_aa64pfr0_el1()))?;
        state.serialize_field("id_aa64pfr1_el1", &HexU64(value.id_aa64pfr1_el1()))?;
        state.serialize_field("id_aa64dfr0_el1", &HexU64(value.id_aa64dfr0_el1()))?;
        state.serialize_field("id_aa64dfr1_el1", &HexU64(value.id_aa64dfr1_el1()))?;
        state.serialize_field("id_aa64isar0_el1", &HexU64(value.id_aa64isar0_el1()))?;
        state.serialize_field("id_aa64isar1_el1", &HexU64(value.id_aa64isar1_el1()))?;
        state.serialize_field("id_aa64mmfr0_el1", &HexU64(value.id_aa64mmfr0_el1()))?;
        state.serialize_field("id_aa64mmfr1_el1", &HexU64(value.id_aa64mmfr1_el1()))?;
        state.serialize_field("id_aa64mmfr2_el1", &HexU64(value.id_aa64mmfr2_el1()))?;
        state.end()
    }
}

struct GicMetadata(crate::HvfGicMetadata);

impl Serialize for GicMetadata {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut state = serializer.serialize_struct("GicMetadata", 5)?;
        state.serialize_field("distributor", &GicRegion(value.distributor))?;
        state.serialize_field("redistributor", &GicRedistributor(value.redistributor))?;
        state.serialize_field(
            "spi_interrupt_range",
            &GicInterruptRange(value.spi_interrupt_range),
        )?;
        state.serialize_field(
            "timer_interrupts",
            &GicTimerInterrupts(value.timer_interrupts),
        )?;
        state.serialize_field("msi", &value.msi.map(GicMsi))?;
        state.end()
    }
}

struct GicRegion(crate::HvfGicRegion);

impl Serialize for GicRegion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("GicRegion", 2)?;
        state.serialize_field("base", &HexU64(self.0.base))?;
        state.serialize_field("size", &self.0.size)?;
        state.end()
    }
}

struct GicRedistributor(crate::HvfGicRedistributor);

impl Serialize for GicRedistributor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("GicRedistributor", 2)?;
        state.serialize_field("region", &GicRegion(self.0.region))?;
        state.serialize_field(
            "single_redistributor_size",
            &self.0.single_redistributor_size,
        )?;
        state.end()
    }
}

struct GicInterruptRange(crate::HvfGicInterruptRange);

impl Serialize for GicInterruptRange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("GicInterruptRange", 2)?;
        state.serialize_field("base", &self.0.base)?;
        state.serialize_field("count", &self.0.count)?;
        state.end()
    }
}

struct GicTimerInterrupts(crate::HvfGicTimerInterrupts);

impl Serialize for GicTimerInterrupts {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("GicTimerInterrupts", 2)?;
        state.serialize_field("el1_virtual_timer_intid", &self.0.el1_virtual_timer_intid)?;
        state.serialize_field("el1_physical_timer_intid", &self.0.el1_physical_timer_intid)?;
        state.end()
    }
}

struct GicMsi(crate::HvfGicMsiMetadata);

impl Serialize for GicMsi {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("GicMsi", 2)?;
        state.serialize_field("region", &GicRegion(self.0.region))?;
        state.serialize_field(
            "interrupt_range",
            &GicInterruptRange(self.0.interrupt_range),
        )?;
        state.end()
    }
}

struct RtcLayout(bangbang_runtime::rtc::RtcMmioLayout);

impl Serialize for RtcLayout {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("RtcLayout", 2)?;
        state.serialize_field("base", &HexU64(self.0.base().raw_value()))?;
        state.serialize_field("region_id", &self.0.region_id().raw_value())?;
        state.end()
    }
}

impl Serialize for TopologyState<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0.state {
            HvfNativeSnapshotDocumentState::V1(_) => serializer.serialize_none(),
            state => platform_v2(state)
                .ok_or_else(|| S::Error::custom("native-v2 inspection platform is missing"))
                .and_then(|platform| PausedTopology(platform).serialize(serializer)),
        }
    }
}

struct PausedTopology<'a>(&'a HvfSnapshotV2PlatformState);

impl Serialize for PausedTopology<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let topology = self.0.topology();
        let mut state = serializer.serialize_struct("PausedTopology", 3)?;
        state.serialize_field("virtual_timer_intid", &topology.virtual_timer_intid())?;
        state.serialize_field("member_count", &topology.members().len())?;
        state.serialize_field("members", &TopologyMembers(topology.members()))?;
        state.end()
    }
}

struct TopologyMembers<'a>(&'a [crate::HvfArm64StablePausedTopologyMember]);

impl Serialize for TopologyMembers<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for member in self.0 {
            sequence.serialize_element(&TopologyMember(member))?;
        }
        sequence.end()
    }
}

struct TopologyMember<'a>(&'a crate::HvfArm64StablePausedTopologyMember);

impl Serialize for TopologyMember<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use crate::HvfArm64StableVcpuDisposition;

        let member = self.0;
        let mut state = serializer.serialize_struct("TopologyMember", 4)?;
        state.serialize_field("index", &member.index())?;
        state.serialize_field("mpidr", &HexU64(member.mpidr()))?;
        match member.disposition() {
            HvfArm64StableVcpuDisposition::Runnable => {
                state.serialize_field("disposition", "runnable")?;
                state.serialize_field("suspend", &Option::<()>::None)?;
            }
            HvfArm64StableVcpuDisposition::Offline => {
                state.serialize_field("disposition", "offline")?;
                state.serialize_field("suspend", &Option::<()>::None)?;
            }
            HvfArm64StableVcpuDisposition::Suspended(suspend) => {
                state.serialize_field("disposition", "suspended")?;
                state.serialize_field("suspend", &Some(Suspend(suspend)))?;
            }
        }
        state.end()
    }
}

struct Suspend<'a>(&'a crate::HvfArm64StableCpuSuspendState);

impl Serialize for Suspend<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use crate::HvfArm64CpuSuspendConvention;

        let arguments = self.0.arguments();
        let mut state = serializer.serialize_struct("CpuSuspend", 3)?;
        state.serialize_field(
            "convention",
            match self.0.convention() {
                HvfArm64CpuSuspendConvention::Call32 => "call32",
                HvfArm64CpuSuspendConvention::Call64 => "call64",
            },
        )?;
        state.serialize_field("arguments", &HexU64Slice(&arguments))?;
        state.serialize_field("return_pc", &HexU64(self.0.return_pc()))?;
        state.end()
    }
}

impl Serialize for TimeState<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0.state {
            HvfNativeSnapshotDocumentState::V1(bundle) => {
                V1Time(bundle.state().interrupts()).serialize(serializer)
            }
            state => platform_v2(state)
                .ok_or_else(|| S::Error::custom("native-v2 inspection platform is missing"))
                .and_then(|platform| V2Time(platform).serialize(serializer)),
        }
    }
}

struct V1Time<'a>(&'a HvfSnapshotV1InterruptState);

impl Serialize for V1Time<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("NativeV1Time", 2)?;
        state.serialize_field("vcpu_timer", &Timer(self.0.timer))?;
        state.serialize_field("restore_policy", "native-v1-retained")?;
        state.end()
    }
}

struct V2Time<'a>(&'a HvfSnapshotV2PlatformState);

impl Serialize for V2Time<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let time = self.0.time();
        let mut state = serializer.serialize_struct("NativeV2Time", 9)?;
        state.serialize_field("rtc_layout", &RtcLayout(time.rtc_layout()))?;
        state.serialize_field("rtc_restore_policy", &RtcPolicy(time.rtc_restore_policy()))?;
        state.serialize_field("vmgenid", &PlatformDeviceMetadata(time.vmgenid()))?;
        state.serialize_field(
            "vmgenid_restore_policy",
            &VmGenIdPolicy(time.vmgenid_restore_policy()),
        )?;
        state.serialize_field("vmclock", &PlatformDeviceMetadata(time.vmclock()))?;
        state.serialize_field("vmclock_abi", &VmClockAbiFingerprint(time.vmclock_abi()))?;
        state.serialize_field(
            "vmclock_restore_policy",
            &VmClockPolicy(time.vmclock_restore_policy()),
        )?;
        state.serialize_field("pvtime_vcpus", &PvTimeVcpus(time.pvtime_vcpus()))?;
        state.serialize_field(
            "pvtime_restore_policy",
            &PvTimePolicy(time.pvtime_restore_policy()),
        )?;
        state.end()
    }
}

struct PlatformDeviceMetadata(bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata);

impl Serialize for PlatformDeviceMetadata {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let metadata = self.0;
        let mut state = serializer.serialize_struct("PlatformDeviceMetadata", 3)?;
        state.serialize_field("range", &GuestRange(metadata.range()))?;
        state.serialize_field("fdt_region", &FdtRegion(metadata.fdt_region()))?;
        state.serialize_field("interrupt_line", &metadata.interrupt_line().raw_value())?;
        state.end()
    }
}

pub(super) struct GuestRange(pub(super) GuestMemoryRange);

impl Serialize for GuestRange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("GuestRange", 2)?;
        state.serialize_field("start", &HexU64(self.0.start().raw_value()))?;
        state.serialize_field("size", &self.0.size())?;
        state.end()
    }
}

struct FdtRegion(bangbang_runtime::fdt::Arm64FdtRegion);

impl Serialize for FdtRegion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("FdtRegion", 2)?;
        state.serialize_field("base", &HexU64(self.0.base))?;
        state.serialize_field("size", &self.0.size)?;
        state.end()
    }
}

struct VmClockAbiFingerprint(bangbang_runtime::vmclock::VmClockAbi);

impl Serialize for VmClockAbiFingerprint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        confidential_bytes("time.vmclock-abi", &self.0.to_bytes()).serialize(serializer)
    }
}

struct PvTimeVcpus<'a>(&'a [HvfSnapshotV2PvTimeVcpuState]);

impl Serialize for PvTimeVcpus<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for vcpu in self.0 {
            sequence.serialize_element(&PvTimeVcpu(vcpu))?;
        }
        sequence.end()
    }
}

struct PvTimeVcpu<'a>(&'a HvfSnapshotV2PvTimeVcpuState);

impl Serialize for PvTimeVcpu<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("PvTimeVcpu", 3)?;
        state.serialize_field("index", &self.0.index())?;
        state.serialize_field("record_ipa", &HexU64(self.0.record_ipa().raw_value()))?;
        state.serialize_field("stolen_time_ns", &self.0.stolen_time_ns())?;
        state.end()
    }
}

struct RtcPolicy(HvfSnapshotV2RtcRestorePolicy);
struct PvTimePolicy(HvfSnapshotV2PvTimeRestorePolicy);
struct VmGenIdPolicy(HvfSnapshotV2VmGenIdRestorePolicy);
struct VmClockPolicy(HvfSnapshotV2VmClockRestorePolicy);

macro_rules! policy_serialize {
    ($wrapper:ident, $type:ty, { $($variant:path => $token:literal),+ $(,)? }) => {
        impl Serialize for $wrapper {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                let token = match self.0 {
                    $($variant => $token),+
                };
                serializer.serialize_str(token)
            }
        }
    };
}

policy_serialize!(RtcPolicy, HvfSnapshotV2RtcRestorePolicy, {
    HvfSnapshotV2RtcRestorePolicy::DestinationSystemTimeReset => "destination-system-time-reset"
});
policy_serialize!(PvTimePolicy, HvfSnapshotV2PvTimeRestorePolicy, {
    HvfSnapshotV2PvTimeRestorePolicy::PreserveCumulativeExcludeDowntime => "preserve-cumulative-exclude-downtime"
});
policy_serialize!(VmGenIdPolicy, HvfSnapshotV2VmGenIdRestorePolicy, {
    HvfSnapshotV2VmGenIdRestorePolicy::RegenerateAndNotify => "regenerate-and-notify"
});
policy_serialize!(VmClockPolicy, HvfSnapshotV2VmClockRestorePolicy, {
    HvfSnapshotV2VmClockRestorePolicy::IncrementAndNotify => "increment-and-notify"
});
