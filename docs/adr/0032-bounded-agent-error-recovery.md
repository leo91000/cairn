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
errors qualify. Structured error codes and HTTP statuses take precedence; the exact
routing timeout is a fallback for older adapters that supply only a message.
Unknown errors, cancellation, invalid requests, authentication, permissions, context
limits, storage/sandbox failures and subscription exhaustion are not retried here.
Existing account-switching and VM-controller recovery retain their own policies.

The worker records the first agent failure as a retry cause in its checkpoint. Only
a failed attempt with a saved session and matching cause can schedule recovery.
Tool output and an ambiguous process/stream failure alone cannot trigger this policy.
The previous execution must be fenced before admission can launch another attempt.
Recovery uses the same conversation and workspace and asks the agent to preserve
completed work and verify external effects before repeating actions.

The run's `retry` record contains the attempt count, limit, planned timestamp and
redacted cause. It is committed transactionally with queued status and an activity
event. There are at most three automatic retries per run, delayed 30, 60 and 120
seconds plus up to 20 percent positive jitter. Restart preserves the count and the
timestamp; it does not restart the delay or reset the budget. Backoff does not hold
an account lease. Existing workspace ownership, permission checks, execution budgets
and storage protections still apply when admission resumes the run.

Cancellation prevents another launch, including while fencing is still pending.
Manual resume explicitly starts a new retry budget. Success clears the retry record;
exhaustion or a changed terminal error leaves a normal failed run and its work intact.
Web conversation and run views display a local countdown and attempt count. Account,
capacity and fencing waits take precedence; the countdown makes no polling requests.

## Validation

The original routing error is injected through the native Codex app-server fixture.
The regression fails on the previous worker and verifies delayed recovery, restart,
the same session and workspace, and preserved work. Additional cases cover exhausting
the budget, a changed authentication error and cancellation during the wait. Policy
tests exercise structured classification and persisted delay boundaries; a browser
journey checks the countdown and automatic completion without new user input.
