# Test fixtures

- `dm-era-sealed.meta.gz`: real dm-era metadata (4 MiB, gzip) from a Firecracker guest
  running the Leo kernel. A 64 MiB disk with 4 MiB blocks was written at blocks 2, 5
  and 6 during era 1; the guest then archived the era with `checkpoint` (era 2) and
  rebooted abruptly. `era_invalidate --written-since 1` lists blocks 2, 5 and 6.
