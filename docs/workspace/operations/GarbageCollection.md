# Garbage collection

The garbage collector reclaims eligible deleted resources, workspace snapshots,
and, when requested, orphaned library objects after their existing retention and
ownership checks. A failure in one collector does not prevent the independent
collectors from running. The command still reports failure after completing the
remaining collectors. The container entrypoint waits 86400 seconds before the
next pass even when the command fails.

## Persistent retry limits

| Setting | Default | Accepted range |
| --- | --- | --- |
| `GC_MAX_CLEANUP_ATTEMPTS` | 5 | 1–100 |
| `GC_RETRY_BASE_SECONDS` | 86400 | 1–2678400 |
| `GC_RETRY_MAX_SECONDS` | 604800 | 1–2678400 |

Values must be positive integers. Empty, negative, zero, malformed, or excessive
values fail startup; the retry maximum must be at least the base. Compose passes
these settings to API, initialization, and GC processes. The retry delay doubles
after successive failed attempts, up to the configured maximum. An expired
cleanup claim consumes its persisted attempt rather than resetting the ceiling.

Retry state survives process restarts. A failing resource or exact storage key
is quarantined when its attempt limit is exhausted. Healthy independent objects
can still be reclaimed. Quarantine retains physical bytes and ownership facts;
neither elapsed time nor a failed deletion proves that storage is free.

## Operator response

Monitor GC command failures and error logs containing `Storage cleanup
quarantined` or `Derived resource cleanup quarantined`. Investigate the reported
resource identity or key digest, the stored failure, filesystem permissions,
backend availability, and current ownership before acting. Treat quarantine as
an operational alert requiring review.

There is no automatic quarantine reset or public release endpoint. Do not delete
retry records, clear attempt counters, or delete retained snapshots to silence an
alert. Manual repair must preserve a restorable database and storage backup,
establish the exact affected object and its current ownership, and verify any
physical deletion before claiming capacity was reclaimed. Resume cleanup only
through a separately reviewed, targeted maintenance procedure after the cause
has been resolved. A broad recursive deletion is not a recovery procedure.

## Migration and rollback

`0009_storage_gc_backoff` follows `0007_business_definition_instances` and
reconstructs existing failed-resource retry state on the selected database alias.
Existing failed resources retain their state and attempt count, and receive a
one-day retry delay. The first eligible claim then applies the configured attempt
limit before physical deletion. The migration does not prematurely quarantine
resources using a fixed limit that may differ from the deployment configuration.

The reconstruction is intentionally irreversible. Running `migrate 0007` is not
a supported rollback. A code rollback may retain the expanded database schema.
A full database rollback requires a verified consistent backup and a recovery
plan that keeps database ownership records and physical storage consistent.
Do not clear durable retry state or delete history to make a downgrade succeed.
