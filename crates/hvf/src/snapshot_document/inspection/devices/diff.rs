use bangbang_runtime::snapshot_diff_v2_13::{
    SnapshotV2DiffBase, SnapshotV2DiffDataExtent, SnapshotV2DiffLayerBinding,
};
use serde::Serialize;
use serde::ser::{SerializeSeq, SerializeStruct};

use super::super::common::{GuestRange, V2Memory, Version};
use super::super::fingerprint::FingerprintBuilder;

pub(super) struct DiffLayer<'a>(pub(super) &'a SnapshotV2DiffLayerBinding);

impl Serialize for DiffLayer<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = self.0;
        let mut integrity = FingerprintBuilder::new("diff.layer.metadata-integrity");
        integrity.u64(value.metadata_checksum());

        let mut state = serializer.serialize_struct("DiffLayer", 11)?;
        state.serialize_field("compatibility", "v2.13")?;
        state.serialize_field("version", &Version(value.version()))?;
        state.serialize_field("base", &DiffBase(value.base()))?;
        state.serialize_field("result", &V2Memory(value.result()))?;
        state.serialize_field("extent_count", &value.data_extents().len())?;
        state.serialize_field("data_extents", &DiffExtents(value.data_extents()))?;
        state.serialize_field("metadata_length", &value.metadata_length())?;
        state.serialize_field("data_offset", &value.data_offset())?;
        state.serialize_field("file_length", &value.file_length())?;
        state.serialize_field("metadata_integrity", &integrity.finish())?;
        state.serialize_field(
            "relationship",
            &DiffRelationship {
                base_is_image: value.base().binding().is_some(),
            },
        )?;
        state.end()
    }
}

struct DiffBase<'a>(&'a SnapshotV2DiffBase);

impl Serialize for DiffBase<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("DiffBase", 2)?;
        match self.0 {
            SnapshotV2DiffBase::Zero => {
                state.serialize_field("kind", "zero")?;
                state.serialize_field("binding", &Option::<u8>::None)?;
            }
            SnapshotV2DiffBase::Image(binding) => {
                state.serialize_field("kind", "image")?;
                state.serialize_field("binding", &V2Memory(binding))?;
            }
        }
        state.end()
    }
}

struct DiffExtents<'a>(&'a [SnapshotV2DiffDataExtent]);

impl Serialize for DiffExtents<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for extent in self.0 {
            sequence.serialize_element(&DiffExtent(*extent))?;
        }
        sequence.end()
    }
}

struct DiffExtent(SnapshotV2DiffDataExtent);

impl Serialize for DiffExtent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("DiffExtent", 2)?;
        state.serialize_field("range", &GuestRange(self.0.range()))?;
        state.serialize_field("file_offset", &self.0.file_offset())?;
        state.end()
    }
}

struct DiffRelationship {
    base_is_image: bool,
}

impl Serialize for DiffRelationship {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("DiffRelationship", 4)?;
        state.serialize_field("base_is_image", &self.base_is_image)?;
        state.serialize_field("base_and_result_are_distinct", &true)?;
        state.serialize_field("result_matches_vm_memory", &true)?;
        state.serialize_field("omitted_bytes_inherit_base", &true)?;
        state.end()
    }
}
