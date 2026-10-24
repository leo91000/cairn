# Frozen reply status after native retries

This observation compares the existing private predecessor marker, its earlier
already-ended range, and the same range with the new canonical `reply.status`.
The synthetic status is `failed` in every range case; the old `reply.failed`
boolean is retained. The canonical case adds **18 JSON bytes per marker**, from
114 to 132; 1,000 unchanged keys plus values rise from 209,000 to 227,000 bytes.
No table, key family, index or encryption envelope is added. Other status strings
have different encoded lengths; this sample does not benchmark them.

## Reproduction

```sh
pnpm test:backend -p cairn-installation --test slack \
  checkpoints::measure_ended_reply_range_storage \
  -- --ignored --exact --nocapture --test-threads=1
```

Use pinned pnpm 12.3.4 and Rust 1.97.1. [Raw result](slack-retry-reply-range-storage.json).
One ignored observation ran successfully in isolation (1.656s including launcher;
no concurrent build/test/emulator owned by this task). Each case seeds 1,000
synthetic markers into a temporary Store, reopens it and reads one key six times
in one actor: first connection-cold, then five warm. It writes the same value six
times. OS page cache remains warm; case order is fixed, not randomized. Reads
exclude actor scheduling; writes include it. CPU samples use process CPU.

## Recorded samples

All latency and CPU columns are milliseconds; warm and write columns are medians.

| Shape | Value JSON bytes | 1,000 keys + JSON bytes | Cold wall | Cold CPU | Warm wall | Warm CPU | Write wall | Write CPU |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| unfrozen | 62 | 157000 | 0.554962 | 0.713733 | 0.008458 | 0.009127 | 0.068707 | 0.078313 |
| legacyRange | 114 | 209000 | 0.571152 | 0.834975 | 0.018950 | 0.019944 | 0.034522 | 0.040542 |
| terminalRange | 132 | 227000 | 0.540554 | 0.575611 | 0.019772 | 0.020920 | 0.036852 | 0.042177 |

Host: Linux 7.2.9-arch1-1/glibc2.44, Intel Core i9-14900K, 12 available threads.
The cgroup CPU/memory limit files were unavailable. Captured host I/O pressure
was some 33.27% / full 25.57% over 10s; unrelated host load is uncontrolled.
These short samples establish neither a speedup nor a production latency bound.

The byte count excludes SQLite pages, WAL and indexes. The read/write timing
excludes native terminal-range selection, authorization, provider/agent work,
network, VM startup and physical recovery. In particular it does not compare
Main's retry-aware event scan with the earlier first-terminal SQL query.

The selector is now shared by live replies and marker creation before advancement;
its status is frozen with the selected end so later turns cannot change it. Old
markers lacking canonical status retain their existing boolean interpretation;
they never search a later request's events. This remains inside the unqualified
[epoch 13 / SQLite 12 draft boundary](../SLACK-PLUGIN-COMPATIBILITY.md), with no
automatic migration, release or deployment approval.
