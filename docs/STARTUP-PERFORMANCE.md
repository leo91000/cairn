# Agent startup measurements — 2026-09-12

The original Hello chat spent about 41.5 seconds preparing nine repositories,
then another 13.5 seconds before `thread.started`. That latter interval included
copying the repositories into the guest; it was not just Firecracker boot time.
The original archive contains 1,146,757,120 bytes (1.07 GiB). Preparing projects on
demand removes both the cloning and import costs from a chat that needs no files.

## Method

Tests used a separate controller on the actual VPS, with private copies of the
original workspace and disposable VM disks. The production source was read-only;
production services, credentials and conversations were not modified. The guest
had two vCPUs and 4 GiB RAM. Final comparisons use the production controller limits
of eight CPUs and 20 GiB RAM. Each variant alternates three empty starts and three
starts with all nine repositories. The guest image is identical between variants;
only the controller's archive encoding changes.

The guest reports entry, toolkit initialization, and real Codex app-server
`initialize`, `model/list` and `thread/start` durations. No model inference is
requested. Account authentication and external MCP initialization are excluded,
so these are infrastructure measurements rather than a promised reply latency.
Controller probes separately measure disk preparation, network setup, guest boot
and imports. First output includes log transport latency. The first empty start
after controller creation is slower than subsequent starts.

## Results

Medians from the final comparison at production CPU/RAM limits:

| Measurement | JSON/Base64 | Binary |
| --- | ---: | ---: |
| Import all nine repositories | 10.01 s | 7.55 s |
| Reach the agent process with those repositories | 12.40 s | 10.38 s |
| Toolkit and Codex initialized with those repositories | 13.65 s | 11.62 s |

Binary streaming reduces import time by **24.6%** and complete measured
initialization by **14.9%** for this workload. The initial experiment under a
smaller controller limit also showed an improvement (22.1% on imports).

Without repositories, subsequent starts take **about 3–4 seconds** to initialize
Codex. The first start after creating the test controller takes **6.5–7.2 seconds**;
cache state and host load matter. Avoid treating the empty-start differences
between encodings as meaningful: only a tiny probe script is being transferred.
On-demand preparation is the major improvement for a Hello chat; binary streaming
mainly helps when an actual repository must be loaded.

## Decision

Retain binary archive streaming. It avoids the 33% Base64 expansion and repeated
JSON serialization, without compressing already-compressed Git objects. A guest
capability flag selects the encoding and preserves compatibility in both directions.

Do not add archive compression for this workload: `zstd -1` reduced the real
archive by only 10.5%, while archive creation rose from about 1.1 to 3.0 seconds.
The tested single-threaded and two-threaded settings gave similar results;
level 3 took 3.8 seconds for little additional reduction.

The boot itself was approximately 1.7 seconds. There is no evidence here to
justify keeping idle VMs alive just for a small further startup improvement.

## Validation

`cargo test --locked --workspace` (55 tests), project lint and Clippy pass. Archive tests cover all byte
values, fragmented reads, truncated data, explicit termination, size limits and
transition back to JSON control messages. `tests/runner-smoke.mjs` passes with
both a binary-capable guest and an older JSON-only guest: initial and on-demand
imports, live inbox updates, preserved edits, cancellation, controller crash,
recovery, nested Docker, and read-only policy after reboot. The VPS comparison
also exercises an older controller importing into a newer guest.

Raw aggregated samples are in [the benchmark data](https://github.com/leo91000/cairn/releases/download/v0.50.6/startup-2026-09-12.json).
The temporary diagnostic probes were removed from production code. The throwaway
harness and raw logs remain in `/var/tmp/cairn-startup-bench` on the development
machine; the harness uses disposable data and must never target a live run disk.
