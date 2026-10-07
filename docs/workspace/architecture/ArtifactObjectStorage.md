# Artifact storage contract and isolated validation

Status: Contract and isolated validation completed on 2026-09-29. This is a proposed application storage
boundary, not a deployed backend. Production code, schema, Core pin, Hosted transcript
capture and deployment configuration are unchanged.

## Existing ownership and coupling

Paths below are relative to `packages/api/`.

| Path / symbol | Current behavior | Artifact slice consequence |
| --- | --- | --- |
| `app_core/artifact_publish.py::publish_artifact` | `default_storage.exists/save/open`, bounded hashing (64 MiB maximum), durable publication identity and transactional eligibility | Keep publication authority in Django; put byte operations behind an explicit resource backend |
| `artifact_publish.py::_ingest_artifact_library_copy` | Copies the published Artifact into the user's library through local storage | Include this copy in backend integration; uploading an object alone does not complete publication |
| `app_core/http/downloads.py::_select_artifact_download` | Resolves published Artifact and rechecks user/Session access | Authorize before opening bytes; bucket prefixes do not confer access |
| `app_core/http/storage_stream.py` | Bounded lanes, 64 KiB chunks, cancellation closes handles, missing storage currently returns 409 | Preserve streaming and cleanup; existing public download does **not** implement HTTP Range |
| `app_core/deleted_resource_gc.py` | Ownership/generation/lease checks before deletion; library orphan cleanup also lists directories | Delete recorded resources only after eligibility checks; bucket listing is not a business-fact reconstruction mechanism |
| `app_core/assets.py::store_immutable_bytes_at_key` | Local path, temporary file, fsync and exclusive hard link | Protects evidence/snapshot writes; Artifact publication does not use this helper. An S3 settings swap cannot reproduce these filesystem operations |
| `app_core/http/internal.py` | Snapshot local paths and hard links | Outside the Artifact slice |
| Plugin installation | Filesystem paths and rename/replace | Executable installation remains a filesystem concern |
| Hosted transcript captures/chunks | Fixed event association and bytes in PostgreSQL | No object-store migration or fallback in Contract and isolated validation |

Five characterization tests in `app_core/test_artifact_storage_contract.py` cover
concurrent immutable-helper writes, conflicting bytes, Django alternate-name
save behavior, missing/corrupt/idempotent deletion, and truncated/hash-invalid
uploads. They do not imply that Artifact already uses the immutable helper or
that its HTTP endpoint supports Range.

## Proposed contract

Persist a resource-specific reference with explicit backend identity, key, byte
length, application SHA-256 and fixed backend version when using versioning.
Bucket configuration and credentials stay server-side. Any new public fields
use exact camelCase; the probe's Python dataclass is not a public schema.

1. **Create or verify:** use a stable application upload/publication identity.
   Conditional creation cannot overwrite a winner. Same-key retries verify size
   and digest before reuse; different bytes conflict. ETag is an opaque
   conditional-request token, never an application SHA-256.
2. **Read the recorded version:** never resolve a historical reference through
   the latest object at that key. Bound reads, close streams, reject incomplete
   and incorrectly bounded responses. Range is a backend proposal, not a public
   API addition. Version plus Content-Range/length does not independently verify
   every range's digest; transport integrity or chunk digests need separate tests.
3. **Distinguish outcomes:** known key/version absence is missing; 401/403 is
   denial; conditional failure is conflict; transport/server failure is
   unavailable. Missing bucket, denial and timeout never prove resource deletion.
   These internal categories do not change existing API status codes.
4. **Publish in Django:** upload success means bytes exist, not business
   publication. Recheck ownership, generation, publication identity and deletion
   state in the existing transaction before exposing the verified reference.
   Reuse `ArtifactPublication`; PostgreSQL and object storage share no transaction.
5. **Delete precisely:** application tombstone/revocation comes first. Background
   GC rechecks references and active work, then deletes the exact version.
   Repeated deletion is safe; stale GC cannot delete a newer version. Orphans
   require recoverable cleanup records, not blind prefix deletion.

Binary Office/PDF contents remain on the Artifact/material file channel. This
does not enlarge the 50 KiB tool-text boundary or put binary data into SSE text.

## Reproducible isolated experiment

