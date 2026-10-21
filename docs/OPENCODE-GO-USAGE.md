# OpenCode Go usage

Cairn reads `GET https://opencode.ai/zen/go/v1/usage` with the connected
`opencode-go` API key on the installation manager. Other OpenCode model-provider
keys are never sent to that endpoint. Authentication headers, response bodies and
transport errors are not included in public account views or error messages.
Redirects are disabled, requests time out after 30 seconds and bodies are bounded
to 64 KB.

The adapter follows the [official endpoint](https://github.com/anomalyco/opencode/blob/2b8a5969e932c15083e599c82d34ce0268f81b9e/packages/console/app/src/routes/zen/go/v1/usage.ts)
and its [window analysis](https://github.com/anomalyco/opencode/blob/2b8a5969e932c15083e599c82d34ce0268f81b9e/packages/console/core/src/subscription.ts).
It maps `rollingUsage`, `weeklyUsage` and `monthlyUsage` to the existing usage
windows. `usagePercent` is the used percentage; `resetInSec` becomes an absolute
Unix timestamp in seconds. The 30-day duration on the monthly window is only an
ordering hint: the displayed reset always comes from the provider, including
calendar months with different lengths. No token pricing or fixed dollar allowance
is used to estimate consumption.

Usage is polled at most once per minute and stays current for five minutes.
A failed or malformed reading preserves the last successful windows and timestamp,
records a safe usage error and retries automatically. Unknown or stale usage keeps
OpenCode's existing admission behavior. A rejected API key requires replacement.
Replacing a key discards its previous usage. Other OpenCode providers retain their
existing five-minute retry after a native rate limit.

Current exhausted windows make Go wait and let Auto try the next connection in
the user's order. A later reading showing restored capacity makes it eligible
again. Explicitly pinned routes still wait for their own connection.

The provider's `useBalance` setting is read, never changed. When enabled, reaching
a Go quota does not prevent admission: the provider may continue on paid Zen
credits. The quota percentages stay unchanged on screen and both clients explain
the setting. A native rate-limit error still marks the account exhausted, because
the balance may itself be unavailable. Since the usage endpoint does not measure
Zen credits, this native-limit fence permits a retry after five minutes even if
the subscription percentages are unchanged. Cairn does not enable spending or redeem
additional capacity on the user's behalf.

## Compatibility and tests

There is no database migration or storage layout change. The account already
stores normalized `Usage` windows, attempt/check timestamps and flattened provider
fields. `useBalance` is an optional provider field; absent values mean disabled.
Previous releases can read these records, and this release can read existing
accounts without usage. The installation data compatibility epoch is unchanged.

The HTTP adapter and account-selection tests use synthetic keys and a loopback
server. Browser journeys use the same adapter against a local fixture through the
`opencodeGoUsageUrl` JSON configuration override. Overrides are accepted only for
loopback IPs; normal installations use the official endpoint. This does not add
custom model-provider execution endpoints. Android uses the same account payload
and native account detail sheet.
