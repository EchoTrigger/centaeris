# Storage execution and contract boundaries

Core owns storage-independent request rules. SQLite and PostgreSQL adapters own
atomic persistence, conflict detection, lease fencing and transaction outcomes.
Database formats and public network contracts are unchanged by the consolidation.

## SQLite job updates

`sqlite_reliability.rs` owns `complete_runtime_job_conn` and
`fail_runtime_job_conn`. These functions use their caller's connection and neither
open a connection nor start or commit a transaction. Ordinary job operations and
the external-context/dead-letter compound operations call the same updates.
Each outer operation still owns its transaction and terminal outbox publication.

`tests::job_updates` characterizes both entry points with valid and invalid lease
owners, the exact expiry boundary, terminal records, retries and missing retry
timestamps. An injected outbox failure must roll back the job and all associated
objects, links or dead-letter records. These tests pass before and after SQL
consolidation; they assert stored facts and errors rather than source spelling.
