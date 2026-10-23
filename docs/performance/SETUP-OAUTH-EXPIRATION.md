# Inert setup OAuth expiration storage

Local native observation, 2026-10-23:

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  mise exec pnpm@12.3.4 -- pnpm test:backend -p cairn-installation \
  --test plugin_setup_activation oauth::measure_setup_credential_expiration_storage \
  -- --ignored --exact --nocapture
```

The [raw samples](setup-oauth-expiration-storage.json) use a credential obtained
through the actual owner setup HTTP API, code exchange and revision-checked save,
against a synthetic OAuth provider. The reference writes the same encrypted
credential without a deadline, as for a live root; the candidate retains the
original setup deadline in the existing KV expiration column. Each case performs
six encryption-plus-native-KV writes and six raw encrypted reads using a reopened
store reader. The reference runs first. No real OAuth account is used.

| Metric | Live reference | Inert deadline |
| --- | ---: | ---: |
| Plain credential / encrypted-value JSON bytes | 224 / 338 | 224 / 338 |
| First reopened-reader read wall / CPU, ms | 0.538 / 0.690 | 0.495 / 0.681 |
| Five warm reads median wall / CPU, ms | 0.015 / 0.016 | 0.015 / 0.017 |
| Six writes median wall / CPU, ms | 8.891 / 0.394 | 8.772 / 0.426 |

Equal encoded lengths exclude the key, SQL row/expiration column, index, WAL and
database pages. The first read is connection-cold with OS caches retained, not
physical cold I/O. Writes include encryption and actor scheduling but exclude
the guarded renewal transactions, authority checks, HTTP and Luau. Reads bypass
decryption. CPU is process CPU; all six samples are retained. This does not
measure a complete renewal, production latency or a reliable performance delta.

Host: Linux 7.2.9-arch1-1 x86-64, Intel Core i9-14900K, 32 logical CPUs reported,
12 available to Rust. Sampled cgroup limits are unavailable. Temporary stores
use `/tmp` (reports ext2/ext3). This agent ran no other local check during the
observation; other host workloads and caches remain uncontrolled. The functional
test separately advances the real credential to expiry, renews through the real
worker, then observes both draft and secret disappearing at the prepared original
deadline. The measurement itself does not demonstrate physical recovery.

No new key, table, encrypted envelope or credential field is introduced by
applying the existing expiry to OAuth writes. The complete unreleased epoch-12 /
SQLite-11 transition and recovery qualification remain mandatory.

This observation precedes integration of Main's guided setup transition. The
current unqualified Slack boundary is epoch 13 / SQLite 12; the original samples
remain historical evidence for the unchanged credential representation. See the
[current compatibility inventory](../SLACK-PLUGIN-COMPATIBILITY.md).
