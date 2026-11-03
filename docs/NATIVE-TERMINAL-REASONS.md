# Native terminal reasons outside the ordinary output journal

Date: 2026-11-03.

## Contract

Native output keeps its cumulative 64 MiB encoded-log limit and controller exit
code 70. When the log is already over budget, the production append guard rejects
the next frame. Its completion diagnostic also targets that full log and may not
be saved. The attempt therefore needs a bounded, controller-owned way to report
the guard's fixed cause.

The guard records the closed reason `native_output_limit` in root-private,
per-attempt controller state, outside the child's filesystem and ordinary output
journal. The record contains no child text, exception, path, credential or
arbitrary payload. Diagnostic appends cannot originate the reason. If the metadata
write fails, the guard still returns its original output-budget error. Repeated
recording of the same reason is idempotent.

The state directory is mode 0700 and `atomic_write` creates the metadata file at
mode 0600, syncing the file and parent directory. The reader uses `O_NOFOLLOW`
and `O_NONBLOCK`, accepts only regular files up to 128 bytes, reads at most 129
bytes, and rejects unknown fields and reason values. The output log and its
64 MiB guard do not change.

After existing physical cleanup, the native controller attaches the reason only
to status 70 with `PhysicalFinalized: true` and no `FinalizationFailed` marker.
The authenticated runner client accepts it only with that status and physical
finalization evidence. The worker persists one fixed error event and typed
checkpoint cause only for the matching native attempt. Child output cannot set
that checkpoint evidence. A prior provider or controller error remains the
displayed error; cancellation, provider completion and controller retry policy
retain their existing precedence. The failure event and checkpoint record are
committed together. Attempt cleanup removes the metadata alongside the log and
lease files.

This change adds no output rotation, acknowledgement, sliding window, quota
increase, timeout change or retry-policy change. A finalization failure keeps
its existing result and cannot carry the output-limit reason.

## Validation

The source regression test drives the production output guard with a sparse
exhausted journal, checks the exact guard error, follows the completion-error
path, and carries the result through authenticated `/wait` parsing. Worker tests
cover event/checkpoint persistence, same-run and same-attempt binding, duplicate
observation, provider and earlier-error precedence, completion, cancellation,
flush failure, legacy results, VM results and stale attempts. Metadata tests cover
the private directory/file modes, malformed and oversized records, symlinks,
forged child output, metadata-write failure, and idempotent reading after opening
another controller handle.

The regular backend job runs the ignored metadata measurement below as a separate
step, after the bootstrap observer. The source tests, hosted exact-head validation,
native physical-finalization qualification and current-head measurement remain
pending until their respective checks complete. Do not treat formatting or static
inspection as a substitute for those checks.

### Historical regression evidence

On the earlier implementation branch, hosted run
[41266578064](https://github.com/leo91000/cairn/actions/runs/41266578064) ran the
actual output guard, completion-error helper, authenticated wait/client and Store
reconciliation. It failed because the saved error was empty after the status-70
assertion passed. The historical RED test was authored against baseline
`93b0ab0012a5947e2a2197219111bf8029c3b5dd`; it establishes the original failure
only. It does not validate this port on current base
`4e5667a6a9b238095db1f8d13752c3cef4855018`.

### Reproducible metadata measurement

The reference base has no terminal metadata channel, so its metadata byte count
is zero by definition and its metadata read/write latency is **inapplicable**,
not a measured zero. The implementation encodes the fixed metadata record as 32
bytes. A healthy finalized status-70 result grows from 42 to 81 encoded bytes
when it carries the reason. The ordinary journal bytes remain unchanged.

The backend job runs this measurement alone with the credential-isolating
launcher. The same command can be used to reproduce it on an exact candidate:

```sh
node scripts/test-backend.mjs -p cairn-installation --lib \
  native::service::terminal_tests::terminal_reason_metadata_measurement \
  -- --exact --ignored --nocapture
```

It records 16 new terminal metadata writes, one first read and 64 repeated reads
per record. Writes use the production atomic metadata helper, including file and
parent-directory sync. The output includes per-operation wall times, process CPU,
OS, architecture, kernel, CPU model, logical CPU count, workload and cache
conditions. The first read follows the syncing write and is not an OS-cold read;
filesystem and provider caches are uncontrolled. Record the candidate SHA, CI
runner and build profile with the emitted JSON. No current-base timing or CPU
result is claimed until that hosted command runs. These task-owned temporary-file
measurements are separate from production network latency and native workload
finalization.
