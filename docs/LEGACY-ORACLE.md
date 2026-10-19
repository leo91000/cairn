# Frozen Node oracle: suite and consumer inventory

F15 / #257, audited on 2026-10-18 against main
`c4012c80f821ddcec3c29ac337c7df769238db75` (after F16 / #287).
The baseline `pnpm test` passed: 65 files, 553 tests. No suite was selected for
removal because of a failure or flakiness.

The audit still finds the original 15 Vitest suites: 13 import
`tests/fixtures/legacy/server` directly; `maintenance` and `mcp` reach it through
`legacy-helpers`. This change removes the two suites whose installation-local
authorities have been retired. It retains the other 13 as historical contract
references where scenario parity has not been established. Their passing results
do **not** verify the Rust implementation. Port the missing scenarios through the
current native interfaces before removing those references; matching test names
or an implementation check alone is insufficient.

## Removed scenarios and current coverage

[ADR-0028](adr/0028-autonomous-installations-behind-the-official-service.md)
removes installation-local passwords and bootstrap setup, and puts account
authentication and external MCP authorization on Beacon. Push devices likewise
belong to Beacon accounts: `crates/installation/src/notifications.rs` removes
local registrations and only maintains relay events. These decisions already
exist on main; this change does not change their behavior.

References below name executable tests in the current Rust/Web suites. They
identify active coverage, not a claim that every assertion of an obsolete Node
implementation has a byte-for-byte replacement.

| Removed suite / scenario | Retirement or current equivalent |
| --- | --- |
| `auth`: bootstrap secret, password login, repeated setup, logout | Retired local authority. `crates/installation/tests/http.rs`: `installation_http_accepts_only_trusted_context_and_checks_host_and_origin` rejects local cookies/setup and accepts trusted identities; `removed_login_cannot_be_enabled_by_forged_forwarded_ips` rejects password login. |
| `auth`: concurrent password setup | Retired local setup, not a current account-creation contract. |
| `auth`: hostile Origin/Host, Unicode comparison, local session expiry | Host/Origin assertions remain in the first native HTTP test above. The local session lifetime and `safeEqual` primitive belong to the retired implementation; no equivalent Unicode primitive assertion is claimed. |
| `auth`: one-use code binding to client, redirect, verifier and resource; scope enforcement; access-token storage secrecy | `crates/beacon/tests/public_mcp.rs`: `oauth_pkce_registration_rotation_and_reuse_are_bound_to_the_selected_installation`, `native_loopback_ports_vary_but_other_redirects_and_code_bindings_stay_exact`, `replaying_an_authorization_code_revokes_its_access_and_rotated_refresh_family`. `oauth_refresh_revocation_removes_access_and_rejects_further_refresh` queries native token storage and checks that access and refresh rows contain their SHA-256 digests, never the plaintext tokens. Current authorization selects an installation through the Beacon owner account. |
| `auth`: refresh rotation, scope escalation rejection, family revocation on reuse | `oauth_pkce_registration_rotation_and_reuse_are_bound_to_the_selected_installation` explicitly exercises these assertions in Beacon. |
| `auth`: refresh-token revocation, invalid redirects, expired codes | `oauth_refresh_revocation_removes_access_and_rejects_further_refresh` ports refresh-token revocation and proves access/refresh rejection afterwards. `native_loopback_ports_vary_but_other_redirects_and_code_bindings_stay_exact` now also rejects wrong-resource exchanges; the new refresh test checks the same binding for refresh. Redirect restrictions and expiry remain tested by the native loopback test and `an_expired_unused_authorization_code_cannot_issue_tokens`. |
| `auth`: consent denial and unsupported password grant machine errors | `crates/beacon/tests/sensitive_access.rs::mcp_consent_requires_proof_for_approval_but_not_denial` covers current consent denial. `oauth_refresh_revocation_removes_access_and_rejects_further_refresh` also checks the unsupported grant's machine error. |
| `notifications`: authentication, CSRF and provider URL validation for installation subscriptions | Old installation subscription API removed. `tests/e2e/chats.spec.ts` asserts `/api/notifications` is 404. `crates/beacon/tests/notifications.rs`: `registering_push_requires_a_recent_proof_but_disabling_it_does_not`, `web_push_devices_belong_to_the_signed_in_cairn_account`, `device_limits_and_subscription_writes_are_isolated_per_account` cover the current account API and hostile endpoints. |
| `notifications`: encrypted local subscriptions/VAPID key persistence; private payloads and single delivery | Installation-local encrypted keys are explicitly discarded by `remove_local_registrations`; retaining their persistence assertion would specify the removed design. Beacon uses an operator key: `notification_configuration_exposes_only_the_public_operator_key`. `relayed_questions_and_alerts_target_only_current_installation_accounts` checks recipient scope and private question text; `healthy_devices_are_not_redelivered_after_a_partial_failure_and_tunnel_reconnect` checks deduplication. No new at-rest encryption claim about Beacon's device store is made. |
| `notifications`: retry after restart, answered-question discard, expired subscription removal | `transient_push_failures_retry_after_reconnection_and_expired_devices_are_removed` covers retries/expiry with the actual relay; `crates/installation/tests/durability.rs::concurrent_messages_and_answers_are_idempotent_and_survive_restart` now also checks that the relay has one pending event before answering and discards that event after answer/restart; `tests/e2e/beacon-notifications.spec.ts` exercises the account notification flow. The old local `Notifications.flush` implementation is retired. |

## Retained suite matrix

All paths in the first column are under `scripts/tests/` and end in `.test.ts`.
These are useful oracle scenarios with incomplete demonstrated native parity,
not 13 additional production backend implementations. The native examples are
representative overlaps; the last column explains why deleting the whole suite
would be premature. Historical Docker paths, flat MCP catalogs and local auth
helpers within a retained suite are not specifications for today's APIs.

| Suite | Scenario groups inspected | Active overlaps and retention reason |
| --- | --- | --- |
| `chats` | default/project access; queue/edit/cancel/dedup; steer/follow-ups; restart; lost completion; disconnected process; restricted inbox; pause/model override; stale questions; persisted async questions | `durability.rs::concurrent_messages_and_answers_are_idempotent_and_survive_restart`, `worker.rs::native_chat_turns_reuse_the_same_run_and_conversation`, `chat_process.rs` and browser chat journeys cover current flows. Retain until the lost-completion/disconnect/queued-follow-up and restricted-inbox scenario matrix is demonstrated through native callers. |
| `codex-account-resets` | idle 2%/credit expiry; weekly/model checks; identity mismatch; natural reset first; lost response/restart; delayed readings; ambiguous failures; low/healthy poll rates; same-session recovery; fallback after redemption failure | Native banked-reset logic exists in `src/accounts/codex.rs`, but the inspected native tests do not demonstrate this redemption matrix. Keep all cases; an implementation search is not replacement coverage. |
| `codex-accounts` | ranking/reservations; unusable/model capacity; natural resets; rotated credentials and crash recovery; identity; CRUD/device login; polling; selected credential isolation; import/encryption; timeout/queue; missing CLI; exhaustion switching/wait/cancel; exhaustion error classification | `accounts.rs` migration/pool tests, `codex.rs::codex_usage_requires_observed_capacity_and_respects_model_limits`, credential-broker tests and `worker.rs::usage_exhaustion_switches_accounts_and_preserves_the_conversation` overlap. Exact timeout, polling, interrupted rotation and ambiguous-error assertions are not all established. |
| `execution` | independent Git checkout with local commits and dirty edits, full/`blob:none`/`tree:0` clones, historical missing objects and source preservation | `project_workspaces.rs::new_git_work_is_fresh_and_local_snapshots_preserve_the_registered_checkout` and `partial_clone_missing_blobs_are_fetched_from_the_real_promisor` test the current project model. Keep the three-way historical preparation reference until the entire assertion set is accounted for. |
| `maintenance` | dedicated credential/lease owner; lease arrives while scheduling; abandoned lease expires | `http.rs::health_and_deployment_lease_report_live_worker_ownership` tests ownership. The in-flight race and expiry scenarios lack demonstrated exact native counterparts. F16 explicitly preserves this lease. |
| `mcp` | stateless SDK discovery/task actions; command connections/agent grants/secrets; browser OAuth; management scopes; self-gateway mutation/deadlock; protocol versions/revocation | `mcps.rs::official_clients_negotiate_modern_and_legacy_protocols_and_enforce_scopes` tests today's `search`/`execute` interface; `installation_mcp.rs` tests operation scopes; `agents_manage_connections_through_the_self_gateway_without_deadlocks_or_stale_grants` tests reentrancy. Retain the remaining operation/secret/self-mutation matrix, without reinstating the old flat catalog. Preserve #281 and F11/#285 generated-catalog freshness checks. |
| `mcp-connections` | confidential OAuth; command environment; PKCE/replay/encryption/concurrent refresh; issuer substitution/superseded consent/denial; tool/resource/prompt grants/revocation; write-only secrets/restricted agents; private/metadata address blocking | `mcps.rs::stdio_discovers_and_calls_the_beacon_sdk_fixture`, `oauth_consent_pkce_callback_replay_and_refresh_use_the_existing_provider`, `network_guards_block_metadata_even_when_private_network_is_allowed` overlap. Confidential-client, issuer-substitution, concurrent refresh and full outbound grant cases still need demonstrated native parity. |
| `policy` | default Main/project-free task; project/skill restrictions; queued permission change; omitted access on edit; unavailable isolation; staged resources/login/home boundary; GitHub credential secrecy/deletion; Docker mount translation | `durability.rs::restricted_agents_cannot_escalate_projects_skills_or_the_main_policy`, queued-access tests in `worker.rs`, and `installation_mcp.rs` conservative-update tests overlap. Keep the exact restricted profile/resource/credential assertions until ported. Docker mount translation alone is historical. |
| `restart` | shutdown/resume budget; explicit/queued/crash cancellation; isolated teardown/home; lost thread notification; missing history; checkpointed completion; fresh capacity; retained credentials/locks; reduced permissions; dirty worktree; incomplete preparation generation | `worker.rs::abrupt_restart_fences_previous_process_and_resumes_workspace`, parallel managed-task recovery, cancellation and access tests overlap. Full process identity/remaining-budget/lock-release failure ordering is not established as equivalent. Preserve the subprocess fixture too. |
| `runner-lifecycle` | stop fences an in-flight start; stopped attempt stays rejected after lifecycle reconstruction; removal error propagates and retry works | `src/runner/broker.rs::lost_logical_completion_stays_fenced_until_physical_cleanup_acknowledges` and `lost_physical_completion_and_missing_subscription_remain_fail_closed` test current teardown. Exact delayed-start rejection after broker reconstruction is not demonstrated. Keep the scenario reference despite obsolete Docker callbacks. |
| `service` | concurrent enqueue/immutable snapshot/references; catch-up/overlap; pause/invalid-skill loop; DST/cron; roots/missing projects; sanitized Git origin; skill files/frontmatter; traversal/symlink roots | `durability.rs::weekly_schedules_and_dst_transitions_match_the_existing_scheduler`, `skills.rs` and `installation_mcp.rs` overlap. Catch-up/overlap, invalid-skill hot-loop, sanitized origin and exact symlink write cases lack demonstrated complete native coverage. |
| `store` | task activity beyond history page; v1 event payload migration/reopen; rollback/expiry/downgrade; compact pagination/detail/archive restrictions | `durability.rs::writes_serialize_and_failed_transactions_roll_back`, `block_codec_migration.rs` and current HTTP/read tests overlap. Keep the historical v1-to-v4 oracle and expiry/pagination reference; it does not alone verify Rust upgrade behavior. |
| `worker` | JSON secret redaction; run MCP credential/revocation; subprocess streaming/usage; failure/cancel; project serialization; timeout/crash; dirty Git worktree; guided device login | Native `worker.rs`, `chat_process.rs`, `codex.rs` and `project_workspaces.rs` cover current execution. The complete structured-output/redaction/timeout/project-serialization assertion matrix has not been demonstrated as equivalent. |

Native paths in this table are in `crates/installation/tests/` unless prefixed
with `src/`. A retained scenario can later be retired individually with evidence;
the absence of a full-suite match must not prevent a focused native port.

## Consumers outside the 15 suites: preserve

| Consumer | Purpose / execution boundary |
| --- | --- |
| `tests/e2e/fixtures.ts`, `beacon-relay-fixture.ts`, Beacon sharing/installation/notification/relay specs | `Store` and `SeedService` seed or inspect SQLite. Browser HTTP and workers execute native binaries. Keep these adapters. |
| `tests/e2e/accounts.ts`, `tests/fixtures/codex-account-fixture.ts` | Account types, synthetic credentials and encrypted account fixture construction. Keep while consumers use this format. |
| `crates/installation/tests/worker.rs::migration_resumes_a_checkpoint_written_by_the_node_backend` → `tests/fixtures/legacy-worker.mjs` | Actual Node-to-Rust worker checkpoint migration, including session and workspace continuity. Keep executable oracle and its transitive modules. |
| `crates/installation/tests/durability.rs` | Native persistence/rollback, Node-compatible ciphertext, cron-parser comparison, question durability and worktree migration. Keep all tests; several use Node without importing the frozen server. |
| `tests/fixtures/worker-process.ts`, `runner-provider.ts` | Process/recovery and isolated execution fixtures used by retained suites. Keep their legacy chat-process dependency. |
| `scripts/benchmark-backend.mjs` | Comparative Node/Rust benchmark and shared historical SQLite seeding. Keep Node server branch, Auth and Store. This inventory does not rerun or claim validity of historical benchmark numbers against today's account authentication. |
| `tests/performance.ts`, `tests/serve.ts`, `tests/fixtures/legacy-helpers.ts` | Historical Fastify benchmark/serve harness and suite fixture. Keep for reference users; they are not the browser application launcher. |

`vitest.config.ts` retains its discovery globs and credential isolation. No
production source, persisted format, storage behavior, frontend or Android file
changes. Native tests gain the answered-notification and OAuth regressions
described above. Storage performance measurements, compatibility epoch changes, Claude
visual review and parity changes therefore do not apply to this deletion and
documentation change. The oracle server itself and its dependencies stay frozen.
