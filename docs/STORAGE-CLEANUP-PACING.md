# Shared-object cleanup pacing

The collector previously completed at most one 32-object batch every five seconds:
6.4 objects per second even when storage was healthy. A production read-only
sample grew from 45,014 eligible objects to 45,263 while the collector logged no
retry errors; its oldest eligible object was already almost two hours old. That
backlog prevents freed conversation storage from catching up with active writes.

Continue immediately after a full successful batch. Keep the 32-object database
claim, four simultaneous remote requests, five-minute reuse grace, durable retry
claims and shared-reference checks. An incomplete or failed batch starts a fresh
five-second idle delay after it finishes. Cancellation takes priority between
batches. Provider latency still limits throughput; this does not promise a given
production reclamation rate or change Garage's own physical block collection.

The virtual-time regression fails with the original cadence. It covers multiple
full batches, a six-second incomplete batch, a six-second failed batch, idle delay
and shutdown during a continuously full queue. Existing ownership, reuse and
retry tests cover the unchanged collection operation.

## Payload measurement reference

The scheduling patch changes no payload format, read, upload, or codec code. The
following reference records the unchanged payload path at `450ce531` using the
existing `block_codec_performance` corpus and loopback S3 fixture. It does not run
the cleanup worker and therefore does not measure I/O contention during cleanup.
These are local debug-build measurements, not production network latency or
release-build performance. Repeat after deployment using live storage metrics to
assess contention separately.

Host: Linux, Intel Core i9-14900K, 32 logical CPUs, 101,005,701,120 bytes RAM.
Seven native conversations and other builds were active; the host was not idle.
One block is 4,194,304 plaintext bytes. Each case has two warmups and 16 measured
samples, alternating v1 reference and current v2. Values below are medians in
milliseconds; warm reads are cached 4 KiB reads. CPU is process user+system time
for the read/write operation, not whole-host utilization. Moto 5.2.3 uses only
loopback and synthetic credentials.

| Corpus | Format | Encoded bytes | Write ms | Cold read ms | Warm 4 KiB ms | Write CPU ms | Read CPU ms |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sparse | v1 reference | 4194340 | 525.595 | 445.390 | 0.001295 | 507.688 | 429.549 |
| sparse | current v2 | 192 | 7.403 | 11.805 | 0.001265 | 4.622 | 10.111 |
| source | v1 reference | 4194340 | 541.198 | 451.200 | 0.001386 | 527.024 | 444.358 |
| source | current v2 | 3290 | 8.107 | 13.553 | 0.002358 | 5.169 | 11.419 |
| mixed | v1 reference | 4194340 | 561.771 | 486.004 | 0.001246 | 539.868 | 462.140 |
| mixed | current v2 | 2099126 | 282.769 | 258.036 | 0.001232 | 265.649 | 237.940 |
| incompressible | v1 reference | 4194340 | 554.728 | 489.679 | 0.004807 | 539.620 | 481.550 |
| incompressible | current v2 | 4194345 | 588.531 | 472.439 | 0.004717 | 573.277 | 468.189 |

Reproduce the corpus with the standard release launcher:

```sh
uv run --with 'moto[server]==5.2.3' python scripts/tests/node_s3_test.py --compression-benchmark
```

For the exact debug profile above, invoke the same launcher with this temporary
wrapper (its only change removes `--release` from the backend test command):

```python
import runpy
import subprocess
import sys

original = subprocess.run

def run(arguments, *args, **kwargs):
    if isinstance(arguments, list) and 'block_codec_performance' in arguments:
        arguments = [argument for argument in arguments if argument != '--release']
    return original(arguments, *args, **kwargs)

subprocess.run = run
sys.argv = ['scripts/tests/node_s3_test.py', '--compression-benchmark']
runpy.run_path(sys.argv[0], run_name='__main__')
```

Use the launcher's isolated S3 credentials and the same Cargo target/profile for
comparisons. Encoded sizes are deterministic for each format/corpus; timings vary
with host load. Registered object sizes in SQLite are logical bytes and must not
be reported as physical filesystem bytes reclaimed.
