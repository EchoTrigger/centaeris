# Upload and temporary-space limits

The API checks ordinary multipart upload bytes before Django's initial body spool. Multipart
parsing checks each file before a view can publish any part, including a late
oversized part following valid files. Content-Length and uploaded size metadata
do not substitute for counting received bytes. The existing library batch count
of 50 files remains unchanged.

## Deployment inputs

| Setting | Policy |
| --- | --- |
| `UPLOAD_FILE_MAX_BYTES` | Defaults to 67108864 (64 MiB). This is a new general single-file policy using the existing Artifact protocol ceiling as its reference; it is not evidence that library/source uploads previously had that limit. |
| `UPLOAD_BODY_MAX_BYTES` | Required positive public HTTP envelope size in bytes. No production default. Includes multipart framing, form fields and all files together. |
| `UPLOAD_TEMP_MAX_BYTES` | Required positive shared spool-byte reservation budget. No production default. Must fit two copies of the public envelope. |
| `UPLOAD_MAX_CONCURRENT` | Required positive count of simultaneous ordinary multipart upload requests across API replicas. No production default. |

Choose the envelope for supported payloads and their actual framing. There is no
independent 128 MiB batch ceiling or invented fixed framing allowance. The gateway
renders its finite body cap from the same `UPLOAD_BODY_MAX_BYTES` input. A lower
envelope than the single-file ceiling makes the envelope the effective limit.
All API replicas sharing the database must use the same limits.

Compose mounts a dedicated `upload-temp` volume at
`/var/lib/centaeris-upload-temp` and passes that path as `UPLOAD_TEMP_ROOT` to
the API and its initialization/maintenance processes. It must be disjoint from
the retained storage root and free of aliases, symlinks and reparse points.

## What the budget bounds

Each admitted ordinary multipart request durably holds twice its declared body length, or twice its
maximum when length is unknown. This covers the overlapping initial body spool
and multipart file copies. False lengths and actual overflow are rejected before
the overflowing chunk reaches the spool. Non-multipart traffic, including ordinary
JSON/control requests, bypasses this upload pool and creates no upload lease or
counter/database/filesystem work in the ingress adapter. An empty multipart body releases
its temporary hold before a potentially long response.

The exact internal workspace commit and execution stage paths are excluded,
including when presented with a multipart content type. Their original Django
body spool, view authentication and issued signed authorization validation remain
unchanged. Upload-pool saturation cannot reject a final snapshot or a lifecycle
control request. Snapshot staging remains on the retained storage filesystem;
its transport and persistent capacity policies are deferred, and this temporary
volume does not promise final-save capacity. Other non-multipart body spools also
remain governed by the original Django transport.

These counters bound spool payload envelopes, not every physical disk byte.
Request markers, directory entries, inodes, filesystem allocation overhead,
database/WAL growth and operational headroom need separate physical constraints.
Provision a hard volume/filesystem limit and adequate space/inodes explicitly.
An application budget does not replace those deployment limits.

## Cleanup and recovery

Every request has a unique directory with a pool/lease identity marker. The
initial spool and multipart handles are registered with its tracker. A normal
response is not cleanup proof: Django can swallow close failures. The API returns
capacity only after it confirms that all tracked handles closed, all exact owned
temporary files were deleted and the request directory is absent. An application
error with that same confirmed cleanup can release capacity too. Uncertain
cleanup retains its byte/slot hold across restart; elapsed time never refunds it.
Confirmed request liabilities are removed in the same transaction that reduces
the counters. Duplicate releases cannot refund a different active request;
completed multipart requests do not accumulate historical lease records.

Oversized content receives 413. Exhausted temporary byte capacity or upload slots
receive 429 with `upload_capacity_exhausted`. These rejections apply only to
ordinary multipart uploads; even a full upload byte/slot budget leaves control
JSON and internal snapshot requests on their original transport path.

After stopping every API writer sharing the pool, run:

```sh
python manage.py reconcile_upload_capacity --api-workers-stopped
```

The command validates the current pool marker, persisted lease identities and
exact managed filenames before removing anything. An unknown object, alias,
marker mismatch or deletion failure keeps the corresponding capacity occupied.
It does not recursively delete a general temporary directory, another pool or
retained customer files. Restore a mismatched pool's original identity and
investigate unrecognized files before attempting recovery; do not reset counters
or erase unknown leases to regain capacity.

`0011_upload_capacity` follows `0010_active_run_admission`. Reverse migration
refuses to erase any active/unknown lease or nonzero counter. Keep a consistent
database and pool backup; stop writers and complete verified offline cleanup
before a schema rollback. A code rollback can retain this expanded schema.
