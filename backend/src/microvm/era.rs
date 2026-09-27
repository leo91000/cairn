//! Guest-side dm-era write tracking over the data disk.
//!
//! The guest init stacks the `era` target named [`TARGET`] on `/dev/vdb`, with its
//! metadata on [`METADATA`], and mounts ext4 through it. Each block recorded here is a
//! 4 MiB range of the host's `data.ext4`, because the target maps the disk 1:1.
use crate::error::{Error, Result};
use serde_json::{Value, json};
use tokio::process::Command;

pub const TARGET: &str = "data";
pub const METADATA: &str = "/dev/vdc";
/// Where the guest init mounted the data filesystem, seen from the agent's root.
pub const DATA_MOUNT: &str = "/oldroot/run/data";

/// `[begin, end)` block ranges from `era_invalidate` XML, which writes either
/// `<block block="3"/>` or `<range begin="110" end = "115"/>`.
pub fn ranges(xml: &str) -> Option<Vec<[u64; 2]>> {
    // The tag name can also match (`<block block=...`), so try each occurrence.
    fn attribute(line: &str, name: &str) -> Option<u64> {
        line.match_indices(name).find_map(|(at, _)| {
            let rest = line[at + name.len()..].trim_start().strip_prefix('=')?;
            let rest = rest.trim_start().strip_prefix('"')?;
            rest[..rest.find('"')?].parse().ok()
        })
    }
    let mut found = Vec::new();
    for line in xml.lines().map(str::trim) {
        if line.starts_with("<block ") {
            let block = attribute(line, "block")?;
            found.push([block, block + 1]);
        } else if line.starts_with("<range ") {
            let (begin, end) = (attribute(line, "begin")?, attribute(line, "end")?);
            (begin < end).then_some(())?;
            found.push([begin, end]);
        } else if !(line.is_empty() || line == "<blocks>" || line == "</blocks>") {
            return None;
        }
    }
    Some(found)
}

async fn dmsetup(args: &[&str]) -> Result<String> {
    let output = Command::new("dmsetup").args(args).output().await?;
    if !output.status.success() {
        return Err(Error::new(503, "Write tracking is unavailable."));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether the data disk is mounted through the era target.
pub async fn active() -> bool {
    dmsetup(&["status", TARGET])
        .await
        .is_ok_and(|status| status.contains(" era "))
}

/// Freezes the data filesystem for good and archives the current era. Called only
/// on shutdown: nothing may write between the seal and the reboot.
pub async fn seal() -> Result<u64> {
    let frozen = Command::new("fsfreeze")
        .args(["--freeze", DATA_MOUNT])
        .status()
        .await?;
    if !frozen.success() {
        return Err(Error::new(503, "Unable to freeze the data filesystem."));
    }
    dmsetup(&["message", TARGET, "0", "checkpoint"]).await?;
    current_era().await
}

async fn current_era() -> Result<u64> {
    // Status: `<start> <length> era <metadata block> <used>/<total> <current era> <held root>`.
    dmsetup(&["status", TARGET])
        .await?
        .split_whitespace()
        .nth(5)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| Error::new(503, "Unreadable write tracking status."))
}

/// Blocks written since `since` (inclusive), and the era to ask from next time.
/// The caller freezes the filesystem first, so nothing is written meanwhile.
pub async fn written(since: Option<u64>) -> Result<Value> {
    // Archive the current writeset, then read a stable copy of the metadata.
    dmsetup(&["message", TARGET, "0", "checkpoint"]).await?;
    dmsetup(&["message", TARGET, "0", "take_metadata_snap"]).await?;
    let result = async {
        let era = current_era().await?;
        // Table: `<start> <length> era <metadata> <origin> <block sectors>`.
        let table = dmsetup(&["table", TARGET]).await?;
        let sectors = table
            .split_whitespace()
            .nth(5)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| Error::new(503, "Unreadable write tracking table."))?;
        let blocks = match since {
            None => Vec::new(),
            Some(since) => {
                let output = Command::new("era_invalidate")
                    .args([
                        "--metadata-snapshot",
                        "--written-since",
                        &since.to_string(),
                        METADATA,
                    ])
                    .output()
                    .await?;
                if !output.status.success() {
                    return Err(Error::new(503, "Unable to list written blocks."));
                }
                ranges(&String::from_utf8_lossy(&output.stdout))
                    .ok_or_else(|| Error::new(503, "Unreadable written block list."))?
            }
        };
        Ok(json!({"ok":true,"era":era,"blockSize":sectors * 512,"blocks":blocks}))
    }
    .await;
    let _ = dmsetup(&["message", TARGET, "0", "drop_metadata_snap"]).await;
    result
}

#[cfg(test)]
mod tests {
    use super::ranges;

    #[test]
    fn reads_single_blocks_and_ranges_as_era_invalidate_prints_them() {
        // Verbatim from era_invalidate (thin-provisioning-tools) in a Firecracker guest.
        let xml = "<blocks>\n  <block block=\"0\"/>\n  <range begin=\"110\" end = \"115\"/>\n  <block block=\"480\"/>\n</blocks>\n";
        assert_eq!(ranges(xml), Some(vec![[0, 1], [110, 115], [480, 481]]));
        assert_eq!(ranges("<blocks>\n</blocks>\n"), Some(vec![]));
    }

    #[test]
    fn refuses_output_it_does_not_understand() {
        assert_eq!(
            ranges("<blocks>\n  <range begin=\"9\" end=\"9\"/>\n</blocks>"),
            None
        );
        assert_eq!(ranges("<blocks>\n  <superblock/>\n</blocks>"), None);
        assert_eq!(ranges("<blocks>\n  <block block=\"x\"/>\n</blocks>"), None);
    }
}
