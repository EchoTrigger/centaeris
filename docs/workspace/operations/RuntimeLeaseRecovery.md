# Runtime lease recovery

A Worker that stops while holding a `leased` or `running` job must not create an
unlimited sequence of automatic recoveries. Expiration consumes a persistent
crash-reclaim allowance separate from `retry_count`, which records reported
failures. The allowance survives reopening the store and does not reset on a new
lease, a wake notification, or a service restart.

Core defines the shared policy: three automatic reclaims, exponential backoff
starting at two seconds and capped at sixty seconds, plus bounded 250 ms jitter.
The next expiration enters `dead_lettered` with
`lease_reclaim_budget_exhausted`. Each transition records the prior owner, count,
deadline, and error in an internal diagnostic. Exhaustion publishes the existing
terminal outbox event. Automatic scheduling stops; operators inspect the stored
job and diagnosis before choosing a recovery action.

A maintenance call transitions at most one hundred expired jobs, ordered by
lease expiration and job ID. This bounds the number of per-job writes and
diagnostics in one transaction. The shared named limit is a conservative
maintenance policy, not a throughput target or a time limit on legitimate work.
Subsequent calls continue the remaining jobs. PostgreSQL skips rows locked by
another Worker; SQLite serializes the maintenance transaction. Both retain the
existing lease-expiration index.

`question_wait` and `runtime_job_wait` yield their execution lease through the
existing wait protocol. They are not expired running jobs and do not consume
this allowance. Ordinary retry counts retain their existing meaning. New claims
mint new lease identities, and late owners cannot renew or publish through the
existing fencing checks. Wake notifications cannot erase the crash backoff.

The critical reclaim transaction uses PostgreSQL `synchronous_commit=on` with
`fsync` required, or SQLite `synchronous=FULL` on its fresh connection. These
settings do not change ordinary-operation durability. Tests verify the critical
settings and persisted transitions; they do not establish survival of actual
hardware power loss or safe external-provider replay after an unknown result.

## Upgrade and rollback

The explicit forward migrations upgrade SQLite schema 4 to 5 and PostgreSQL
schema 5 to 6. They preserve existing jobs, retry counts, lease owners, deadlines,
and migration history while adding the crash count and not-before fields.
Unsupported versions or malformed schemas fail validation.

Stop writers and take the consistent backup described in [Data](Data.md) before
upgrading persistent stores. An older binary does not understand the new schema:
reverting source alone is not a data rollback. Prefer a forward corrective
release that preserves the counters and fences. Restoring an older consistent
backup requires stopped writers and separate consideration of external work
whose outcome may already have committed; do not clear counters or blindly
re-execute jobs to make an old binary start.