`scripts/artifact_storage_probe.py` is evaluation-only: boto3 1.43.97 with its
standalone uv lock (botocore 1.43.104), no production dependency or application
import. With Docker and uv, choose a new evidence directory outside the repo:

```powershell
docker pull --platform linux/amd64 rustfs/rustfs@sha256:ba0a1b53e36f321c0d46f3867104abef169f7bc59c467c664ddac87e7ddc9a8b
uv run --locked --script scripts/artifact_storage_probe.py --evidence D:/Projects/centaeris-storage-evidence/my-new-run
```

The probe creates a fresh Compose project, synthetic credentials, private network
and data volume, with a random host port bound only on 127.0.0.1. It refuses
existing project resources, removes its project/volume afterwards, redacts log
credentials and compares unrelated running container IDs. Image and SDK caches
remain. Killing the process can prevent `finally` cleanup: inspect the recorded
project before manual cleanup.

Limits: 1 CPU, 1 GiB, 128 PIDs, four concurrent requests; SDK connect/read timeout
2/3 seconds and one attempt; readiness deadline 60 seconds; each Docker command
timeout 60 seconds. Largest object: 5 MiB + 4 bytes; synthetic payloads total
less than 64 MiB. There is no filesystem quota or whole-process watchdog: this
is bounded input testing, not disk exhaustion or process-hang acceptance.
Bucket versioning is explicitly enabled; null version IDs fail validation.

| Actual SDK/HTTP case | Result |
| --- | --- |
| Empty, UTF-8 and binary round trips | Passed |
| Four concurrent same-key/same-content conditional writes; differing bytes | Passed; one shared version, conflict rejected |
| Retry after deliberately disregarding successful PUT response | Passed; same version reused |
| First/last byte and 64 KiB Range, invalid interval, wrong If-Match | Passed |
| Same-key AAA → BBB; old reference reads AAA; repeated old-version deletion preserves BBB | Passed |
| Wrong credentials versus known missing key | Passed; distinct outcomes |
| Multipart completion, opaque ETag, abort and abandoned-part listing | Passed |
| Incomplete HTTP PUT body | Passed; no completed object visible |
| Outage, restart and persisted fixed-version read | Passed after probe endpoint fix |

The first run passed eight cases but timed out after restart: Docker reassigned
the published port and the probe reused its old client. A regression test now
requires port discovery and client recreation after restart. The second run
observed port 57646 → 57410 and passed all nine cases. Both receipts remain
outside the repository; the successful receipt records source/lock hashes,
SDK versions, image identity, timings, resource removal and container comparison.

Final review added guards for ambiguous HEAD 404 (unavailable, not proven missing)
and differing-length conflicts. After those changes the final probe again passed
all nine cases. Five service-free probe guards now protect isolation, endpoint
rediscovery and outcome classification; five API characterization tests protect
the existing local primitives. The complete local CI passed (547 API tests,
118 Web tests); the scripts gate was rerun after the final probe adjustments.

The response-loss case models an unobserved committed response, not a proxy
dropping packets. Restart is single-node orderly stop/start, not power loss.
These results do not certify disk-full behavior, tenant IAM policy, production
TLS, arbitrary corruption detection, backup restore, high availability,
sustained performance or full S3 compatibility.

Primary references: [RustFS compatibility](https://docs.rustfs.com/en/reference/s3-compatibility),
[CLI configuration](https://docs.rustfs.com/en/reference/cli),
[1.0.0 announcement](https://rustfs.com/blog/announcing-rustfs-1-0-0-ga/),
[boto3 release](https://pypi.org/project/boto3/1.43.97/).
Vendor compatibility claims do not replace the pinned-image experiment.

## Decision and next boundary

Contract and isolated validation support an **Artifact-only backend implementation proposal**. This does not
justify replacing global `default_storage`, deploying RustFS or migrating data.
Keep the local backend and existing records.

Backend integration covers explicit backend/version metadata and tested forward migration,
Artifact publication plus the library copy, authorized streaming downloads and
reference-aware GC. Add failing tests for DB rollback after upload, lost
publication response, deletion versus late upload, and cleanup versus successful
publication. Bound memory: do not promote the probe's in-memory byte API into
production.

Existing-object migration covers copy/verify/switch migration and rollback records. Operational acceptance covers quotas,
recovery, backup/restore, upgrades and load. Resource expansion decides whether other resources
benefit. Each stage retains its own acceptance boundary.
