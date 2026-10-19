# F22: cancellation of absent conversation wakeups

Reference: main `7a554da12c72ba4f8722f6216fe889e9a093fc6e` after F3 (#317).

`chat_tick` cancels a wakeup before returning for an attention-needing paused chat. Previously that cancellation always queued one writer job, even if there was no wakeup. `cancel` already avoided `put` when no wakeup existed; this observation is not evidence of a disk write per tick or SQLite saturation.

The cancellation now reads the current chat through a reader before entering the writer. It does not trust the earlier scheduler snapshot. A live wakeup still goes through `cancel`, which re-reads the record on the writer and preserves unrelated fields. A wakeup committed after the read remains visible for cancellation on a later tick; no read/modify/write of an old snapshot is added. Failure/pause ordering and F3 launch/queue checks are unchanged.

The writer-blocking regression uses synthetic failed and cancelled paused chats. The actor is occupied by a controlled no-op without a SQLite transaction or reader lock. Ten candidate ticks must finish before the writer is released. The one-second bound detects waiting on that writer, not an application latency promise. A second case commits a wakeup after the idle read and verifies cancellation on the next tick with a concurrent title edit preserved. Existing `chat_sleep` and `wait_for` cases exercise real wakes, failure/manual resume, successful pause, user input and launch rechecks.

## Reproduction

Use pinned Rust 1.97.1, pnpm 12.3.4 and normal test profile. With a persistent, owned test directory whose real filesystem has more than the 10 GiB reserve plus binary/build margin:

```sh
CARGO_BUILD_JOBS=2 TMPDIR="$PWD/../f22-tmp" mise exec -- pnpm test:backend \
  -p cairn-installation --test store_notifications \
  measure_failed_paused_scheduler -- --ignored --exact --nocapture --test-threads=1
```

The same harness runs on the reference and candidate. Each of failed/cancelled uses one synthetic paused chat without a wakeup. Fifty iterations measure a warm read, first read through a newly opened Store, unchanged chat write and scheduler tick. Fresh connection reads exclude opening Store; process user+system CPU includes connection/thread creation. These are warm OS-page-cache local samples, not cold-host reads or production network latency. Setup/compilation are excluded. The explicit unchanged writes belong to the measurement harness, not the scheduler's no-op cancellation path.

## Measurements

Local sample, 2026-10-19: Linux 6.12.109 x86-64 KVM execution VM, 7 Xeon vCPUs, 19 GiB RAM, overlay filesystem, Rust 1.97.1 normal unoptimized test profile. No other test/build ran during either measurement. No exclusive host reservation. Host/cache conditions are not controlled between samples; source compilation preceded each setting. Raw host/load records accompany the checkpoint.

| Status / metric | Reference | Candidate |
| --- | ---: | ---: |
| failed: Encoded chat bytes | 84 | 86 |
| failed: Warm read median (ms) | 0.888865 | 0.620262 |
| failed: Fresh-connection read median (ms) | 8.727785 | 3.967450 |
| failed: Unchanged write median (ms) | 53.874459 | 28.627145 |
| failed: Tick median (ms) | 4.430721 | 2.916151 |
| failed: Workload process CPU (s) | 2.811138 | 1.315802 |
| cancelled: Encoded chat bytes | 86 | 84 |
| cancelled: Warm read median (ms) | 0.610660 | 0.476220 |
| cancelled: Fresh-connection read median (ms) | 5.719443 | 3.669498 |
| cancelled: Unchanged write median (ms) | 27.817733 | 35.963099 |
| cancelled: Tick median (ms) | 4.728977 | 3.118599 |
| cancelled: Workload process CPU (s) | 2.066147 | 1.845002 |

The common chat payload before the generated slug is 55 encoded bytes in both settings. Full record sizes vary because each fresh Store generates a different readable slug; this is fixture metadata variation, not an encoding change. The measured timing/CPU differences also include cache/filesystem variation and do not isolate a speedup. The unchanged-write samples measure the explicit harness write, not a scheduler disk write.

Writer behavior is verified separately: on the reference the first failed paused tick waits for the held writer; on the candidate ten ticks for each of failed/cancelled finish before releasing it. The control-flow writer-call count for an attention-needing paused tick without a wakeup changes from one to zero; the fresh read replaces that dispatch. These observations do not measure physical disk writes.

Validation: all 10 chat_sleep cases and all 5 active store_notifications cases pass. The initial wait_for run has 31 passes, one ignored measurement and one fixture-probe-start timeout before its concurrency assertions; the single isolated follow-up passes with the same assertions and deadlines. Both results are retained in the checkpoint; the isolated pass does not establish the cause of the initial failure.

## Contracts and limits

No persisted schema, record, encoding, epoch or legacy tree changes. Epoch 5 and legacy tree `a6aae371f1d76cceb2f510180531f0ef17ec3e60` are preserved. No product UI behavior/presentation changes; no web/Android UI child is needed. This is F22 only, not the C3 redesign.

This experiment establishes actor-call behavior and reports local timings; it supplies no production latency guarantee, disk-write count or SQLite saturation evidence. Raw passing/failing logs and sample host/load conditions accompany the PR checkpoint.
