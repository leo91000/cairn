# Direct publication scheduling reference

A direct publication transfers immutable four-object batches from the node to
object storage. A serialized manager waits for every member of one batch before
admitting another. One slow member can therefore leave global PUT permits idle
even though later local blocks are ready. The ordering witness in
`nodes::publication::pipeline_tests` holds one real PUT until three others finish
and checks whether a fifth can begin. It releases and drains the fixture before
asserting, including in the failing serialized reference.

The bounded schedule allows two ordinary control batches, each of at most four
objects. It keeps the existing process-global four-PUT budget and deadlines.
The terminal batch waits for every earlier receipt to be validated and settled,
so the final diagnostic summary cannot overtake earlier transfers. There is no
runtime option to raise the window.

Every admission still revalidates the owner/attempt and on-demand read grant and
persists scoped object capabilities before issuing the request. Receipts settle
only their exact publication, object set and plaintext length. Create-only 412
responses require an authenticated digest-verified read of the existing object.
An observed error stops further admission and joins the accepted control tasks.
An HTTP failure can precede a detached node PUT's completion: unsettled durable
grants retain GC tombstones, the node session retains physical ownership, and
superseding capture cannot discard the busy reader. This is an error with retained
work, not successful publication. Successful publication requires the explicit
reader-close acknowledgement before publishing its manifest or committing its
pointer.

Two batches bound produced plaintext to eight blocks (32 MiB) per publication.
The unchanged global PUT permits bound concurrent encoded bodies to four objects
(4 x `MAX_ENCODED_BYTES`), shared by all publications in that node process. These
are payload bounds, not full-process RSS bounds. Reader caches, manifests, HTTP
buffers, codec workspaces and other publications also consume memory.

## Reproduce in isolated CI

Use the pinned Rust toolchain and the safe backend launcher. Measurement mode
runs all-target Clippy and this fixture only, then preserves the log as
`direct-publication-measurement`. Its job is named `publication measurement`,
so its success does not stand for the full backend suite. Run ordinary backend
or complete qualification separately on the same commit. This keeps independent
checks from duplicating the full workspace suite ahead of the benchmark in one
job budget. Existing deadlines and full/default qualification remain unchanged.
The fixture contacts only a loopback object server with synthetic credentials,
not a real provider or live disk.

```sh
# Ordinary backend/Clippy/namespace/doctest qualification.
gh workflow run ci.yaml --ref YOUR_BRANCH -f backend-only=true

# Separate measurement job; this does not replace ordinary qualification.
gh workflow run ci.yaml --ref YOUR_BRANCH \
  -f backend-only=true -f publication-measurement=true
```

The measurement itself can also run on an isolated build host:

```sh
node scripts/test-backend.mjs -p cairn-installation --lib \
  nodes::publication::pipeline_tests::measurement::direct_publication_window_comparison \
  -- --exact --ignored --nocapture --test-threads=1
```

The fixture crosses the actual manager ownership transaction and SDK presigning,
an authenticated controller route, the owned node upload task and retained
publication reader, the production codec and the global PUT semaphore. It runs
128 unique 4 MiB immutable blocks. A seeded 18% random prefix and zero remainder
produce the same plaintext hashes in every mode. One fixture supplies the same
vault, object IDs, keys, codec and source file to both modes. Fresh encryption
nonces change ciphertext contents, but encoded byte counts must match exactly.
The object bodies are cleared before each trial to measure new writes rather
than authenticated 412 replay. No host page cache is dropped.

Each of three conditions has one warmup pair and three measured pairs, alternating
serialized reference and two-batch candidate order:

- no injected control or PUT delay;
- 25 ms control delay per batch, 20 ms per PUT, plus a 100 ms tail on one member
  of every four-object batch;
- the same delays with a shared 16 MiB/s object-body consumption ceiling.

