# 0020 — Bounded CPU-paused conversation retention

Date: 2026-10-01

## Decision

Keep a successful Codex conversation's own VM for three minutes, CPU-paused,
in a separate policy from the anonymous single-use pool. Operators can use
`LEO_VM_RETENTION_SECONDS=0..300`. Never retain failed/cancelled attempts or
command plans. Never transfer native or guest memory to another conversation.

Before CPU pause, inflate the existing balloon while the guest can cooperate.
Keep at least 1 GiB of usable guest address space and 768 MiB of available-memory
headroom. Free-page reporting punches holes in the shared guest memfd. No global
guest cache drop is added: the balloon already reclaims available clean pages,
and the bound is based on physical allocation after reclamation. A missing,
failed or unacknowledged memory/pause operation retires the VM safely.

RSS alone misses allocated memfd pages held by KVM/backend mappings. Count each
backing inode's allocated blocks once, then add VMM resident memory outside its
shared mapping. Unknown cost makes retention ineligible. Limit idle guests to
two and combined cost to min(2 GiB, 25% of the node budget); require total node
usage below 75%. The cgroup hard ceiling and active-admission checks remain.
Evict oldest retained guests, then the anonymous pool, before refusing active
work; recheck actual usage after each eviction. This permits bounded reuse of
idle virtual address spaces without reserving their nominal size.

Successful guest sync and closed account relay precede retention. Serialize
reclamation/pause with the attempt's existing safety lock. Retention waits at
most 250 ms for that lock: an unsettled capture skips this optional optimization
and follows normal cancellation/teardown instead of blocking completion. An idle
capture holds physical ownership and a per-conversation barrier without blocking
admission on other conversations. It neither resumes CPUs nor cancels mounted reads.
Paused captures are explicitly crash-consistent: CPU pause does not flush dirty
guest pages, and background processes may have written since the turn's sync.
Every new turn renews its manager/node/account/MCP access; the old disk binding
stays immutable so outstanding publication receipts remain valid. Changed mount
or privilege geometry and changed budgets require a cold VM. Revocation,
expiry, eviction and deletion never depend on saved RAM for durable recovery.

## Validation and measurements

The comparison uses the same runner image with retention enabled/disabled,
real Firecracker/vhost and pinned native Codex, a synthetic account, model and
MCP origin, and the same three-turn conversation. Account cloud configuration
is also synthetic, so fake credentials never reach production services. Model
responses wait 1.5 s deliberately; pre-model timestamps exclude that wait.
There is no manager, UI, S3 or WAN in this local comparison. Raw samples remain
outside Git and will be release assets only with a qualified measured release.

The ABBA comparison (off/on/on/off, two three-turn conversations per setting)
has four cached resumes per setting on the identical image. Median pre-model
latency is **1,165.5 ms without retention vs 477 ms with retention** (-59.1%).
Observed ranges are 1,139–1,328 ms vs 453–490 ms. Median total including the
synthetic 1.5 s response and the HTTP wait endpoint's polling is 3,379.5 vs
2,680 ms; that total is not a shutdown microbenchmark. New conversations still
boot cold in this comparison because the anonymous pool is disabled for both
settings. These local numbers do not replace the production phase measurements.

Actual guest/VMM RAM in the retained comparison is 743–798 MiB paused vs
765–809 MiB active, for a 9,728 MiB virtual guest. Reclamation recovers only
9–39 MiB additionally because free-page reporting already returns unused pages;
its measured cost is 265–292 ms. A separate 4 GiB node test
passes account and MCP renewal/removal, unique prior-turn context recovery,
repeated paused captures and verified publication, forced VMM crash recovery,
and eviction for three simultaneous active tasks.

