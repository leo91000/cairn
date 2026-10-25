# Delegated plugin requester storage: local observation

Delegated first inputs now retain their original authenticated principal and one
boolean distinguishing their internal return path and personal suspension. Native
wakeups copy that source; a new human-authored input starts with its own authority.
The previous creation path stored neither value. These are noncredential metadata
in the existing KV table/envelope, removed by the existing conversation purge
prefixes. No provider token, question answer or model report is copied here.

The ignored native test uses the real `Store` with 50 synthetic external-source
inputs per shape. The unsuspended case has 100 KV values: 25,500 JSON bytes plus
10,400 key bytes; the suspended case has 25,450 JSON bytes with the same keys.
That is 510/509 value bytes and 208 key bytes per representative input, versus no
such metadata previously. Actual source/context sizes vary; this is not a bound.

Each read looks up the principal and boolean. The Store is reopened after seed:
first read is connection-cold, followed by five warm samples on that actor and
connection. OS cache remains. Each write sample is a transaction updating one
input's two values; the prior-path reference is a no-op transaction. Writes
include actor scheduling; reads are measured inside the actor. CPU is process
CPU across threads. Times are milliseconds; warm values are medians of five.

| Shape | Values | JSON bytes | Key bytes | Cold read | Warm read | First write | Warm write | Warm read CPU | Warm write CPU |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| previous-no-source | 0 | 0 | 0 | 0.462367 | 0.016353 | 0.156926 | 0.018766 | 0.017399 | 0.023102 |
| delegated-source | 100 | 25500 | 10400 | 0.427944 | 0.015376 | 0.168013 | 0.065513 | 0.015692 | 0.028896 |
| delegated-suspended-source | 100 | 25450 | 10400 | 0.548859 | 0.035728 | 0.268679 | 0.075227 | 0.037075 | 0.083221 |

Reproduce from the repository with the command recorded in
[raw observations](DELEGATED-PLUGIN-SOURCE.local.json). That file includes every
sample, source hashes and observed host parallelism/load/I/O pressure; no cgroup
CPU or memory ceiling file was available. This shared native Linux node had 12
available processors and uncontrolled concurrent host work. Case order is fixed.

Serialized byte counts exclude SQLite pages, compression, indexes, WAL and vault
storage. This measures incremental KV primitives, not either version's complete
chat-creation path, rule-comparison scans, package files, remote authority,
provider execution, network latency, startup or recovery. No speedup, production
latency or complete migration claim follows from these samples. The unreleased
epoch-14 / SQLite-13 candidate, absent automatic migration and whole-service/vault
forward/recovery qualification gates remain unchanged.
