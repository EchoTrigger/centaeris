-- The Runtime obtains and audits the bearer through Django connect. This fresh
-- dispatch witness reports only the exact workspace credential identity.
WITH requested AS (
    SELECT $1::text AS run_id, $2::text AS authorization_digest,
           $3::text AS authorization_signature, $4::text::jsonb AS authorization_payload,
           $5::text AS lease_owner, $6::text AS environment_id,
           $7::bigint AS controller_epoch, $8::text AS plugin_name,
           $9::text AS resource_path, $10::text AS resource_digest,
           $11::text AS credential_ref
), /* native_run_execution_authority */
SELECT credential.id::text AS credential_id, credential.version::bigint AS credential_version
FROM current_run
CROSS JOIN requested
JOIN public.app_core_workspacepluginenablement AS enablement
  ON enablement.workspace_id = current_run.workspace_id AND enablement."pluginName" = requested.plugin_name
JOIN public.app_core_mcp_bearer_credential AS credential
  ON credential.workspace_id = current_run.workspace_id AND credential.plugin_name = requested.plugin_name
 AND credential.credential_ref = requested.credential_ref AND credential.version > 0
WHERE current_run.session_status = 'active'
  AND current_run.acting_app_id IS NULL AND current_run.app_delegation_id IS NULL
  AND EXISTS (
      SELECT 1
      FROM jsonb_array_elements(current_run.authorization_payload->'pluginActivation'->'packages') AS signed(package)
      WHERE signed.package->>'name' = requested.plugin_name
        AND jsonb_array_length(signed.package->'mcpServers') = 1
        AND signed.package->'mcpServers'->0->>'path' = requested.resource_path
        AND signed.package->'mcpServers'->0->>'digest' = requested.resource_digest
  )