A 349-second soak completes 100 native turns, 200 paused captures and ten
independently digest-verified loopback publications. The same physical VMM
serves turns 1–99; after an injected SIGKILL, turn 100 recovers its unique prior
context from disk on a new VMM. There are no unexpected VMM replacements.
All 100 turns renew account access; MCP credentials change and the server is
removed without leaking its old tools. Deletion and active-work eviction reap
the paused VMM and free its slot. A separate three-second TTL test confirms
expiry followed by durable context recovery.

During this soak, allocated guest/VMM memory is **738–858 MiB retained vs
770–897 MiB active**. Balloon reclamation recovers 11–90 MiB additionally and
takes p50 318 ms, p99 369 ms, maximum 370 ms. The 98 retained resumes have
pre-model p50 520 ms, p99/max 732 ms. Node admission usage stays below 1,657 MiB;
the container's total cgroup charge also includes reclaimable backing-file cache.
An independent observer inspects the broker's backing memfd inodes and VMM RSS
after every pause and agrees with admission within 32 MiB. Aggregate cgroup
`shmem` proved unsuitable for per-VM accounting because its updates can lag;
neither the implementation nor this final comparison treats that aggregate as
the physical cost of an individual guest.

## Memory accounting release gate

The earlier inode/cgroup discrepancy is delayed cgroup accounting. In a
stationary follow-up, the guest remains CPU-paused and its backing allocation
stays exactly 848,474,112 bytes. The cgroup discrepancy falls from 60.136719 MiB
to 12 KiB within 350 ms, without a new turn or guest allocation. The earlier
34.343750 MiB discrepancy on turn 94 is therefore not evidence of a leak.

A 600-turn, 24-minute audit cross-checks unique backing inodes and the VMM's
private memory against independent `smaps` page-table measurements. The full-byte
admission calculation is conservatively 12 KiB higher on every sample. The
rounded MiB health display and sequential sampling differ by at most 1.09 MiB;
the admission bound uses recomputed bytes, not that display or aggregate cgroup
`shmem`. Injecting 2.81 GiB of real additional backing allocation causes eviction,
complete reaping and durable context recovery on the next turn.

That successful trial uses one physical VMM, 120 paused captures and 12 verified
publications. Its pool is disabled, so native Codex restarts each turn. Separate
900-turn resident-native audits cover Codex 0.159.3 and 0.160.0, retain one native
PID, renew access on every lease, and reap the process at the end. These are
local synthetic endpoint qualifications, not production latency measurements.

The 600-turn trial also reveals real, correctly charged growth from 757.36 to
1,020.08 MiB. It is separate from the counter discrepancy. A subsequent 300-turn
guest diagnostic keeps one VMM: persistent Leo anonymous RSS stabilizes around
16–18 MiB after warmup, while guest-only cache reclamation plus three active
seconds reduces retained cost to 697.77 MiB, below the initial 747.91 MiB. A fresh
VM with the same intervention retains 662.82 MiB. Its 34.95 MiB difference from
the warm VM is counted; these snapshots cannot classify each remaining byte.

A 100-turn probe demonstrates additional free-page recovery with the persistent
process alive and anonymous RSS unchanged. Finer reporting plus additional
active time reduces allocated backing from 870.66 to 703.32 MiB after cache
reclamation, and normal balloon/pause leaves total cost at 591.68 MiB. Duration
and reporting order change together, so this does not isolate a production
tuning benefit; no production reporting setting changes. These workloads show
substantial recovery and no continuously growing persistent guest-process heap.
Safety still relies on the measured physical bound and eviction, rather than a
claim that finite tests prove indefinite leak freedom.

An isolated disk-pressure replay on the main image with Codex 0.160.0 raises
only the fixture's free-space reserve. Health reports `disk` pressure, the paused
VMM is reaped and its slot freed, and the next turn recovers its unique context.
This proves that pressure path, not the historical cause of the earlier failed
long trial whose eviction log lacks a reason. Eviction logs now include the
decision reason, pressure type, node RAM and retained allocation for future
diagnoses; the admission thresholds and teardown behavior stay the same.
