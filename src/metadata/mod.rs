use core::ops::Range;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::force_de_typed_map::ForceDeTypedMap;

pub mod deserialize;

#[derive(Deserialize, Clone, Debug)]
pub struct MetadataRoot {
    pub id: String,
    pub extent_size: u64,
    pub physical_volumes: BTreeMap<String, PVDesc>,
    pub logical_volumes: BTreeMap<String, LVDesc>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct PVDesc {
    pub id: String,
    pub device: String,
    pub pe_start: u64,
    pub pe_count: u64,
}
#[derive(Deserialize, Clone, Debug)]
pub struct LVDesc {
    pub id: String,
    pub status: Vec<String>,
    pub flags: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Seconds since the epoch, or 0 when the metadata has none. LVM2 leaves
    /// both fields out for an LV that has no creation timestamp.
    #[serde(default)]
    pub creation_time: u64,
    /// Empty when the metadata has none.
    #[serde(default)]
    pub creation_host: String,
    pub segment_count: usize,
    #[serde(flatten)]
    pub segments: ForceDeTypedMap<String, LVSegmentDesc>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct LVSegmentDesc {
    pub start_extent: u64,
    pub extent_count: u64,
    pub r#type: String,
    pub stripe_count: Option<usize>,
    pub stripe_size: Option<usize>,
    /// The PV and starting extent of a one-stripe segment. A segment with
    /// several stripes parses with `None`, so the rest of the VG still opens
    /// and only opening that LV fails.
    #[serde(default, deserialize_with = "one_stripe")]
    pub stripes: Option<(String, u64)>,
    pub raid0_lvs: Option<Vec<String>>,
}
/// `stripes = ["pv0", 0, "pv1", 0]`: one PV name and starting extent per
/// stripe. Keeps the pair only when there is exactly one.
fn one_stripe<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<(String, u64)>, D::Error> {
    struct Stripes;

    impl<'de> Visitor<'de> for Stripes {
        type Value = Option<(String, u64)>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an array of PV names, each followed by its starting extent")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut stripes: Vec<(String, u64)> = Vec::new();
            while let Some(pv) = seq.next_element::<String>()? {
                let extent = seq
                    .next_element::<u64>()?
                    .ok_or_else(|| de::Error::custom("stripe without a starting extent"))?;
                stripes.push((pv, extent));
            }
            Ok(if stripes.len() == 1 {
                stripes.pop()
            } else {
                None
            })
        }
    }

    deserializer.deserialize_seq(Stripes)
}

impl LVSegmentDesc {
    pub fn extents(&self) -> Range<u64> {
        self.start_extent..(self.start_extent + self.extent_count)
    }
}
