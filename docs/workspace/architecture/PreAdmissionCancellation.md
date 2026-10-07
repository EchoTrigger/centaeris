# Cancellation before Session admission

Cancelling an unadmitted run must not fabricate a Session turn. Runtime commits
`AgentRun.preAdmissionCancelledAt` as a hosted receipt instead. This is distinct
from requesting cancellation and from the API status projection.

The receipt transaction verifies the run/session/workspace binding, current
lifecycle lease, authorization digest and durable cancellation request. It locks
the Session against admission and rejects existing run-level Session records.
The timestamp is immutable across retries. New user-turn admission rejects a
receipt committed first. Runtime terminal reads and API projection both recognize
the receipt; worker teardown and job completion continue through the existing
terminal path. API projection clears the provisional `startedAt`, preserves
`execution_queue_expired`, and writes no Session records.

The first-release initial schema includes `preAdmissionCancelledAt`. Runtime
does not rewrite dead-letter rows without verified evidence. Regression coverage includes stale leases, absent
cancellation requests, idempotent receipts, Session zero-write, terminal resolve
after interrupted delivery, and API rejection of an unsupported cancelled claim.
