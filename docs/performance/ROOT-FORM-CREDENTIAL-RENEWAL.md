# Root form OAuth credential storage

Local native observation on 2026-10-23. Reproduce alone, using the standard
backend launcher and current native worker:

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  mise exec pnpm@12.3.4 -- pnpm test:backend -p cairn-installation --test plugin_resource_forms -- \
  oauth::measure_root_credential_storage --ignored --exact --nocapture
```

The [raw samples](root-form-credential-storage.json) compare the actual synthetic
root credential before, during and after one owner HTTP/Luau form renewal against a fake
OAuth provider. The initial authorization-code exchange is real host code; only
expiry is prepared in the fixture. No live provider or personal credential is used.
The token endpoint is held while the actual consumed state is captured: its
refresh token is absent and `oauth.refreshing` is true. The observation then
writes each captured value six times through native
`vault.set`, and reads its encrypted KV value six times using one reopened store
reader per case. The reference runs first, then consumed intent and renewed state.

| Metric | Expired reference | Consumed intent | Renewed candidate |
| --- | ---: | ---: | ---: |
| Credential JSON / encrypted-value JSON bytes | 229 / 346 | 209 / 318 | 226 / 342 |
| First reopened-reader read wall / CPU, ms | 0.474 / 0.618 | 0.427 / 0.442 | 0.412 / 0.439 |
| Five warm reads median wall / CPU, ms | 0.014 / 0.016 | 0.015 / 0.016 | 0.005 / 0.005 |
| Six writes median wall / CPU, ms | 1.454 / 0.205 | 1.490 / 0.187 | 1.557 / 0.187 |

The same existing KV key/envelope is reused. Consumed intent exchanges the stored
refresh token for a boolean; the smaller renewed value comes from shorter fixture
token strings. Neither difference is a compression improvement.
Serialized bytes exclude database pages, WAL, indexes and keys. Writes include
encryption and actor scheduling. Reads bypass authorization and decryption;
the first reader is connection-cold, followed by five warm reads, with OS caches
retained. This is not cold physical I/O. CPU is process CPU; every sample is
retained, without extrapolating a tail percentile from six values.

Fixture/schema preparation, OAuth HTTP, Luau execution and authority/approval
checks are excluded from these store timings. The writes measure `vault.set`,
not the two guarded compare-and-replace transactions of a full renewal. Their
latencies must not be inferred by multiplying these samples. The correctness tests separately
exercise serialized refresh, paused-instance configuration and late results
after owner, instance or package changes. These timings do not measure an entire
renewal, a production provider, physical durability or network/object storage.

Host: native Linux 7.2.9-arch1-1 x86-64, Intel Core i9-14900K, 32 logical CPUs
reported by `lscpu`, 12 available to Rust. Sampled cgroup `cpu.max`/`memory.max`
paths are unavailable. Temporary stores use `/tmp`, whose filesystem reports
`ext2/ext3`. No other test/build was launched by this agent during measurement;
unrelated host processes, filesystem caches and scheduling remain uncontrolled.
The optional consumed metadata remains inside the unreleased epoch-12 draft;
complete forward-transition/recovery qualification remains required.

This observation precedes integration of Main's guided setup transition. The
current unqualified Slack boundary is epoch 13 / SQLite 12; the original samples
remain historical evidence for the unchanged credential representation. See the
[current compatibility inventory](../SLACK-PLUGIN-COMPATIBILITY.md).
