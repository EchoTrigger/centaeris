-- The caller has verified the signed Run grant. Compare it to the current row
-- and re-read mutable authority on every dispatch. Legacy results grant nothing.
WITH requested AS (
    SELECT $1::text AS run_id, $2::text AS authorization_digest,
           $3::text AS authorization_signature, $4::text::jsonb AS authorization_payload,
           $5::text AS lease_owner, $6::text::jsonb AS inputs,
           $7::text AS environment_id, $8::bigint AS controller_epoch
), current_run AS (
    SELECT run.id, session.id AS session_id, run.user_id,
           session.status = 'active' AND owner.is_active AS library_session_active,
           COALESCE(material_scope.personal_library, false) AS personal_library,
           grant_row.payload->'assetRefs' AS declared_inputs, requested.inputs
    FROM requested
    JOIN public.app_core_agentrun AS run ON run.id = requested.run_id
    JOIN public.app_core_agentrunauthorization AS grant_row
      ON grant_row.agent_run_id = run.id
     AND grant_row.digest = requested.authorization_digest
     AND grant_row.signature = requested.authorization_signature
     AND grant_row.payload = requested.authorization_payload
    JOIN public.app_core_workspace AS workspace
      ON workspace.id = run.workspace_id AND workspace.status = 'active'
    JOIN public.app_core_workspacemembership AS membership
      ON membership.id = run.membership_ref AND membership.workspace_id = workspace.id
     AND membership.user_id = run.user_id AND membership.role IN ('owner', 'admin', 'member')
    JOIN public.app_core_session AS session
      ON session.id = run.session_id AND session.workspace_id = workspace.id
     AND session.owner_id = run.user_id AND session.purpose = 'interactive'
     AND session.current_environment_id = run.environment_id
    JOIN public.auth_user AS owner ON owner.id = session.owner_id
    JOIN public.app_core_sessionenvironment AS environment
      ON environment.id = run.environment_id AND environment.id = requested.environment_id
     AND environment.session_id = session.id AND environment.controller_epoch = requested.controller_epoch
     AND environment.controller_run_id = run.id AND environment.controller_lease_owner = requested.lease_owner
     AND environment.controller_purpose = 'execute' AND NOT environment.controller_released
    JOIN runtime.runtime_jobs AS job
      ON job.job_id = 'agent_run.lifecycle:' || run.id
     AND job.job_kind = 'agent_run.lifecycle' AND job.lease_purpose = 'execute'
     AND job.status IN ('leased', 'running') AND job.lease_owner = requested.lease_owner
     AND job.session_id = session.id AND job.idempotency_key = job.job_id || ':' || grant_row.digest
     AND job.payload_ref = 'record:agent_run:' || run.id
     AND job.lease_expires_at_ms > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint
    JOIN runtime.execution_job_tenants AS tenant ON tenant.job_id = job.job_id AND tenant.workspace_id = workspace.id
    LEFT JOIN public.app_core_runmaterialscope AS material_scope ON material_scope.agent_run_id = run.id
    WHERE run.status IN ('queued', 'running') AND run."preAdmissionCancelledAt" IS NULL
      AND grant_row.payload->>'schema' = 'workspace.agent_run_authorization.v2'
      AND grant_row.payload->>'id' = grant_row.id
      AND grant_row.payload->>'agentRunId' = run.id
      AND grant_row.payload->>'sessionId' = session.id
      AND grant_row.payload->>'workspaceId' = workspace.id
      AND grant_row.payload->>'userId' = run.user_id::text
      AND grant_row.payload->>'modelConfigRef' = run."modelConfig_id"
      AND NULLIF(run."thinkingMode", '') IS NOT DISTINCT FROM grant_row.payload->>'thinkingMode'
      AND grant_row.payload->'environmentProfile' = environment.environment_profile
      AND grant_row.payload->>'environmentProfileDigest' = environment.environment_profile_digest
      AND grant_row.payload->'environmentProfile'->>'network' = environment.network
      AND NOT EXISTS (
          SELECT 1 FROM runtime.runtime_events AS control_fact
          WHERE (control_fact.event_id, control_fact.event_type) IN (
              ('agent_run_cancel_requested:' || run.id, 'runtime.agent_run.cancel_requested.v1'),
              ('agent_run_pre_admission_failure:' || run.id, 'runtime.agent_run.pre_admission_failure.v1')
          )
      )
      AND NOT EXISTS (
          SELECT 1 FROM public.app_core_sessionevent AS terminal
          WHERE terminal.workspace_id = workspace.id AND terminal.session_id = session.id
            AND terminal.agent_run_id = run.id AND NOT terminal.session_level
            AND terminal.payload->>'type' IN ('agent_run_completed', 'agent_run_failed', 'agent_run_interrupted')
      )
)
SELECT input.descriptor->>'inputRef' AS input_ref,
       CASE
           -- Only confirmed, canonical personal Library note originals use this path.
           WHEN record.id IS NULL OR note.id IS NULL OR note."objectKind" <> 'note'
             OR (declared.descriptor IS NOT NULL AND
                 declared.descriptor->'inputIdentity'->>'ownerKind' <> 'userLibraryObject') THEN 'legacy'
           WHEN NOT current_run.library_session_active
             OR (declared.descriptor IS NULL AND NOT current_run.personal_library) THEN 'access_revoked'
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
             OR octet_length(convert_to(head.markdown, 'UTF8')) IS DISTINCT FROM head."sizeBytes"
             OR 'sha256:' || encode(sha256(convert_to(head.markdown, 'UTF8')), 'hex') IS DISTINCT FROM head.sha256
             THEN 'invalid'
           WHEN record.resolved->'inputRef' IS DISTINCT FROM input.descriptor->'inputRef'
             OR record.resolved->'virtualPath' IS DISTINCT FROM input.descriptor->'virtualPath'
             OR record.resolved->'sizeBytes' IS DISTINCT FROM input.descriptor->'sizeBytes'
             OR record.resolved->'sha256' IS DISTINCT FROM input.descriptor->'sha256'
             OR record.resolved->'sourceVersion' IS DISTINCT FROM input.descriptor->'sourceVersion' THEN 'stale_generation'
           ELSE 'active'
       END AS state
FROM current_run
CROSS JOIN LATERAL jsonb_array_elements(current_run.inputs) WITH ORDINALITY AS input(descriptor, ordinal)
LEFT JOIN public.app_core_sessionmaterialinput AS record
  ON record.session_id = current_run.session_id AND record.input_ref = input.descriptor->>'inputRef'
 AND record.confirmed AND record.resolved->>'ownerKind' = 'userLibraryObject'
 AND record.input_ref = 'library:' || (record.resolved->>'objectRef') || ':'
                        || (record.resolved->>'sourceVersion') || ':' || substring(record.resolved->>'sha256' FROM 8)
LEFT JOIN public.app_core_userlibraryobject AS note
  ON note.id = record.resolved->>'objectRef' AND note.owner_id = current_run.user_id
LEFT JOIN public.app_core_librarynoterevision AS head ON head.id = note."noteHead_id"
LEFT JOIN LATERAL (
    SELECT descriptor FROM jsonb_array_elements(current_run.declared_inputs) AS entry(descriptor)
    WHERE descriptor->>'inputRef' = input.descriptor->>'inputRef'
) AS declared ON true
ORDER BY input.ordinal
