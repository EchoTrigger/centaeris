-- Resolve only the versioned Session baseline. Download and commit retain
-- their own current-authority checks when they touch snapshot storage.
WITH requested AS (
    SELECT $1::text AS run_id, $2::text AS authorization_digest,
           $3::text AS authorization_signature, $4::text::jsonb AS authorization_payload,
           $5::text AS lease_owner, $6::text AS environment_id,
           $7::bigint AS controller_epoch
), /* native_run_execution_authority */, baseline AS (
    SELECT current_run.id AS run_id, session."workspaceBaselineKnown" AS known,
           session."workspaceLastAdvancedAgentRun_id" AS last_advanced_run,
           session."workspaceGeneration" AS generation,
           jsonb_build_object(
               'generation', session."workspaceGeneration",
               'snapshotSha256', session."workspaceSnapshotSha256",
               'snapshotSizeBytes', session."workspaceSnapshotSizeBytes",
               'expandedSizeBytes', session."workspaceExpandedSizeBytes",
               'fileCount', session."workspaceFileCount"
           ) AS current_workspace,
           current_run.authorization_payload->'sessionWorkspace' AS signed_workspace
    FROM current_run
    JOIN public.app_core_session AS session ON session.id = current_run.session_id
    CROSS JOIN requested
    WHERE session.status = 'active' AND session."purgedAt" IS NULL
      AND session.current_environment_id = requested.environment_id
      AND current_run.job_status = 'running'
    FOR SHARE OF session NOWAIT
)
SELECT CASE
           WHEN NOT known THEN 'baseline_unknown'
           WHEN last_advanced_run = run_id AND generation = (signed_workspace->>'generation')::numeric + 1
               THEN 'advanced'
           WHEN current_workspace <> signed_workspace THEN 'baseline_conflict'
           WHEN signed_workspace->'snapshotSizeBytes' = '0'::jsonb THEN 'empty'
           ELSE 'download'
       END AS disposition,
       current_workspace::text, signed_workspace::text
FROM baseline
