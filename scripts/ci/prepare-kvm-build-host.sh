#!/usr/bin/env bash
set -euo pipefail

test -e /dev/kvm
if ! sudo modprobe ublk_drv; then
  sudo apt-get update -qq
  sudo apt-get install -y "linux-modules-extra-$(uname -r)"
  sudo modprobe ublk_drv
fi
test -c /dev/ublk-control
test "$(cat /proc/sys/kernel/io_uring_disabled)" = 0

# A working L0 device alone does not guarantee nested KVM for our guest.
if grep -qw vmx /proc/cpuinfo; then
  grep -Eq '^(Y|1)$' /sys/module/kvm_intel/parameters/nested
else
  grep -qw svm /proc/cpuinfo
  grep -Eq '^(Y|1)$' /sys/module/kvm_amd/parameters/nested
fi

df -h /
# Hosted runners currently have ~87 GB free. Deleting unused SDKs takes up
# to minutes, so do it only if a smaller disk ever needs the space.
if (( $(df --output=avail -BG / | tail -1 | tr -dc 0-9) < 40 )); then
  sudo rm -rf /usr/share/dotnet /usr/local/lib/android
fi
