# Native VM disk capability (layout 27 → 28)

Stop the installation and preserve an offline database backup before upgrading.
Startup adds `nodes.vm_disks` transactionally, defaults existing nodes to false,
and advances `user_version` to 28. A fresh matching controller report enables the
capability only on a host that supports the required namespace mapping.

Node identities, grants, conversations, checkpoints and storage points remain in
place. VM-created disks move between VM and native execution without file
conversion; native-created disks retain the existing conversion requirements.

An older manager rejects layout 28. Restore the pre-upgrade database and
compatible environment points for a historical rollback, or use this version to
recover and move the current environments. Do not simply decrement `user_version`:
new typed move state and VM disks mounted natively are not understood by old code.
The user accepted this incompatible rollback behavior.

Validate an upgrade/reopen with SQLite integrity and foreign-key checks, including
historical startup chains that already create the current inventory schema.