`DIRECT_PIPELINE_HOST` records commit, kernel, architecture, available parallelism,
build profile and cache conditions. `DIRECT_PIPELINE_PERF` rows record exact data
identity, encoded bytes, write wall time, combined process CPU time, cumulative
process memory high-water, observed concurrency and existing manager/node stage
metrics. The manager, node handler and loopback object server share the process,
so the CPU and high-water values include all three. Stage durations can overlap
and must not be added as an exclusive wall-time breakdown.

After every write trial, a fresh real `LazyDisk` performs a verified direct GET,
decrypt/decode and cold 4 KiB read. It then averages 256 reads from the same
verified block cache. Those read measurements are outside the write interval;
"cold" describes the verified disk cache, not a dropped host/provider page cache.
The manifest is already loaded. Ciphertext corruption, journal preservation,
owner/grant revocation, malformed/ambiguous receipts, transport failures, capture
caller cancellation and two-publication fairness are separate correctness tests.

## Interpretation

This controlled debug-build reference demonstrates a scheduling mechanism; it is
not production provider latency or a prediction of production speedup. It omits
DNS/TLS/WAN, real provider scheduling, mounted journal bootstrap and production
contention. The aggregate ceiling controls loopback body reads, not the whole
network path. Compare only identical workloads, toolchains and host conditions.
Do not use shared-runner wall time as a flaky correctness threshold. The causal
gate is the held-PUT ordering witness, alongside identical byte counts and bounded
resource/ownership checks. A deployment and fresh equivalent runtime observations
are separate from this code qualification.

## Controlled CI result

The causal serialized-reference head was
`2316bef9416440964730b94a7159dd33e594c37c`: CI run
[41226547351](https://github.com/leo91000/cairn/actions/runs/41226547351) completed
the transfer and failed only the held-PUT ordering witness (2,055 other tests
passed). Compilation/setup failure was not counted as this causal result.

The paired comparison ran both schedules at
`3d285e2c6a2f94b969a7dda194b2d559fc6478a7`, in
[CI run 41230596729](https://github.com/leo91000/cairn/actions/runs/41230596729).
The measurement step passed; that experimental head intentionally retained the
serialized production default, so the production-route overlap test was the sole
backend-suite failure (2,057 other tests passed). The numerical comparison used
explicit windows one and two in the same production transfer implementation.
The selected production window is two; final-head correctness qualification is
separate from these historical experimental results.

Host: Linux `7.0.0-1017-azure`, x86_64, four reported execution threads, pinned
Rust 1.97.1, debug assertions enabled with CI debug symbols disabled. Every trial
wrote 536,870,912 plaintext bytes and 96,686,592 encoded bytes, with observed PUT
concurrency at most four in both modes. The dataset identity is
`b3-18824f41f57fdff79303892c3781121846cc1a97bef692699380c65ef1c155a0`.
The table contains medians of three measured, alternating post-warmup pairs.

| Condition | Serialized write wall | Window two write wall | Serialized / window two CPU | Wall reduction in fixture |
| --- | ---: | ---: | ---: | ---: |
| No injected delay | 9.902 s | 9.412 s | 33.037 / 33.171 s | 4.95% |
| Controlled stragglers | 14.594 s | 9.673 s | 33.093 / 30.707 s | 33.72% |
| Stragglers + body-rate ceiling | 20.270 s | 13.953 s | 33.253 / 29.040 s | 31.16% |

Cold verified 4 KiB read medians ranged from 142.202 to 143.359 ms across modes
and conditions; warm verified-cache reads ranged from 0.002747 to 0.002820 ms.
No read-path optimization is claimed. Cumulative process memory high-water was
about 302–345 MiB and includes the loopback object store and all earlier trials;
it cannot isolate candidate memory cost. Refer to the logical payload bounds
above and the raw CI rows for CPU/read and stage details. CPU differences are
observations of this debug fixture, not an independently attributed CPU gain.
No latency percentage here is a production speedup estimate.
