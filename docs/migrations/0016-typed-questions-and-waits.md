# Installation database 15 → 16

`Store::open` upgrades version 15 before starting database actors or serving
requests. The upgrade uses one immediate SQLite transaction. As for other
installation upgrades, stop the previous manager and retain a consistent backup
of `manager.db` and its WAL. Never open this database with two manager versions.

Question definitions move from `chat-question:*` keys into `chat_questions`,
ordered `question_fields` and `question_options`. Their integer relation keys
avoid repeating the external question digest in every field/option row. Answer selections, comments and
private delivery text move from message payloads into `question_answers`,
`question_answer_choices` and `question_answer_deliveries`. Historical string-list
answers retain an explicit legacy kind and ordered `question_legacy_answer_items`,
including duplicates, empty lists and free text, without guessing selections or comments. Public message headers
contain a placeholder; the internal delivery reader reconstructs the original
text. A private sensitivity snapshot belongs to each reply, survives rewrites and
expired/purged/reused definitions, and defaults to private when its historical
definition is unavailable. Known non-private replies retain their visibility. Question, field, option and message identities and ordering remain intact.
Delivery text uses a rowid table: large text values otherwise cross SQLite's
smaller `WITHOUT ROWID` local-payload limit and waste overflow pages per reply.
Opaque retired engine extensions and any previous expiry are retained privately
in `question_import_metadata`; they never supply current definitions. Expired
questions remain expired. The `kv` constraint rejects the removed question path.

Conversation and mission continuations move into `durable_wakeups` and ordered
`wait_conditions`; `conversation_lineage` owns requester chat/run/request IDs.
An internal integer relation key links conditions to their wakeup; partial unique
indexes select either a conversation or mission owner, with foreign keys and an
exclusive-owner constraint. One lineage index covers both requester lists and
idempotent requests; absent request IDs remain distinct. External identities
remain unchanged.
Wait identity, turn, absolute deadline, reason, original delay/date, member order,
all/any mode, progress, next probe, transient errors, observations and plugin
payloads remain intact. Plugin conditions have an explicit input object and plugin
results an explicit result object. Numeric GitHub workflow IDs remain numeric.
Retained wait/wait_ready/wakeup events and run continuation snapshots receive the
same conversion without changing event IDs, order, timestamps or counters.

The old conversation `wakeup`/`requested_by` and message `answers` columns, and the
mutable run boundary `wakeup`, are removed in the transaction. Other domain rows,
credentials, references, identities and counters are unchanged. Foreign-key
validation and the version update precede the commit. Invalid input or any write
failure rolls back schema, data and version together; reopening can retry against
the unchanged version 15 database. No downgrade or reset is supplied.

`questions_waits_migration` loads a SQL dump produced by the real preceding
version's Store, then checks preserved data, secrets/order, grouped progress,
plugin history/mission snapshots, expiry/engine metadata, reopen stability and
rollback/retry. The fixture contains synthetic content and credentials only.

Retained snapshots can predate the GitHub plugin migration: native
`github_workflow` and `github_pr_checks` conditions/results did not include a
`pluginId`. The v15 importer identifies those two historical kinds as belonging
to GitHub before converting their condition input and result payload. This does
not create connections, grants or active continuations. Numeric workflow IDs,
commit identities, event envelopes, ordering and timestamps remain intact.
Unknown condition kinds and explicitly invalid plugin identities still fail
atomically. Synthetic regression fixtures cover the historical event shape,
group results, run continuation snapshots, absence of reactivation and rollback.

## Private Store-only rehearsal

The test-only `browser_fixture` binary supports `upgradeStoreCopy` before any
Config, Service, Vault, credential refresh or network setup. Build it from the
reviewed source (`cargo build --locked -p cairn-installation --example browser_fixture`)
or use that exact source's full-CI `browser-backend` artifact. It is not included
in production images and adds no service endpoint.

Use only a private, consistent **offline copy** of the installation database,
with the original backup retained unchanged. Never point this operation at the
live directory, even when the installed manager is stopped. For example:

```sh
browser_fixture <<'JSON'
{"operation":"upgradeStoreCopy","args":["/private/rehearsal-copy"]}
JSON
```

No config or credentials are needed. The directory must already contain
`manager.db`; a missing database fails without creating one. The command opens
only the real Store, runs its ordinary migrations, and reports `versionBefore`,
`versionAfter`, `quickCheck` and `foreignKeyCheck`. Its output contains no row
contents or identities. Failure returns a fixed diagnostic, without printing
the underlying private error. Check that the source backup remains unchanged
and the copy reaches the expected version before attempting the real upgrade.
