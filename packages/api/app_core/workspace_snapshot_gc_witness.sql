-- Shared relational witness: opaque owner IDs and hashes, never authority bodies.
-- Runtime alone applies Core checkpoint reachability semantics.
WITH owner AS (
 SELECT id,workspace_id FROM public.app_core_session WHERE id=$1::text
), candidates AS MATERIALIZED (
 SELECT r.id FROM public.app_core_agentrun r
 LEFT JOIN runtime.runtime_jobs j ON j.job_id='agent_run.lifecycle:'||r.id
 WHERE r.session_id=(SELECT id FROM owner) AND (
  r.status IN('queued','running') OR j.status IN('queued','leased','running')
  OR EXISTS(SELECT 1 FROM public.app_core_sessionenvironment e
   WHERE e.id=r.environment_id AND e.controller_run_id=r.id AND NOT e.controller_released)
  OR EXISTS(SELECT 1 FROM runtime.resident_sandboxes s
   JOIN runtime.execution_instances i ON i.execution_id=s.execution_id AND i.instance_id=s.instance_id
   JOIN public.app_core_sessionenvironment e ON e.id=s.environment_id
   WHERE i.agent_run_id=r.id AND e.session_id=r.session_id AND NOT e.controller_released)
  OR EXISTS(SELECT 1 FROM runtime.resident_sandboxes s
   LEFT JOIN runtime.execution_instances i ON i.execution_id=s.execution_id AND i.instance_id=s.instance_id
   WHERE s.environment_id=r.environment_id AND (s.agent_run_id=r.id OR i.agent_run_id=r.id)
   AND (s.state<>'resident' OR s.container_id IS NULL OR s.allocation_mode='unbound'))
  OR EXISTS(SELECT 1 FROM runtime.environment_creation_attempts c
   JOIN runtime.execution_instances i ON i.execution_id=c.allocation_execution_id
   LEFT JOIN runtime.resident_sandboxes s ON s.execution_id=i.execution_id AND s.instance_id=i.instance_id
   WHERE i.agent_run_id=r.id AND c.state IN('prepared','dispatched')
   AND (s.execution_id IS NULL OR s.state<>'resident' OR s.container_id IS NULL)
   AND NOT EXISTS(SELECT 1 FROM runtime.agent_run_cleanup_receipts p WHERE p.instance_id=i.instance_id
    AND p.environment_id=i.environment_id AND p.cleanup_complete AND p.physical_removed)
   AND NOT EXISTS(SELECT 1 FROM runtime.environment_retirements t WHERE t.instance_id=i.instance_id
    AND t.environment_id=i.environment_id AND t.state='destroyed')))
 ORDER BY r.id LIMIT 257
)
SELECT COALESCE((SELECT MAX(sequence) FROM public.app_core_sessionevent
 WHERE session_id=owner.id),0)::bigint AS session_sequence,
 jsonb_build_object(
  'protectedAgentRunIds',COALESCE((SELECT jsonb_agg(id ORDER BY id) FROM candidates),'[]'::jsonb),
  'runsOverflow',(SELECT count(*)>256 FROM candidates),
  'session',(SELECT encode(sha256(convert_to((to_jsonb(s)-'updatedAt')::text,'UTF8')),'hex')
   FROM public.app_core_session s WHERE s.id=owner.id),
  'runs',COALESCE((SELECT jsonb_agg(jsonb_build_array(r.id,
   encode(sha256(convert_to((to_jsonb(r)-'updatedAt')::text,'UTF8')),'hex'),a.digest,
   encode(sha256(convert_to(a.payload::text,'UTF8')),'hex'),
   encode(sha256(convert_to(jsonb_build_array(j.job_id,j.job_kind,j.status,j.session_id,
    j.payload_ref,j.idempotency_key,j.lease_purpose,j.lease_control_stage)::text,'UTF8')),'hex')) ORDER BY r.id)
   FROM public.app_core_agentrun r JOIN candidates c ON c.id=r.id
   LEFT JOIN public.app_core_agentrunauthorization a ON a.agent_run_id=r.id
   LEFT JOIN runtime.runtime_jobs j ON j.job_id='agent_run.lifecycle:'||r.id
   ),'[]'::jsonb),
  'environments',COALESCE((SELECT jsonb_agg(jsonb_build_array(e.id,
   encode(sha256(convert_to(to_jsonb(e)::text,'UTF8')),'hex')) ORDER BY e.id)
   FROM public.app_core_sessionenvironment e WHERE e.session_id=owner.id),'[]'::jsonb),
  'instances',COALESCE((SELECT jsonb_agg(jsonb_build_array(i.execution_id,
   encode(sha256(convert_to(to_jsonb(i)::text,'UTF8')),'hex')) ORDER BY i.execution_id)
   FROM runtime.execution_instances i WHERE i.session_id=owner.id),'[]'::jsonb),
  'residents',COALESCE((SELECT jsonb_agg(jsonb_build_array(s.host_id,s.execution_id,
   encode(sha256(convert_to((to_jsonb(s)-'updated_at_ms')::text,'UTF8')),'hex')) ORDER BY s.host_id,s.execution_id)
   FROM runtime.resident_sandboxes s JOIN public.app_core_sessionenvironment e ON e.id=s.environment_id
   WHERE e.session_id=owner.id),'[]'::jsonb),
  'creations',COALESCE((SELECT jsonb_agg(jsonb_build_array(c.allocation_execution_id,
   encode(sha256(convert_to(to_jsonb(c)::text,'UTF8')),'hex')) ORDER BY c.allocation_execution_id)
   FROM runtime.environment_creation_attempts c JOIN runtime.execution_instances i ON i.execution_id=c.allocation_execution_id
   WHERE i.session_id=owner.id),'[]'::jsonb),
  'preAdmissionFailures',COALESCE((SELECT jsonb_agg(jsonb_build_array(f.event_id,
   encode(sha256(convert_to(to_jsonb(f)::text,'UTF8')),'hex')) ORDER BY f.event_id)
   FROM runtime.runtime_events f WHERE f.session_id=owner.id
   AND f.event_type='runtime.agent_run.pre_admission_failure.v1'),'[]'::jsonb),
  'checkpointFacts',COALESCE((SELECT jsonb_agg(jsonb_build_array(c.checkpoint_id,c.kind,c.status,
   c.done_reason,c.updated_at_ms,encode(sha256(convert_to(c.payload_json,'UTF8')),'hex'),
   encode(sha256(convert_to(COALESCE(c.wait_handoff_json,''),'UTF8')),'hex')) ORDER BY c.checkpoint_id)
   FROM runtime.checkpoints c WHERE c.session_id=owner.id AND c.kind='recovery'
   AND c.payload_json::jsonb->>'agentRunId' IN(SELECT id FROM candidates)),'[]'::jsonb),
  'semanticSnapshot',(SELECT encode(sha256(convert_to(snapshot_json,'UTF8')),'hex')
   FROM runtime.session_runtime_snapshots WHERE session_id=owner.id)
 )::text AS authority_json
FROM owner;
