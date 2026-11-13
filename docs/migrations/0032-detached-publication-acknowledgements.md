# 0032: Detached environment publication acknowledgements

An expired environment's ambiguous publication acknowledgement can remain pending
while a fresh environment of the same conversation runs on another node. The old
publication keeps its shared-object references until its identity is durably fenced.

Version 32 adds `direct_journal_acknowledgements.detached` (default `0`, checked
boolean). It replaces the table's unconditional unique run key with a partial
unique index on `run_id WHERE detached=0`. Existing rows retain their publication,
run, node and retirement state and become non-detached. There remains at most one
active acknowledgement per run. Any number of detached environments may retain
independent pending acknowledgements. Ordinary collection's `retired` flag does
not free the active slot; only environment detachment does.

Environment retirement records also persist the exact detached publications and
physical-erasure progress. A fenced environment's manager run directory and local
publication directory move into `retired-environments/<retirement-id>/` before
admission resumes. Background work cleans only that namespace and its captured
publication identities; it never removes a replacement environment's files,
account lease or recovery references. Physical cleanup errors remain retryable
without globally blocking admission. Destination-specific retired disks still
must acknowledge deletion before that node admits a new environment of the run.

Abandoned conversion copies in the dedicated `backend-migration-staging` root
are discovered by background cleanup and queued in `manager-staging-retirements`
before erasure. Failures retain their error and retry deadline. Cleanup holds the
legacy storage owners and removes only the root contents, preserving a mounted
staging root. Environment retirement also removes the captured old manager
attempt plans by identity, including retries after restart; replacement plans
remain independent. These queues use existing records and add no SQL layout.

A shared manager plan is kept until every affected node acknowledges retirement
or is permanently revoked. Controllers persist their own run-to-attempt mapping
before creating a log or retrying legacy plan-only cleanup, so ordinary worker
plan removal cannot orphan an attempt's output after a failed deletion.
