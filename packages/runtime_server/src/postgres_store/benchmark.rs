//! Maintenance reads bind one signed benchmark Run to its recorded incarnation.
use super::PostgresRuntimeStore;
use crate::agent_run_authorization::{
    BenchmarkExecutionProfile, WorkspaceAgentRunAuthorization,
    BENCHMARK_AGENT_RUN_AUTHORIZATION_SCHEMA,
};

#[derive(Clone, Debug)]
pub(crate) struct BenchmarkAllocation {
    pub execution_id: String,
    pub agent_run_id: String,
    pub instance_id: String,
    pub host_id: String,
    pub container_id: Option<String>,
    pub binding_digest: String,
    pub retained_profile_digest: String,
    pub state: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct BenchmarkTarget {
    pub session_id: String,
    pub environment_id: String,
    pub agent_run_id: String,
    pub controller_epoch: i64,
    pub profile: BenchmarkExecutionProfile,
    pub profile_digest: String,
    pub allocation: Option<BenchmarkAllocation>,
    pub resource_holding: bool,
    pub retirement_state: Option<String>,
    pub setup_ready: bool,
}

struct GrantBinding<'a> {
    session: &'a str,
    run: &'a str,
    workspace: &'a str,
    user: &'a str,
    model: &'a str,
    thinking: Option<&'a str>,
    bound_profile: &'a str,
    bound_digest: &'a str,
}

fn checked_grant(
    payload: &str,
    digest: &str,
    signature: &str,
    signing_key: &[u8],
    binding: &GrantBinding<'_>,
) -> Result<WorkspaceAgentRunAuthorization, String> {
    let grant: WorkspaceAgentRunAuthorization =
        serde_json::from_str(payload).map_err(|_| "benchmark_grant_invalid")?;
    grant
        .checked_binding(binding.run)?
        .verify(digest, signing_key, signature)?;
    if grant.schema != BENCHMARK_AGENT_RUN_AUTHORIZATION_SCHEMA
        || grant.session_id != binding.session
        || grant.workspace_id != binding.workspace
        || grant.user_id != binding.user
        || grant.model_config_ref != binding.model
        || grant.thinking_mode.as_deref() != binding.thinking.filter(|value| !value.is_empty())
    {
        return Err("benchmark_grant_binding_rejected".into());
    }
    let profile: BenchmarkExecutionProfile =
        serde_json::from_str(binding.bound_profile).map_err(|_| "benchmark_profile_invalid")?;
    if grant.benchmark_profile.as_ref() != Some(&profile)
        || grant.benchmark_profile_digest.as_deref() != Some(binding.bound_digest)
    {
        return Err("benchmark_profile_binding_rejected".into());
    }
    Ok(grant)
}

