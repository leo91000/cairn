//! Validate the single ext4 filesystem format of conversation disks.
use serde_json::Value;
use std::io;

pub(crate) fn validate(manifest: &Value) -> io::Result<()> {
    if let Some(layout) = manifest.get("layout")
        && *layout != serde_json::json!({ "kind": "flat-ext4-v1" })
    {
        return Err(io::Error::other("Unsupported disk layout"));
    }
    Ok(())
}
