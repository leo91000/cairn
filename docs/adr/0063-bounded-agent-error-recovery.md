# Bounded recovery after transient agent failures

Date: 2026-10-06.

## Context

A long-running Codex conversation failed with `workspace routing discovery timed out`.
Its workspace and recovery point were available, but the manager treated the failed
turn as terminal. The provider's routing failure and the installation's decision to
stop the conversation are separate concerns.

## Decision

`run_retry` classifies confirmed agent failures and plans recovery independently of
the provider adapter. Known connection, timeout, temporary server and request-rate
errors qualify. Structured error codes and HTTP statuses take precedence; two exact
messages are a fallback for older adapters that supply only a message and for
native Codex errors classified as `other`: the routing timeout, and the model at
capacity (“Selected model is at capacity. Please try a different model.”, which
native Codex reports as `serverOverloaded`) as a temporary server error. These
exceptions match only the complete message and cannot override a specific error
code or terminal HTTP status.
Unknown errors, cancellation, invalid requests, authentication, permissions, context
limits, storage/sandbox failures and subscription exhaustion are not retried here.
Existing account-switching and VM-controller recovery retain their own policies.

Codex's intermediate `error` notifications with `willRetry: true` are recorded
as conversation notices only for the active thread and turn. They show native
reconnection progress without marking the turn failed, scheduling another Cairn
attempt or switching accounts. The confirmed final turn failure remains the
source of Cairn's bounded recovery policy.

The worker records the first agent failure as a retry cause in its checkpoint. Only
a failed attempt with a saved session and matching cause can schedule recovery,
except the explicitly safe startup failures below.
Tool output and an ambiguous process/stream failure alone cannot trigger this policy.
The previous execution must be fenced before admission can launch another attempt.
Recovery uses the same conversation and workspace and asks the agent to preserve
completed work and verify external effects before repeating actions.

The run's `retry` record contains the attempt count, limit, planned timestamp and
redacted cause. It is committed transactionally with queued status and an activity
event. There are at most five automatic retries per run, delayed 5, 10, 20, 40 and 80
seconds plus up to 20 percent positive jitter. Optional `startedAt` and `deadlineAt`
fields persist a ten-minute window from the first failure. Once it expires,
admission does not launch another recovery attempt; an already resumed agent turn
continues under its normal execution budget. Admission rechecks the persisted
deadline transactionally after slow account preparation and releases any acquired
account if the window expired while preparing. Old records without these fields
remain readable and acquire a window when scheduling their next retry. The old
release ignores the added fields, so the persisted-data compatibility epoch stays
unchanged. Restart preserves the count and the
timestamp; it does not restart the delay or reset the budget. Backoff does not hold
an account lease. Existing workspace ownership, permission checks, execution budgets
and storage protections still apply when admission resumes the run.

Codex startup has one narrow exception to the saved-session requirement. Before
submitting a new `turn/start`, the adapter may mark an exact RPC timeout for
`initialize`, `account/login/start`, `thread/resume` or history reads, or a confirmed
app-server disconnect, as a startup failure. This also recovers a first message
whose session was never created. The failed process is closed and fenced, its
workspace retained and the same pending message used by a fresh process. The
checkpoint retains this confirmed pre-turn cause across cancellation and manual
budget resets, clearing it before a new supervised attempt, so manual recovery
without an existing session stays safe. Native preflight history reads propagate
transient failures into the same budget; missing executable and session mismatch
errors remain terminal. An
uncertain `turn/start`, a tool operation, an unrecognized error or an authentication
rejection cannot take this path. Native Codex reconnection within an active turn
remains separate from these manager retries.

Cancellation prevents another launch, including while fencing is still pending.
Manual resume explicitly starts a new retry budget. Success clears the retry record;
exhaustion or a changed terminal error leaves a normal failed run and its work intact.
Web and Android conversation and run views display a local countdown, segmented
attempt count, expandable cause and cancellation action. Conversation cancellation
uses the existing stop-and-pause action; run cancellation uses the existing stop action.
Both are also exposed through public MCP and the installation agent tools as
`stop_chat` and `cancel_run`. Account,
capacity and fencing waits take precedence; the countdown makes no polling requests.

## Validation

The original routing error is injected through the native Codex app-server fixture.
The regression fails on the previous worker and verifies delayed recovery, restart,
the same session and workspace, and preserved work. Additional cases cover exhausting
the budget, a changed authentication error and cancellation during the wait. Policy
tests exercise structured classification and persisted delay boundaries; a browser
journey checks the countdown and automatic completion without new user input.
