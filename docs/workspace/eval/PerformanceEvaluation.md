# Performance evaluation

This page defines reproducible measurements. It contains no local run receipt,
developer path, ignored-log link, provider credential, or release claim.

## Aggregate

Run the current focused gates and write the local aggregate below the ignored
`test-results` directory:

```powershell
node scripts/performance-eval.mjs --output test-results/performance-repair-current.json
```

The default evaluator executes current Core continuation and context gates,
local JSONL/content-addressed storage gates, and PostgreSQL serialization gates.
A checked-in fixture is reference-only and cannot replace current observations.

## Real PostgreSQL

The transactional database gate is destructive to its dedicated test schema.
Run it only against an authorized disposable database:

```powershell
$env:CENTAERIS_ALLOW_POSTGRES_TEST_RESET='1'
node scripts/performance-eval.mjs --postgres --output test-results/performance-repair-postgres.json
```

Serialization measurements are not database I/O measurements. The real gate
reports actual rows and payload bytes but does not infer network round trips,
index size, MVCC overhead, or production latency.

The evaluator can validate a previously captured PostgreSQL artifact when its
source fingerprints and required gates match. An imported artifact remains
marked as not executed in the current invocation and must not be presented as a
new database run.

## Context and provider cache

Core projects context under token pressure without a message-count ceiling and
keeps tool groups atomic. Synthetic boundaries at 80, 4,096, and 10,000 messages
test prefix stability and retention; their elapsed time is an in-process
diagnostic sample, not provider latency.

A provider cache-hit rate is reported only when the provider returns the
corresponding cached-input usage field. Stable prefix bytes and estimated tokens
do not prove a provider cache hit. A real provider run requires explicit
authorization for the credential, request count, model, and cost boundary.

## Storage growth

Observation tests separately report logical events, physical rows, manifest
nodes, content bytes, and submitted payload bytes. Results from different
workloads are not compared as though they shared a baseline. Claims of linear
growth apply only to the named workload and measured quantity.

## Worker and browser

Worker pickup tests use a deterministic clock or a clearly identified local
sample. Browser measurements use one `performance.now()` clock from event
acceptance through reducer application, DOM commit, and a double-animation-frame
boundary. That boundary proves a paint opportunity, not physical GPU display
time.

## Evidence classes

Every report labels each result as one of:

- deterministic code regression;
- synthetic in-process benchmark;
- real PostgreSQL test;
- Docker startup or execution test;
- browser DOM timing;
- real model-provider usage.

One class never substitutes for another. A passing working-tree report is not a
clean-clone release gate, and an ignored local artifact is not a public release
asset.