impl PostgresRuntimeStore {
    /// Pin the Environment against retirement/controller mutation only through a
    /// bounded physical dispatch. Long exec output collection happens afterwards.
    pub(crate) fn with_benchmark_target<T: Send>(
        &self,
        session: &str,
        environment: &str,
        run: &str,
        signing_key: &[u8],
        operation: impl FnOnce(&BenchmarkTarget) -> Result<T, String> + Send,
    ) -> Result<T, String> {
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|_| "benchmark_store_unavailable")?;
            tx.batch_execute("SET LOCAL lock_timeout='1s'; SET LOCAL statement_timeout='5s'").map_err(|_| "benchmark_store_unavailable")?;
            let session_row = tx.query_opt("SELECT workspace_id,owner_id::text,agent_id,current_environment_id FROM public.app_core_session WHERE id=$1 FOR SHARE", &[&session]).map_err(|_| "benchmark_store_unavailable")?.ok_or("benchmark_session_missing")?;
            let env = tx.query_opt("SELECT session_id,controller_epoch,benchmark_profile::text,benchmark_profile_digest,benchmark_setup_completed_at IS NOT NULL,benchmark_setup_run_id,benchmark_setup_instance_id,benchmark_setup_container_id FROM public.app_core_sessionenvironment WHERE id=$1 FOR SHARE", &[&environment]).map_err(|_| "benchmark_store_unavailable")?.ok_or("benchmark_environment_missing")?;
            if env.get::<_, String>(0) != session || session_row.get::<_, Option<String>>(3).as_deref() != Some(environment) {
                return Err("benchmark_session_environment_rejected".into());
            }
            let run_row = tx.query_opt("SELECT a.session_id,a.environment_id,a.workspace_id,a.user_id::text,a.\"modelConfig_id\",a.\"thinkingMode\",g.payload::text,g.digest,g.signature FROM public.app_core_agentrun a JOIN public.app_core_agentrunauthorization g ON g.agent_run_id=a.id WHERE a.id=$1 FOR SHARE OF a,g", &[&run]).map_err(|_| "benchmark_store_unavailable")?.ok_or("benchmark_run_missing")?;
            let workspace: String = run_row.get(2);
            let user: String = run_row.get(3);
            if run_row.get::<_, String>(0) != session || run_row.get::<_, Option<String>>(1).as_deref() != Some(environment)
                || session_row.get::<_, String>(0) != workspace || session_row.get::<_, String>(1) != user
            {
                return Err("benchmark_run_binding_rejected".into());
            }
            let profile_json = env.get::<_, Option<String>>(2).ok_or("benchmark_profile_missing")?;
            let profile_digest: String = env.get(3);
            let grant = checked_grant(&run_row.get::<_, String>(6), &run_row.get::<_, String>(7), &run_row.get::<_, String>(8), signing_key,
                &GrantBinding {session,run,workspace:&workspace,user:&user,model:&run_row.get::<_, String>(4),thinking:run_row.get::<_, Option<String>>(5).as_deref(),bound_profile:&profile_json,bound_digest:&profile_digest})?;
            if grant.agent_id != session_row.get::<_, String>(2) {
                return Err("benchmark_agent_binding_rejected".into());
            }
            let profile = grant.benchmark_profile.ok_or("benchmark_profile_missing")?;
            let retirement = tx.query_opt("SELECT state,container_id,instance_id,host_id,allocation_execution_id FROM runtime.environment_retirements WHERE environment_id=$1", &[&environment]).map_err(|_| "benchmark_store_unavailable")?;
            let retirement_state = retirement.as_ref().map(|row| row.get::<_, String>(0));
            let origins = tx.query("SELECT DISTINCT allocation_execution_id FROM runtime.execution_instances WHERE session_id=$1 AND environment_id=$2 AND agent_run_id=$3 AND allocation_mode='retained' LIMIT 2", &[&session,&environment,&run]).map_err(|_| "benchmark_store_unavailable")?;
            if origins.len() > 1 { return Err("benchmark_incarnation_ambiguous".into()); }
            if origins.is_empty() && tx.query_one("SELECT EXISTS(SELECT 1 FROM runtime.resident_sandboxes WHERE environment_id=$1)", &[&environment]).map_err(|_|"benchmark_store_unavailable")?.get::<_,bool>(0) { return Err("benchmark_unbound_resource_holding".into()); }
            let mut resource_holding = false;
            let allocation = if let Some(origin) = origins.first() {
                let execution: String = origin.get(0);
                let a = tx.query_one("SELECT agent_run_id,instance_id,host_id,image_digest,authorization_digest,binding_digest,profile_digest FROM runtime.execution_instances WHERE execution_id=$1 AND allocation_execution_id=execution_id AND environment_id=$2 AND session_id=$3 AND allocation_mode='retained' FOR SHARE", &[&execution,&environment,&session]).map_err(|_| "benchmark_allocation_missing")?;
                if a.get::<_, String>(0) != run || a.get::<_, String>(3) != profile.image_digest || a.get::<_, String>(4) != run_row.get::<_, String>(7) {
                    return Err("benchmark_allocation_binding_rejected".into());
                }
                let resident = tx.query_opt("SELECT container_id,state,workspace_id,memory_bytes,cpu_milli,pids,workspace_bytes FROM runtime.resident_sandboxes WHERE execution_id=$1 AND instance_id=$2 AND environment_id=$3 AND host_id=$4 AND allocation_mode='retained' FOR SHARE", &[&execution,&a.get::<_, String>(1),&environment,&a.get::<_, String>(2)]).map_err(|_| "benchmark_store_unavailable")?;
                if let Some(row) = &resident {
                    resource_holding = true;
                    let r = profile.resources;
                    if row.get::<_, String>(2) != workspace || row.get::<_, i64>(3) != i64::try_from(r.memory_bytes).map_err(|_| "benchmark_resources_invalid")?
                        || row.get::<_, i64>(4) != i64::from(r.cpu_milli) || row.get::<_, i64>(5) != i64::from(r.pids_limit)
                        || row.get::<_, i64>(6) != i64::try_from(r.data_tmpfs_bytes).map_err(|_| "benchmark_resources_invalid")?
                    { return Err("benchmark_resource_binding_rejected".into()); }
                }
                let container_id = resident.as_ref().and_then(|row| row.get::<_, Option<String>>(0)).or_else(|| retirement.as_ref().map(|row| row.get(1)));
                Some(BenchmarkAllocation {execution_id: execution,agent_run_id:a.get(0),instance_id:a.get(1),host_id:a.get(2),container_id,binding_digest:a.get(5),retained_profile_digest:a.get(6),state:resident.as_ref().map(|row|row.get(1))})
            } else { None };
            let setup_ready = env.get::<_, bool>(4) && allocation.as_ref().is_some_and(|a| {
                env.get::<_, Option<String>>(5).as_deref() == Some(run) && env.get::<_, String>(6) == a.instance_id && Some(env.get::<_, String>(7)).as_ref() == a.container_id.as_ref()
            });
            if env.get::<_,bool>(4) && !setup_ready {return Err("benchmark_setup_binding_rejected".into());}
            if let Some(retired)=&retirement {
                if !allocation.as_ref().is_some_and(|a|a.container_id.as_deref()==Some(retired.get::<_,String>(1).as_str()) && a.instance_id==retired.get::<_,String>(2) && a.host_id==retired.get::<_,String>(3) && a.execution_id==retired.get::<_,String>(4)) {return Err("benchmark_retirement_binding_rejected".into());}
            }
            let target = BenchmarkTarget {session_id:session.into(),environment_id:environment.into(),agent_run_id:run.into(),controller_epoch:env.get(1),profile,profile_digest,allocation,resource_holding,retirement_state,setup_ready};
            let output = operation(&target)?;
            tx.commit().map_err(|_| "benchmark_dispatch_commit_unknown")?;
            Ok(output)
        })
    }

    pub(crate) fn benchmark_setup_ready(
        &self,
        session: &str,
        environment: &str,
        run: &str,
        instance: &str,
        container: &str,
        profile_digest: &str,
    ) -> Result<bool, String> {
        let key = std::env::var("AGENT_RUN_AUTHORIZATION_SIGNING_KEY")
            .map_err(|_| "benchmark_signing_key_unavailable")?;
        if key.is_empty() {
            return Err("benchmark_signing_key_unavailable".into());
        }
        self.with_benchmark_target(session, environment, run, key.as_bytes(), |target| {
            if target.profile_digest != profile_digest
                || target.retirement_state.is_some()
                || !target.resource_holding
                || !target.allocation.as_ref().is_some_and(|a| {
                    a.instance_id == instance
                        && a.container_id.as_deref() == Some(container)
                        && a.state.as_deref() == Some("resident")
                })
            {
                return Err("benchmark_setup_instance_rejected".into());
            }
            Ok(target.setup_ready)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_grant_binds_signed_identity_and_the_immutable_profile() {
        let payload = include_str!(
            "../../../../tests/workspace/fixtures/agent_run_authorization/v2/valid.json"
        );
        let authorization: WorkspaceAgentRunAuthorization = serde_json::from_str(payload).unwrap();
        let digest = authorization.digest().unwrap();
        let signature = authorization.signature(b"test-key").unwrap();
        let profile =
            serde_json::to_string(authorization.benchmark_profile.as_ref().unwrap()).unwrap();
        let binding = GrantBinding {
            session: "sess_1",
            run: "agent_run_1",
            workspace: "ws_1",
            user: "user_1",
            model: "model_1",
            thinking: Some("high"),
            bound_profile: &profile,
            bound_digest: authorization.benchmark_profile_digest.as_deref().unwrap(),
        };
        checked_grant(payload, &digest, &signature, b"test-key", &binding).unwrap();
        assert!(checked_grant(payload, &digest, &signature, b"wrong-key", &binding).is_err());
        for wrong in [
            GrantBinding {
                session: "foreign",
                ..binding
            },
            GrantBinding {
                run: "foreign",
                ..binding
            },
            GrantBinding {
                workspace: "foreign",
                ..binding
            },
            GrantBinding {
                user: "foreign",
                ..binding
            },
            GrantBinding {
                bound_digest: "sha256:wrong",
                ..binding
            },
        ] {
            assert!(checked_grant(payload, &digest, &signature, b"test-key", &wrong).is_err());
        }
    }

    #[test]
    #[ignore = "requires the gate-owned disposable PostgreSQL database"]
    fn postgres_benchmark_observation_setup_and_retirement_remain_exactly_bound() {
        let _guard = crate::postgres_store::integration_tests::TEST_LOCK
            .lock()
            .unwrap();
        let (_url, store, mut db) =
            crate::postgres_store::integration_tests::retained_instance_tests::setup();
        db.batch_execute("ALTER TABLE public.app_core_sessionenvironment ADD COLUMN benchmark_profile jsonb,ADD COLUMN benchmark_profile_digest text NOT NULL DEFAULT '',ADD COLUMN benchmark_setup_completed_at timestamptz,ADD COLUMN benchmark_setup_run_id text,ADD COLUMN benchmark_setup_instance_id text NOT NULL DEFAULT '',ADD COLUMN benchmark_setup_container_id text NOT NULL DEFAULT ''").unwrap();
        let authorization: WorkspaceAgentRunAuthorization = serde_json::from_str(include_str!(
            "../../../../tests/workspace/fixtures/agent_run_authorization/v2/valid.json"
        ))
        .unwrap();
        let digest = authorization.digest().unwrap();
        let signature = authorization.signature(b"test-key").unwrap();
        let profile = authorization.benchmark_profile.as_ref().unwrap();
        let profile_json = serde_json::to_string(profile).unwrap();
        let profile_digest = profile.digest().unwrap();
        db.execute("UPDATE public.app_core_sessionenvironment SET benchmark_profile=$1::text::jsonb,benchmark_profile_digest=$2", &[&profile_json,&profile_digest]).unwrap();
        db.batch_execute("INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,\"modelConfig_id\",\"thinkingMode\") VALUES('agent_run_1','sess_1','environment','ws_1','user_1','member','running','model_1','high')").unwrap();
        db.execute("INSERT INTO public.app_core_agentrunauthorization VALUES('agent_run_1',$1,$2::text::jsonb,$3)",&[&digest,&serde_json::to_string(&authorization).unwrap(),&signature]).unwrap();
        let read = |store: &PostgresRuntimeStore| {
            store.with_benchmark_target(
                "sess_1",
                "environment",
                "agent_run_1",
                b"test-key",
                |target| Ok(target.clone()),
            )
        };
        let before = read(&store).unwrap();
        assert!(before.allocation.is_none());
        assert!(!before.resource_holding);
        assert!(!before.setup_ready);
        let internal_profile = format!("sha256:{}", "c".repeat(64));
        let binding_digest = format!("sha256:{}", "d".repeat(64));
        db.execute("INSERT INTO runtime.execution_instances(execution_id,instance_id,environment_id,session_id,agent_run_id,host_id,authorization_digest,binding_digest,image_digest,allocation_execution_id,allocation_mode,profile_digest) VALUES('benchmark-execution','benchmark-instance','environment','sess_1','agent_run_1','host',$1,$2,$3,'benchmark-execution','retained',$4)",&[&digest,&binding_digest,&profile.image_digest,&internal_profile]).unwrap();
        db.execute("INSERT INTO runtime.resident_sandboxes(host_id,execution_id,agent_run_id,workspace_id,container_id,state,memory_bytes,cpu_milli,pids,workspace_bytes,created_at_ms,updated_at_ms,instance_id,environment_id,allocation_mode) VALUES('host','benchmark-execution','agent_run_1','ws_1','exact-container','resident',$1,$2,$3,$4,0,0,'benchmark-instance','environment','retained')",&[&(profile.resources.memory_bytes as i64),&i64::from(profile.resources.cpu_milli),&i64::from(profile.resources.pids_limit),&(profile.resources.data_tmpfs_bytes as i64)]).unwrap();
        let prepared = read(&store).unwrap();
        assert!(prepared.resource_holding);
        assert!(!prepared.setup_ready);
        assert_eq!(
            prepared.allocation.unwrap().container_id.as_deref(),
            Some("exact-container")
        );
        db.batch_execute("UPDATE public.app_core_sessionenvironment SET benchmark_setup_completed_at=now(),benchmark_setup_run_id='agent_run_1',benchmark_setup_instance_id='benchmark-instance',benchmark_setup_container_id='exact-container',controller_run_id='agent_run_1',controller_epoch=3,controller_purpose='cleanup';UPDATE public.app_core_agentrun SET status='completed' WHERE id='agent_run_1'").unwrap();
        let ready = read(&store).unwrap();
        assert!(ready.setup_ready);
        assert_eq!(ready.controller_epoch, 3);
        assert!(store
            .with_benchmark_target(
                "foreign",
                "environment",
                "agent_run_1",
                b"test-key",
                |_| Ok(())
            )
            .is_err());
        db.execute(
            "UPDATE public.app_core_sessionenvironment SET benchmark_profile_digest=$1",
            &[&internal_profile],
        )
        .unwrap();
        assert!(read(&store).is_err());
        db.execute(
            "UPDATE public.app_core_sessionenvironment SET benchmark_profile_digest=$1",
            &[&profile_digest],
        )
        .unwrap();
        let retirement = crate::postgres_store::residency::retirement::RetirementRequest {
            schema: "runtime.environment.retire.v1".into(),
            operation_id: "benchmark-retirement".into(),
            environment_id: "environment".into(),
            instance_id: "benchmark-instance".into(),
            host_id: "host".into(),
            expected_controller_epoch: 3,
            acknowledge_tmpfs_loss: true,
        };
        assert_eq!(
            store
                .retire_retained_instance(&retirement, |_| Ok(false))
                .unwrap()
                .status,
            "pending"
        );
        let pending = read(&store).unwrap();
        assert_eq!(pending.retirement_state.as_deref(), Some("pending"));
        assert!(pending.resource_holding);
        assert_eq!(
            store
                .retire_retained_instance(&retirement, |_| Ok(true))
                .unwrap()
                .status,
            "destroyed"
        );
        let destroyed = read(&store).unwrap();
        assert!(!destroyed.resource_holding);
        assert_eq!(destroyed.retirement_state.as_deref(), Some("destroyed"));
        assert_eq!(
            destroyed.allocation.unwrap().container_id.as_deref(),
            Some("exact-container")
        );
    }
}
