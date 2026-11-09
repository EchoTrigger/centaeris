WITH requested AS (
    SELECT $1::text AS run_id, $2::text AS authorization_ref, $3::text AS authorization_digest,
           $4::text AS lease_owner, $5::text AS provider_id
)
SELECT jsonb_build_object(
    'agentRunId', run.id,
    'authorizationRef', grant_row.id,
    'authorizationDigest', grant_row.digest,
    'leaseOwner', job.lease_owner,
    'workspaceId', workspace.id,
    'sessionId', session.id,
    'environmentId', environment.id,
    'modelConfigRef', model.id,
    'providerId', provider.id,
    'apiBase', model."resolvedApiBase",
    'model', model."modelName",
    'contextTokens', model."contextTokens",
    'maxOutputTokens', model."maxOutputTokens",
    'thinkingMode', grant_row.payload->>'thinkingMode',
    'thinkingModes', model."thinkingModes",
    'credentialId', credential.id,
    'credentialVersion', credential.version
)::text
FROM requested
JOIN public.app_core_agentrun AS run ON run.id = requested.run_id
JOIN public.app_core_agentrunauthorization AS grant_row
  ON grant_row.agent_run_id = run.id
 AND grant_row.id = requested.authorization_ref
 AND grant_row.digest = requested.authorization_digest
JOIN public.app_core_workspace AS workspace
  ON workspace.id = run.workspace_id AND workspace.status = 'active'
JOIN public.app_core_workspacemembership AS member
  ON member.id = run.membership_ref AND member.workspace_id = workspace.id
 AND member.user_id = run.user_id AND member.role IN ('owner', 'admin', 'member')
JOIN public.app_core_session AS session
  ON session.id = run.session_id AND session.workspace_id = workspace.id
 AND session.owner_id = run.user_id AND session.purpose = 'interactive'
 AND session."deletedAt" IS NULL AND session.current_environment_id = run.environment_id
JOIN public.app_core_sessionenvironment AS environment
  ON environment.id = run.environment_id AND environment.session_id = session.id
 AND environment.controller_run_id = run.id AND environment.controller_lease_owner = requested.lease_owner
 AND environment.controller_purpose = 'execute' AND NOT environment.controller_released
JOIN public.app_core_modelconfig AS model ON model.id = run."modelConfig_id"
JOIN public.app_core_modelprovider AS provider
  ON provider.id = model.provider_id AND provider.id = requested.provider_id
 AND provider.workspace_id = workspace.id AND provider.enabled AND provider."archivedAt" IS NULL
 AND provider.template_id IS DISTINCT FROM 'opencode_zen_go'
JOIN public.app_core_providercredential AS credential ON credential.provider_id = provider.id
JOIN runtime.runtime_jobs AS job
  ON job.job_id = 'agent_run.lifecycle:' || run.id
 AND job.job_kind = 'agent_run.lifecycle' AND job.lease_purpose = 'execute' AND job.status = 'running'
 AND job.lease_owner = requested.lease_owner AND job.session_id = session.id
 AND job.idempotency_key = job.job_id || ':' || grant_row.digest
 AND job.payload_ref = 'record:agent_run:' || run.id
 AND job.lease_expires_at_ms > (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint
WHERE run.status IN ('queued', 'running') AND run."preAdmissionCancelledAt" IS NULL
  AND model."resolvedApi" = 'openai-responses'
  AND grant_row.payload->>'schema' = 'workspace.agent_run_authorization.v2'
  AND grant_row.payload->>'id' = grant_row.id
  AND grant_row.payload->>'agentRunId' = run.id
  AND grant_row.payload->>'sessionId' = session.id
  AND grant_row.payload->>'workspaceId' = workspace.id
  AND grant_row.payload->>'userId' = run.user_id::text
  AND grant_row.payload->>'modelConfigRef' = model.id
  AND NULLIF(run."thinkingMode", '') IS NOT DISTINCT FROM grant_row.payload->>'thinkingMode'
  AND (grant_row.payload->>'thinkingMode' IS NULL OR model."thinkingModes" ? (grant_row.payload->>'thinkingMode'))
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
