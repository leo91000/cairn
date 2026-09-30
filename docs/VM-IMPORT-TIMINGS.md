# VM import timings

VM imports emit structured records under the existing `leo_performance` target.
They contain random transfer IDs, VM IDs, fixed phase names and aggregate byte
counts; they do not include paths, archive contents or credentials.

The host's `vm_import` record links its transfer `id` to `vm_id` and a fixed
`import_kind` (`entrypoint`, `home`, `chat_inbox` or `other`). Find that VM's ID in
the run's `{attempt}.vm.json` record. Project imports use `vm_project_import`
with `import_kind="project"` and the same identifiers.
The optional `traceId` in import requests links host and guest records by `id`.

| Operation | What its phases measure |
| --- | --- |
| `guest_prepare` | Keeps `current_entrypoint` separate from `run_connect` and `run_request`. |
| `vm_import` / `vm_project_import` | Guest status, connection, request, host `tar` startup, archive streaming, end marker, host `tar` exit and guest acknowledgement. Project imports also measure the ready/reused reply. |
| `guest_import` / `guest_project_import` | Target preparation, guest `tar` startup, receiving the archive into `tar`, `tar` exit, ownership, permissions where applicable and acknowledgement. Project imports also measure reopening, publication and `sync`. |
| `vm_archive_stream` | Total archive `bytes`, `chunks`, cumulative `read_ms` and `write_ms`, labelled `side="host"` or `side="guest"`. |

Host stream reads wait on the archive-producing process; host writes wait on the
VM transport. Guest reads wait on that transport (and include Base64 decoding
for legacy JSON transfers); guest writes wait on the archive extractor's stdin.
These are elapsed waits including scheduling, not exclusive CPU measurements.
Host and guest work overlaps, so do not add their durations together. There is
one aggregate stream record per side, rather than one log per chunk.

Phase start records show the last entered step while an operation is blocked.
Cancellation or failure drops the timer and records `event="incomplete"` with
the current phase and elapsed time. Completed stream totals are only available
when streaming reaches its end.

Host timings are in the runner/controller logs; guest timings are in the VM's
`{vm-id}.boot.log` in runner state. Old guest images accept the optional trace
field but do not emit guest timings. The current controller still supplies host
timings for those retained images. New guest code also accepts requests without
a valid UUID trace ID, recording them as `id="legacy"`. No startup timeouts, archive
encoding, import contents or transfer ordering are changed.
