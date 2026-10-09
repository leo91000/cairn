#!/usr/bin/env bash
# A disposable real Firecracker + jailer check; does not touch controller data.
set -euo pipefail
kernel="${1:?Usage: tests/lazy-disk-smoke.sh /path/to/pinned/vmlinux}"
mode="${2:-all}"
test -f "$kernel"
test -c /dev/kvm
test -c /dev/fuse
command -v busybox >/dev/null
command -v mkfs.ext4 >/dev/null
test -x /usr/local/bin/firecracker
test -x /usr/local/bin/jailer
fixture=$(mktemp -d "${TMPDIR:-/tmp}/cairn-storage-smoke.XXXXXXXX")
cleanup() {
  sudo -n python3 - "$fixture" <<'PY'
import pathlib, shutil, sys
root = pathlib.Path(sys.argv[1])
assert root.name.startswith('cairn-storage-smoke.') and root.parent != root
shutil.rmtree(root)
PY
}
trap cleanup EXIT
mkdir -p "$fixture/rootfs/bin" "$fixture/rootfs/dev" "$fixture/rootfs/proc" "$fixture/rootfs/sys" "$fixture/rootfs/data" "$fixture/datafs"
cp "$(command -v busybox)" "$fixture/rootfs/bin/busybox"
cp "$kernel" "$fixture/vmlinux"
cat > "$fixture/rootfs/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mount -t devtmpfs devtmpfs /dev
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount /dev/vdb /data || exec /bin/busybox poweroff -f
/bin/busybox cat /data/probe
echo storage-after-guest-sync > /data/probe
/bin/busybox sync
echo CAIRN_STORAGE_GUEST_SYNCED
while true; do /bin/busybox sleep 1; done
INIT
chmod +x "$fixture/rootfs/init"
echo storage-before-guest-write > "$fixture/datafs/probe"
dd if=/dev/urandom of="$fixture/datafs/unused" bs=1M count=32 status=none
truncate -s 16M "$fixture/root.ext4"
mkfs.ext4 -q -F -d "$fixture/rootfs" "$fixture/root.ext4"
cat > "$fixture/rootfs/init" <<'INIT'
#!/bin/busybox sh
/bin/busybox mount -t devtmpfs devtmpfs /dev
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
echo CAIRN_STORAGE_GUEST_PREPARED
# A size query observes the device configuration without reading its contents.
# The host replaces the 4 MiB placeholder before any data I/O or filesystem mount.
while [ "$(/bin/busybox blockdev --getsize64 /dev/vdb)" -lt 268435456 ]; do
  /bin/busybox sleep 0.05
done
/bin/busybox mount /dev/vdb /data || exec /bin/busybox poweroff -f
/bin/busybox cat /data/probe
echo storage-after-guest-sync > /data/probe
/bin/busybox sync
echo CAIRN_STORAGE_GUEST_SYNCED
while true; do /bin/busybox sleep 1; done
INIT
truncate -s 16M "$fixture/root-prepared.ext4"
mkfs.ext4 -q -F -d "$fixture/rootfs" "$fixture/root-prepared.ext4"
truncate -s 256M "$fixture/data.ext4"
mkfs.ext4 -q -F -d "$fixture/datafs" "$fixture/data.ext4"
cargo build --locked --example lazy_disk_smoke
if [ "$mode" = all ]; then
  sudo -n target/debug/examples/lazy_disk_smoke "$fixture"
fi
sudo -n target/debug/examples/lazy_disk_smoke "$fixture" --prepared
if [ "$mode" = all ]; then
  sudo -n target/debug/examples/lazy_disk_smoke "$fixture" --cancel
fi
