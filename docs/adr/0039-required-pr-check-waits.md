# Wait for required pull-request checks at an exact head

Date: 2026-10-13.

Extend ADR-0038 with `github_pr_checks`, identified by an authorized project,
positive PR number and full head SHA. Observe through the same existing agent
GitHub connection, durable wakeup, cancellation and current-permission contracts.
The condition neither performs a merge nor grants authorization for one.

Query the PR's latest commit status-check rollup through GraphQL, including
GitHub's `isRequired` classification. Read classic branch protection and the
active branch rules endpoint to find requirements that have no result yet.
Rulesets can bind a check context to an integration; GitHub's required
classification permits attributing legacy status contexts only when their name
has exactly one requirement binding. Multiple app bindings, including unbound
plus app-specific requirements, return unavailable rather than assuming one
legacy status fulfills all of them. Unknown,
partial or inaccessible responses must never become a successful observation.

Success waits for all required checks. Missing, expected and running checks keep
the wait pending. A required failure or cancellation wakes immediately, even if
other checks have not started. Optional checks do not affect this condition.
No configured required checks returns `no_required_checks`, rather than CI
success. Required-workflow rules beyond status checks return unavailable.

Check identity on every check page and again after rules are re-read. A changed
head returns `superseded`, and a closed or merged PR returns `closed`. Changes
to the base branch or required checks during the observation cause another poll.
The observation is a read-only sample, not a merge reservation: the agent must
verify the current head, checks and other merge requirements before acting.

Bound each complete observation to 20 seconds, five pages of checks, two pages
of active rules and 500 distinct requirements. Limit returned check detail to
100 entries shared between present and missing checks, prioritizing failures,
while reporting full counts and truncation. Polling shares the
existing bounded, deduplicated observer; no network request holds a scheduler
transaction or conversation execution slot.

No new storage codec or table is introduced. The condition and observations
use the existing wakeup JSON. See `docs/WAIT-FOR-MEASUREMENTS.md` for reproducible
metadata measurements against sleep, with synthetic CLI probes distinguished
from production network latency.

References: [GitHub branch rules](https://docs.github.com/en/rest/repos/rules#get-rules-for-a-branch),
[required checks](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches#require-status-checks-before-merging),
and [GitHub CLI's rollup query](https://github.com/cli/cli/blob/trunk/api/queries_pr.go).
