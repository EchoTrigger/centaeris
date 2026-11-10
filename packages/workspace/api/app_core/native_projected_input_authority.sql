-- The caller has verified the signed Run grant. Compare it to the current row
-- and re-read mutable authority on every dispatch. Legacy results grant nothing.
WITH requested AS (
    SELECT $1::text AS run_id, $2::text AS authorization_digest,
           $3::text AS authorization_signature, $4::text::jsonb AS authorization_payload,
           $5::text AS lease_owner, $6::text AS environment_id,
           $7::bigint AS controller_epoch, $8::text::jsonb AS inputs
), /* native_run_execution_authority */
SELECT input.descriptor->>'inputRef' AS input_ref,
       CASE
           -- Only confirmed, canonical personal Library note originals use this path.
           WHEN record.id IS NULL OR note.id IS NULL OR note."objectKind" <> 'note'
             OR (declared.descriptor IS NOT NULL AND
                 declared.descriptor->'inputIdentity'->>'ownerKind' <> 'userLibraryObject') THEN 'legacy'
           WHEN NOT EXISTS (
               SELECT 1 FROM pg_catalog.pg_constraint
               WHERE conrelid = 'public.app_core_librarynoterevision'::pg_catalog.regclass
                 AND conname = 'note_revision_body_identity' AND contype = 'c' AND convalidated
           ) THEN 'legacy'
           WHEN NOT (current_run.session_status = 'active' AND owner.is_active)
             OR (declared.descriptor IS NULL AND NOT COALESCE(material_scope.personal_library, false)) THEN 'access_revoked'
           WHEN note.status = 'deleted' THEN 'asset_removed'
           WHEN note.status <> 'ready' THEN 'access_revoked'
           WHEN note."contentGeneration"::text IS DISTINCT FROM record.resolved->>'sourceVersion'
             OR note.sha256 IS DISTINCT FROM record.resolved->>'sha256'
             OR to_jsonb(note."sizeBytes") IS DISTINCT FROM record.resolved->'sizeBytes' THEN 'stale_generation'
           WHEN declared.descriptor IS NOT NULL AND (
                 declared.descriptor->'inputIdentity'->>'ownerId' IS DISTINCT FROM note.id
              OR declared.descriptor->'inputIdentity'->>'generation' IS DISTINCT FROM note."contentGeneration"::text
              OR declared.descriptor->'inputIdentity'->>'sha256' IS DISTINCT FROM note.sha256
           ) THEN 'stale_generation'
           -- Without a revision, Django resolves through external storage.
           WHEN note."noteHead_id" IS NULL THEN 'legacy'
           WHEN head.note_id IS DISTINCT FROM note.id
             OR head.generation IS DISTINCT FROM note."contentGeneration"
             OR head.sha256 IS DISTINCT FROM note.sha256
             OR head."sizeBytes" IS DISTINCT FROM note."sizeBytes"
             OR head."displayName" IS DISTINCT FROM note."displayName"
             THEN 'invalid'
           WHEN record.resolved->'inputRef' IS DISTINCT FROM input.descriptor->'inputRef'
             OR record.resolved->'virtualPath' IS DISTINCT FROM input.descriptor->'virtualPath'
             OR record.resolved->'sizeBytes' IS DISTINCT FROM input.descriptor->'sizeBytes'
             OR record.resolved->'sha256' IS DISTINCT FROM input.descriptor->'sha256'
             OR record.resolved->'sourceVersion' IS DISTINCT FROM input.descriptor->'sourceVersion' THEN 'stale_generation'
           ELSE 'active'
       END AS state
FROM current_run
JOIN public.auth_user AS owner ON owner.id = current_run.user_id
LEFT JOIN public.app_core_runmaterialscope AS material_scope ON material_scope.agent_run_id = current_run.id
CROSS JOIN requested
CROSS JOIN LATERAL jsonb_array_elements(requested.inputs) WITH ORDINALITY AS input(descriptor, ordinal)
LEFT JOIN public.app_core_sessionmaterialinput AS record
  ON record.session_id = current_run.session_id AND record.input_ref = input.descriptor->>'inputRef'
 AND record.confirmed AND record.resolved->>'ownerKind' = 'userLibraryObject'
 AND record.input_ref = 'library:' || (record.resolved->>'objectRef') || ':'
                        || (record.resolved->>'sourceVersion') || ':' || substring(record.resolved->>'sha256' FROM 8)
LEFT JOIN public.app_core_userlibraryobject AS note
  ON note.id = record.resolved->>'objectRef' AND note.owner_id = current_run.user_id
LEFT JOIN public.app_core_librarynoterevision AS head ON head.id = note."noteHead_id"
LEFT JOIN LATERAL (
    SELECT descriptor FROM jsonb_array_elements(current_run.authorization_payload->'assetRefs') AS entry(descriptor)
    WHERE descriptor->>'inputRef' = input.descriptor->>'inputRef'
) AS declared ON true
ORDER BY input.ordinal
