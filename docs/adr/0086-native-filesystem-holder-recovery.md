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
