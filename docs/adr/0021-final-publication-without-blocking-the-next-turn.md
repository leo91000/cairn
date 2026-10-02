# Final publication without blocking the next turn

## Context

A completed conversation remained in the manager's active set until its final
S3 publication finished. Retaining its VM did not eliminate this admission wait.
The retained capture also held the resume barrier while reconstructing blocks.
In production on v0.52.2, a follow-up sent 128 ms after completion started its
turn 19,947 ms after sending. This is one immediate-resume baseline, not a
percentile or an estimate of the storage cost alone.

## Decision

After fencing the execution and releasing its account and MCP leases, persist
a publication request and notify the bounded existing publisher. Release the
manager's active slot without waiting for S3. Requests have durable monotonically
increasing revisions; later requests coalesce behind the single publication
operation for that conversation. Restart discovers pending requests from the
database. Retries retain the existing five-second backoff and storage protection.

Successful guest sync precedes a local journal seal before releasing the turn's
execution lease. Its generation and completion timestamp persist in the same
transaction. Final publication reconstructs that completed prefix without
freezing or pausing a following turn. If it was already acknowledged, reconcile
its existing durable receipt instead of creating another publication. Older nodes
and pre-upgrade turns use the existing coherent capture path.

A periodic retained snapshot releases its resume barrier immediately
after sealing its immutable journal prefix. It keeps physical ownership through
manifest reconstruction. The following turn writes a newer generation; publishing
and reclaiming the older generation cannot discard those newer frames.

A retained VM with unpublished journal bytes is protected from ordinary expiry,
memory-pressure eviction and explicit disk pruning. Admission evicts other
unprotected retained guests, then the anonymous pool, or waits for capacity. RAM
ceilings remain enforced: protection does not authorize adding extra retained
VMs beyond their budget. Administrative shutdown still stops processes while
preserving the durable journal on the source node. The manager prevents automatic
or requested movement while a final publication request is unacknowledged.

Publication ownership uses the stable disk grant and source node, rather than
the previous turn's changing attempt identifier. Legacy disk publication keeps
its attempt fence. The published pointer remains `saving` until the node's
durable receipt arrives. An acknowledgement only covers the revision captured
by that operation; a newer request remains pending. Storage-status reconciliation
preserves these request and acknowledgement revisions.

## Validation

Two deterministic regressions fail with the old barriers and pass with the new
ones: manager release under a held publication operation, and retained resume
while reconstruction waits for a remote block. The latter also verifies old
captured bytes and preservation of a partial newer write after acknowledgement.
Admission tests protect an expired unpublished guest, evict the spare first,
and permit eviction only after acknowledgement.

The loopback S3 regression holds transfer and acknowledgement separately,
changes the attempt, writes the next generation and requests two more final
publications. It verifies that three requests produce two publications, that
the first receipt preserves demand, and that the final verified S3 block contains
the newer bytes. Existing crash/reopen tests verify generation retirement and
preservation of writes beyond an acknowledged prefix.

Production candidate measurements and prolonged VM qualification remain release
gates. Raw traces belong in release assets, not this repository.
