# Workspace transfer: local permission observation

A native-node validation reproduced `0755` becoming `0700` on receive. The same
public send/receive test failed under umask `0077` and passed under `0022`.
The receiver created each file with the announced mode, which the process mask
restricted, then never restored it. It now applies the sanitized mode through
the open file descriptor before synchronization. Exclusive creation, no-follow
flags, path/ancestor checks, size limits and directory ordering remain in place.
No stream frame, stored record, key, encryption envelope or epoch is added.

The regression runs in an isolated POSIX child with umask `0077`, so parallel tests
and the caller retain their masks. Its parent can use `0022` and still reproduce
the defect before the correction. On the corrected code, it also reaches the
existing symlink and incomplete-stream refusal checks.

Reproduce the separate, narrow I/O observation from the repository root:

```sh
python3 scripts/measure-workspace-transfer-mode.py --output docs/performance/WORKSPACE-TRANSFER-MODE.local.json
```

The Python child creates 100 synthetic files under a private temporary directory
on the managed workspace filesystem. Both shapes request `0755` with umask `0077`.
The candidate adds descriptor `fchmod` after writing and before `fsync`. It reads
all files through new descriptors, then performs 1,000 reads through one open raw
file. All contents are checked. The following are observed local values:

| Observation | Creation only | Restore descriptor mode |
| --- | ---: | ---: |
| Payload bytes per file | 4600 | 4600 |
| Total persisted file bytes | 460000 | 460000 |
| Result mode | 0700 | 0755 |
| Write median / p95 ms | 16.5218 / 39.6082 | 16.4643 / 25.1737 |
| New-descriptor read median / p95 ms | 0.006996 / 0.008475 | 0.009426 / 0.010362 |
| Warm read median / p95 ms | 0.001115 / 0.001445 | 0.001505 / 0.001635 |
| Write CPU seconds | 0.024160 | 0.023803 |
| New-descriptor read CPU seconds | 0.000804 | 0.001045 |
| Warm-read CPU seconds | 0.001272 | 0.001785 |

Linux x86_64, Python 3.14.8; host load averages
63.44, 34.40, 20.91, with other host work active.
This is a Python POSIX primitive observation, excluding Rust/Tokio scheduling,
stream encoding, wire bytes, native admission, providers and network/S3 latency.
Persisted payload sizes are measured; the existing stream representation is
unchanged by the descriptor operation. New-descriptor reads do not evict the OS
cache; warm reads retain a descriptor. Reference precedes candidate and CPU
includes Python overhead. No cache, host service or driver was changed. The timing
differences do not establish an intrinsic speedup or regression. Broader node,
worker and production recovery qualification remain separate.
