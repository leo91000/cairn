//! Filesystem boundaries are part of the immutable disk manifest, not node state.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;

const ALIGNMENT: u64 = super::super::nodes::snapshots::BLOCK;
const MIN_SYSTEM: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Layout {
    #[default]
    #[serde(rename = "flat-ext4-v1")]
    Flat,
    #[serde(rename = "paired-ext4-v1")]
    Paired {
        #[serde(rename = "systemBytes")]
        system_bytes: u64,
    },
}

impl Layout {
    pub fn from_manifest(manifest: &Value) -> io::Result<Self> {
        let Some(encoded) = manifest.get("layout") else {
            if manifest["version"] == 2 {
                return Err(io::Error::other("Paired manifest is missing its layout"));
            }
            return Ok(Self::Flat);
        };
        let layout: Self = serde_json::from_value(encoded.clone()).map_err(io::Error::other)?;
        let size = manifest["size"]
            .as_u64()
            .ok_or_else(|| io::Error::other("Missing disk size"))?;
        layout.validate(size)?;
        Ok(layout)
    }

    pub fn paired(size: u64) -> io::Result<Self> {
        let system_bytes = size / 4 / ALIGNMENT * ALIGNMENT;
        let layout = Self::Paired { system_bytes };
        layout.validate(size)?;
        Ok(layout)
    }

    pub fn validate(self, size: u64) -> io::Result<()> {
        if let Self::Paired { system_bytes } = self
            && (system_bytes < MIN_SYSTEM
                || !system_bytes.is_multiple_of(ALIGNMENT)
                || !size.is_multiple_of(512)
                || system_bytes
                    .checked_add(MIN_SYSTEM)
                    .is_none_or(|end| end > size))
        {
            return Err(io::Error::other("Invalid paired filesystem boundaries"));
        }
        Ok(())
    }

    pub fn grown(self, size: u64) -> io::Result<Self> {
        match self {
            Self::Flat => Ok(Self::Flat),
            Self::Paired { system_bytes } => {
                let next = Self::Paired {
                    system_bytes: system_bytes.max(size / 4 / ALIGNMENT * ALIGNMENT),
                };
                next.validate(size)?;
                Ok(next)
            }
        }
    }

    pub fn system_bytes(self, total: u64) -> u64 {
        match self {
            Self::Flat => total,
            Self::Paired { system_bytes } => system_bytes,
        }
    }

    pub fn store(self, manifest: &mut Value) -> io::Result<()> {
        self.validate(manifest["size"].as_u64().unwrap_or(0))?;
        let object = manifest
            .as_object_mut()
            .ok_or_else(|| io::Error::other("Invalid disk manifest"))?;
        object.insert("version".into(), self.manifest_version().into());
        match self {
            Self::Flat => {
                object.remove("layout");
            }
            Self::Paired { .. } => {
                object.insert("layout".into(), serde_json::to_value(self)?);
            }
        }
        Ok(())
    }

    /// Older readers reject version 2 instead of silently booting only the OS
    /// filesystem and losing access to its separately addressed workspace.
    pub fn manifest_version(self) -> u64 {
        match self {
            Self::Flat => 1,
            Self::Paired { .. } => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paired_boundaries_are_versioned_bounded_and_share_the_disk_budget() {
        let size = 512 * 1024 * 1024;
        let layout = Layout::paired(size).unwrap();
        assert_eq!(layout.system_bytes(size), size / 4);
        let mut manifest = json!({ "size": size });
        layout.store(&mut manifest).unwrap();
        assert_eq!(manifest["version"], 2);
        assert_eq!(Layout::from_manifest(&manifest).unwrap(), layout);
        assert!(Layout::paired(128 * 1024 * 1024).is_err());
        for invalid in [
            json!(null),
            json!({ "kind": "paired-ext4-v2", "systemBytes": size / 4 }),
            json!({ "kind": "paired-ext4-v1", "systemBytes": size }),
            json!({ "kind": "paired-ext4-v1", "systemBytes": u64::MAX }),
            json!({ "kind": "paired-ext4-v1", "systemBytes": size / 4 + 512 }),
            json!({ "kind": "paired-ext4-v1", "systemBytes": size / 4, "offset": 0 }),
        ] {
            manifest["layout"] = invalid;
            assert!(Layout::from_manifest(&manifest).is_err());
        }
        Layout::Flat.store(&mut manifest).unwrap();
        assert_eq!(Layout::from_manifest(&manifest).unwrap(), Layout::Flat);
        assert_eq!(manifest["version"], 1);
        manifest["version"] = 2.into();
        assert!(Layout::from_manifest(&manifest).is_err());
    }

    #[test]
    fn growth_never_shrinks_an_existing_filesystem_or_rejects_a_mib_budget() {
        let original = Layout::Paired {
            system_bytes: 256 * 1024 * 1024,
        };
        let total = 512 * 1024 * 1024;
        original.validate(total).unwrap();
        let next = original.grown(total + 1024 * 1024).unwrap();
        assert_eq!(next.system_bytes(total + 1024 * 1024), 256 * 1024 * 1024);
        assert_eq!(
            Layout::paired(257 * 1024 * 1024)
                .unwrap()
                .system_bytes(257 * 1024 * 1024),
            64 * 1024 * 1024
        );
    }
}
