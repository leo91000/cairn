#!/usr/bin/env bash
# Run native: container build sandboxes may deny io_uring_setup. No tests skip
# unsupported io_uring; discard qualification must actually exercise the engine.
set -euo pipefail
repository=$(cd -- "$(dirname -- "$0")/.." && pwd)
source_commit=95f868c8e345b1cc8faccd1a3c910b4989dc3f58
source_sha256=27280787407229c412901dfcf74cd378f1336010e71050aba43ace51cd4a0f4f
grep -Fq -- "$source_commit" "$repository/Dockerfile"
grep -Fq -- "$source_sha256" "$repository/Dockerfile"
fixture=$(mktemp -d "${TMPDIR:-/tmp}/cairn-firecracker-discard.XXXXXXXX")
trap 'rm -rf -- "$fixture"' EXIT
curl -fsSL "https://codeload.github.com/firecracker-microvm/firecracker/tar.gz/$source_commit" -o "$fixture/source.tar.gz"
printf '%s  %s\n' "$source_sha256" "$fixture/source.tar.gz" | sha256sum -c -
tar -xzf "$fixture/source.tar.gz" --strip-components=1 -C "$fixture"
rm "$fixture/source.tar.gz"
git -C "$fixture" apply "$repository/deploy/microvm/firecracker-shared-memory.patch"
git -C "$fixture" apply "$repository/deploy/microvm/firecracker-discard.patch"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repository/target/firecracker-discard}"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0
export RUSTC
RUSTC=$(rustup which --toolchain 1.97.0 rustc)
cd "$fixture"
rustup run 1.97.0 cargo test --locked -p vmm --lib discard -- --nocapture
rustup run 1.97.0 cargo test --locked -p vmm --lib test_new_all_features -- --nocapture
