# Attribute native filesystem holders before physical retirement

A successful host unmount does not remove private copies of that filesystem from
other mount namespaces. Native close therefore retains the block attachment's
device identity and inventories live task namespaces, descriptors, mappings,
working directories and roots before it permits retirement. A shared host mount
entry alone is not a process reference and never authorizes killing its observer.
After the expected host mount disappears, any remaining host mount is a refusal,
not permission to kill every observer. Namespace descriptors and persistent nsfs
mounts are inspected on separate threads, including namespaces without member tasks.

Only the current attempt's unit and canonical legacy Docker scopes can be stopped
automatically. Process identities are pinned with pidfds and revalidated before
signalling. Recovery includes retained block attachments whose host mount already
disappeared. Its leases are retained by the kernel operation even if startup is
cancelled. Unrelated holders, incomplete inspection or remaining references keep
the disk owned and report the holders. Ordinary unmount is attempted first;
retries never unmount the parent filesystem after the target disappeared. A
successful flush is remembered across those retries for the same reason.

This adds holder recovery to ADR-0085 without adopting legacy Docker stores.
Physical deletion remains separate from logical completion; this change does not
make an uninterruptible kernel operation cancellable. Isolating retirement and
startup cleanup is a subsequent change. No journal or runtime marker changes.

A namespace can cover its root and hide older mounts from a process-relative
mountinfo inventory. After unmount and holder recovery, close also requires the
owned ublk device's ext4 registration to disappear from trusted host sysfs. The
current-boot receipt supplies the device name; missing inventory or any matching
registration refuses retirement. This is evidence against a retained active
superblock, not proof that journal destruction or kernel retirement completed.

After a host reboot, an attachment with exclusively previous-boot receipts cannot
still refer to an old mounted filesystem. If its expected host path has no mount,
recovery removes only that private stale block-node entry, without inspecting or
signalling any process and without controlling a possibly reused device number.
Unknown attachments and current-boot conflicts continue to fail closed.

Raw block-device mappings retain their backing file after close(fd), while maps
reports the device-node filesystem rather than the block rdev. Holder inspection
also resolves file-backed map_files entries; denied inspection fails closed.

Persisted nsfs mount roots carry the kernel namespace type and inode. Ignore
known non-mount namespace types before resolving their paths: a Docker network
namespace can be hidden under a newer private /run and cannot retain a filesystem
mount graph. Unknown types fail closed. Mount namespaces still require an opened
descriptor with the same inode and confirmed namespace type before traversal.
