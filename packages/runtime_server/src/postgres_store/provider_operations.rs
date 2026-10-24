//! Durable provider creation evidence; observations do not grant execution authority.
use super::controller_authority::{
    verify_controller_authority, ControllerAuthorityRequest, ControllerPurpose,
    VerifiedOriginalGrant,
};
use super::PostgresRuntimeStore;
use crate::agent_run_authorization::EnvironmentProfile;
use postgres::{GenericClient, Row};

pub(crate) mod capacity;
pub mod cleanup;
pub mod origin;
pub(crate) mod provision;
use centaeris_core::execution::EnvironmentTaskOwner;
pub mod retained;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCreationIdentity {
    pub attempt_id: String,
    pub execution_id: String,
    pub instance_id: String,
    pub environment_id: String,
    pub session_id: String,
    pub workspace_id: String,
    pub owner: EnvironmentTaskOwner,
    pub authorization_digest: String,
    pub profile_digest: String,
    pub provider_config_digest: String,
    pub request_digest: String,
    pub idempotency_key: String,
    pub processing_job_id: Option<String>,
}

impl ProviderCreationIdentity {
    pub fn agent_run_id(&self) -> Option<&str> {
        match &self.owner {
            EnvironmentTaskOwner::AgentRun { run_id } => Some(run_id),
            EnvironmentTaskOwner::MaintenanceOperation { .. } => None,
        }
    }
}

pub struct ProviderCreationRequest<'a> {
    pub execution_id: &'a str,
    pub attempt_id: &'a str,
    pub idempotency_key: &'a str,
    pub request_digest: &'a str,
    pub configured_provider_config_digest: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCreationDispatchState {
    Prepared,
    Dispatched,
    NotDispatched,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCreationOperation {
    pub identity: ProviderCreationIdentity,
    pub dispatch_state: ProviderCreationDispatchState,
    pub reported_sandbox_id: Option<String>,
    pub recorded_at_ms: i64,
}

/// A committed send grant. Persisted operation evidence cannot construct this value.
#[derive(Debug)]
pub struct FreshProviderCreationDispatchPermit {
    operation: ProviderCreationOperation,
}

impl FreshProviderCreationDispatchPermit {
    pub fn operation(&self) -> &ProviderCreationOperation {
        &self.operation
    }

    pub(crate) fn consume(self) -> ProviderCreationOperation {
        self.operation
    }
}

impl PostgresRuntimeStore {
    pub fn prepare_provider_creation(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        request: &ProviderCreationRequest<'_>,
    ) -> Result<ProviderCreationOperation, String> {
        require_execute(authority)?;
        for value in [
            request.execution_id,
            request.attempt_id,
            request.idempotency_key,
        ] {
            validate_identity(value)?;
        }
        validate_digest(request.request_digest)?;
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let verified = verify_controller_authority(&mut tx, authority)?;
            let EnvironmentProfile::E2b(profile) = &verified.authorization.environment_profile else {
                return Err("provider_creation_requires_e2b_profile".into());
            };
            if profile.provider_config_digest != request.configured_provider_config_digest {
                return Err("provider_creation_configuration_rejected".into());
            }
            let retained_lifetime = profile.lifetime == "retained";
            let identity = ProviderCreationIdentity {
                attempt_id: request.attempt_id.into(),
                execution_id: request.execution_id.into(),
                instance_id: super::execution_instances::instance_id(request.execution_id),
                environment_id: authority.environment_authorization.payload.environment_id.clone(),
                session_id: verified.authorization.session_id.clone(),
                workspace_id: verified.authorization.workspace_id.clone(),
                owner: EnvironmentTaskOwner::AgentRun { run_id: verified.authorization.agent_run_id.clone() },
                authorization_digest: verified.authorization_digest,
                profile_digest: verified.authorization.environment_profile_digest.clone(),
                provider_config_digest: profile.provider_config_digest.clone(),
                request_digest: request.request_digest.into(),
                idempotency_key: request.idempotency_key.into(),
                processing_job_id: Some(authority.lease.job_id.clone()),
            };
            if let Some(existing) = load(&mut tx, request.attempt_id, true)? {
                require_identity(&existing, &identity)?;
                if retained_lifetime { retained::bind_creation(&mut tx, &identity)?; }
                tx.commit().map_err(|error| error.to_string())?;
                return Ok(existing);
            }
            tx.execute(
                r#"INSERT INTO runtime.provider_creation_operations(
                    attempt_id,execution_id,instance_id,environment_id,session_id,workspace_id,
                    agent_run_id,authorization_digest,profile_digest,provider,provider_config_digest,
                    request_digest,idempotency_key,processing_job_id,dispatch_state,recorded_at_ms,creation_owner_json,maintenance_operation_id)
                   VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'e2b',$10,$11,$12,$13,'prepared',
                    (EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint,$14,NULL)"#,
                &[
                    &identity.attempt_id, &identity.execution_id, &identity.instance_id,
                    &identity.environment_id, &identity.session_id, &identity.workspace_id,
                    &identity.agent_run_id(), &identity.authorization_digest, &identity.profile_digest,
                    &identity.provider_config_digest, &identity.request_digest,
                    &identity.idempotency_key, &identity.processing_job_id, &serde_json::to_string(&identity.owner).map_err(|_| "provider_creation_owner_invalid")?,
                ],
            ).map_err(|_| "provider_creation_identity_or_open_operation_conflict")?;
            let operation = load(&mut tx, request.attempt_id, false)?.ok_or("provider_creation_missing")?;
            if retained_lifetime { retained::bind_creation(&mut tx, &identity)?; }
            tx.commit().map_err(|error| error.to_string())?;
            Ok(operation)
        })
    }

    /// Only this fresh committed CAS constructs a send permit. Reads return evidence instead.
    pub fn dispatch_provider_creation(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        identity: &ProviderCreationIdentity,
    ) -> Result<FreshProviderCreationDispatchPermit, String> {
        self.dispatch_provider_creation_limited(authority, identity, None)
    }
    pub(crate) fn dispatch_provider_creation_limited(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        identity: &ProviderCreationIdentity,
        limit: Option<u32>,
    ) -> Result<FreshProviderCreationDispatchPermit, String> {
        require_execute(authority)?;
        self.with_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let verified = verify_controller_authority(&mut tx, authority)?;
            require_current_binding(&verified, authority, identity)?;
            let operation = load(&mut tx, &identity.attempt_id, true)?.ok_or("provider_creation_missing")?;
            require_identity(&operation, identity)?;
            if operation.dispatch_state != ProviderCreationDispatchState::Prepared {
                return Err("provider_creation_dispatch_not_authorized".into());
            }
            capacity::require(&mut tx,limit,&identity.provider_config_digest)?;
            tx.execute(
                "UPDATE runtime.provider_creation_operations SET dispatch_state='dispatched' WHERE attempt_id=$1 AND dispatch_state='prepared'",
                &[&identity.attempt_id],
            ).map_err(|error| error.to_string())?;
            let operation = load(&mut tx, &identity.attempt_id, false)?.ok_or("provider_creation_missing")?;
            tx.commit().map_err(|error| error.to_string())?;
            Ok(FreshProviderCreationDispatchPermit { operation })
        })
    }

    pub fn close_provider_creation_before_dispatch(
        &self,
        authority: &ControllerAuthorityRequest<'_>,
        identity: &ProviderCreationIdentity,
    ) -> Result<ProviderCreationOperation, String> {
        self.with_execution_control_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let verified = verify_controller_authority(&mut tx, authority)?;
            require_current_binding(&verified, authority, identity)?;
            let operation = load(&mut tx, &identity.attempt_id, true)?.ok_or("provider_creation_missing")?;
            require_identity(&operation, identity)?;
            match operation.dispatch_state {
                ProviderCreationDispatchState::Dispatched => {
                    return Err("provider_creation_not_dispatched_unproven".into());
                }
                ProviderCreationDispatchState::NotDispatched => {
                    tx.commit().map_err(|error| error.to_string())?;
                    return Ok(operation);
                }
                ProviderCreationDispatchState::Prepared => {}
            }
            tx.execute(
                "UPDATE runtime.provider_creation_operations SET dispatch_state='notDispatched' WHERE attempt_id=$1 AND dispatch_state='prepared'",
                &[&identity.attempt_id],
            ).map_err(|error| error.to_string())?;
            let operation = load(&mut tx, &identity.attempt_id, false)?.ok_or("provider_creation_missing")?;
            tx.commit().map_err(|error| error.to_string())?;
            Ok(operation)
        })
    }

    /// Late candidate evidence grants no execution, deletion, binding or refund rights.
    pub fn record_provider_creation_observation(
        &self,
        identity: &ProviderCreationIdentity,
        reported_sandbox_id: &str,
    ) -> Result<ProviderCreationOperation, String> {
        validate_identity(reported_sandbox_id)?;
        self.with_execution_control_client(|client| {
            let mut tx = client.transaction().map_err(|error| error.to_string())?;
            super::resource_commit::require_durable_resource_commit(&mut tx)?;
            let operation = load(&mut tx, &identity.attempt_id, true)?.ok_or("provider_creation_missing")?;
            require_identity(&operation, identity)?;
            if operation.dispatch_state != ProviderCreationDispatchState::Dispatched {
                return Err("provider_creation_observation_requires_dispatch".into());
            }
            if let Some(recorded) = &operation.reported_sandbox_id {
                if recorded != reported_sandbox_id {
                    return Err("provider_creation_reported_identity_conflict".into());
                }
                tx.commit().map_err(|error| error.to_string())?;
                return Ok(operation);
            }
            tx.execute(
                "UPDATE runtime.provider_creation_operations SET reported_sandbox_id=$2 WHERE attempt_id=$1 AND dispatch_state='dispatched' AND reported_sandbox_id IS NULL",
                &[&identity.attempt_id, &reported_sandbox_id],
            ).map_err(|error| error.to_string())?;
            let operation = load(&mut tx, &identity.attempt_id, false)?.ok_or("provider_creation_missing")?;
            tx.commit().map_err(|error| error.to_string())?;
            Ok(operation)
        })
    }

    pub fn provider_creation_operation(
        &self,
        attempt_id: &str,
    ) -> Result<Option<ProviderCreationOperation>, String> {
        validate_identity(attempt_id)?;
        self.with_execution_control_client(|client| load(client, attempt_id, false))
    }
}

fn validate_identity(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        return Err("provider_creation_identity_invalid".into());
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), String> {
    if value.len() != 71
        || !value.starts_with("sha256:")
        || !value.as_bytes()[7..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err("provider_creation_request_digest_invalid".into());
    }
    Ok(())
}

fn require_execute(authority: &ControllerAuthorityRequest<'_>) -> Result<(), String> {
    if authority.purpose != ControllerPurpose::Execute {
        return Err("provider_creation_requires_execute_authority".into());
    }
    Ok(())
}

fn require_current_binding(
    verified: &VerifiedOriginalGrant,
    authority: &ControllerAuthorityRequest<'_>,
    identity: &ProviderCreationIdentity,
) -> Result<(), String> {
    let original = &verified.authorization;
    let EnvironmentProfile::E2b(profile) = &original.environment_profile else {
        return Err("provider_creation_requires_e2b_profile".into());
    };
    if identity.environment_id != authority.environment_authorization.payload.environment_id
        || identity.session_id != original.session_id
        || identity.workspace_id != original.workspace_id
        || identity.agent_run_id() != Some(original.agent_run_id.as_str())
        || identity.authorization_digest != verified.authorization_digest
        || identity.profile_digest != original.environment_profile_digest
        || identity.provider_config_digest != profile.provider_config_digest
        || identity.processing_job_id.as_deref() != Some(authority.lease.job_id.as_str())
    {
        return Err("provider_creation_authority_binding_rejected".into());
    }
    Ok(())
}

fn require_identity(
    operation: &ProviderCreationOperation,
    identity: &ProviderCreationIdentity,
) -> Result<(), String> {
    if operation.identity != *identity {
        return Err("provider_creation_identity_rejected".into());
    }
    Ok(())
}

fn load(
    db: &mut impl GenericClient,
    attempt_id: &str,
    locked: bool,
) -> Result<Option<ProviderCreationOperation>, String> {
    // Optional fields are reconstructed only while upgrading released Run rows.
    // JSON projection keeps that migration read in the original single SQL call.
    let query=format!("SELECT attempt_id,execution_id,instance_id,environment_id,session_id,workspace_id,agent_run_id,authorization_digest,profile_digest,provider_config_digest,request_digest,idempotency_key,processing_job_id,dispatch_state,reported_sandbox_id,recorded_at_ms,to_jsonb(p)->>'creation_owner_json',to_jsonb(p)->>'maintenance_operation_id' FROM runtime.provider_creation_operations p WHERE attempt_id=$1{}",if locked {" FOR UPDATE"} else {""});
    db.query_opt(&query, &[&attempt_id])
        .map_err(|e| e.to_string())?
        .map(decode)
        .transpose()
}

/// Read-only durable facts for provider environment observation. No fact here
/// grants execution authority or fabricates a physical identity.
pub(in crate::postgres_store) struct ProviderEnvironmentFacts {
    pub(in crate::postgres_store) creation: Option<ProviderCreationOperation>,
    pub(in crate::postgres_store) verified_target: Option<cleanup::ProviderVerifiedTarget>,
    pub(in crate::postgres_store) power: Option<retained::power::ProviderEnvironmentPowerOperation>,
    pub(in crate::postgres_store) removed: bool,
}

pub(in crate::postgres_store) fn provider_environment_facts(
    db: &mut impl GenericClient,
    environment_id: &str,
) -> Result<ProviderEnvironmentFacts, String> {
    let Some(binding) = db
        .query_opt(
            "SELECT creation_attempt_id FROM runtime.provider_environment_bindings WHERE environment_id=$1 FOR SHARE",
            &[&environment_id],
        )
        .map_err(|e| e.to_string())?
    else {
        return Ok(ProviderEnvironmentFacts {
            creation: None,
            verified_target: None,
            power: None,
            removed: false,
        });
    };
    let creation = load(db, &binding.get::<_, String>(0), false)?;
    let verified_target = creation
        .as_ref()
        .map(|operation| cleanup::load_target(db, &operation.identity))
        .transpose()?
        .flatten();
    let power = retained::power::latest_environment_power(db, environment_id)?;
    let removed: bool = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM runtime.provider_environment_retirements WHERE environment_id=$1)",
            &[&environment_id],
        )
        .map_err(|e| e.to_string())?
        .get(0);
    Ok(ProviderEnvironmentFacts {
        creation,
        verified_target,
        power,
        removed,
    })
}

fn decode(row: Row) -> Result<ProviderCreationOperation, String> {
    let run: Option<String> = row.get(6);
    let job: Option<String> = row.get(12);
    let owner = match row.get::<_, Option<String>>(16) {
        Some(json) => serde_json::from_str::<EnvironmentTaskOwner>(&json)
            .map_err(|_| "provider_creation_owner_invalid")?,
        None => EnvironmentTaskOwner::AgentRun {
            run_id: run
                .clone()
                .ok_or("provider_creation_legacy_owner_missing")?,
        },
    };
    origin::owner_metadata(&owner, job.as_deref())?;
    let maintenance: Option<String> = row.get(17);
    match &owner {
        EnvironmentTaskOwner::AgentRun { run_id }
            if run.as_deref() == Some(run_id.as_str()) && maintenance.is_none() => {}
        EnvironmentTaskOwner::MaintenanceOperation { operation_id }
            if run.is_none() && maintenance.as_deref() == Some(operation_id.as_str()) => {}
        _ => return Err("provider_creation_owner_columns_rejected".into()),
    }
    Ok(ProviderCreationOperation {
        identity: ProviderCreationIdentity {
            attempt_id: row.get(0),
            execution_id: row.get(1),
            instance_id: row.get(2),
            environment_id: row.get(3),
            session_id: row.get(4),
            workspace_id: row.get(5),
            owner,
            authorization_digest: row.get(7),
            profile_digest: row.get(8),
            provider_config_digest: row.get(9),
            request_digest: row.get(10),
            idempotency_key: row.get(11),
            processing_job_id: job,
        },
        dispatch_state: match row.get::<_, String>(13).as_str() {
            "prepared" => ProviderCreationDispatchState::Prepared,
            "dispatched" => ProviderCreationDispatchState::Dispatched,
            "notDispatched" => ProviderCreationDispatchState::NotDispatched,
            _ => return Err("provider_creation_dispatch_state_invalid".into()),
        },
        reported_sandbox_id: row.get(14),
        recorded_at_ms: row.get(15),
    })
}

pub(super) const DDL: &str = r#"
CREATE TABLE runtime.provider_creation_operations(
 attempt_id text PRIMARY KEY,execution_id text NOT NULL UNIQUE,instance_id text NOT NULL UNIQUE,
 environment_id text NOT NULL,session_id text NOT NULL,workspace_id text NOT NULL,agent_run_id text NOT NULL,
 authorization_digest text NOT NULL,profile_digest text NOT NULL,provider text NOT NULL,
 provider_config_digest text NOT NULL,request_digest text NOT NULL,idempotency_key text NOT NULL,
 processing_job_id text NOT NULL REFERENCES runtime.runtime_jobs(job_id),
 dispatch_state text NOT NULL,reported_sandbox_id text,recorded_at_ms bigint NOT NULL,
 CONSTRAINT provider_creation_operations_provider_config_key UNIQUE(provider,provider_config_digest,idempotency_key),
 CONSTRAINT provider_creation_operation_facts CHECK(provider='e2b' AND dispatch_state IN('prepared','dispatched','notDispatched') AND (reported_sandbox_id IS NULL OR dispatch_state='dispatched') AND recorded_at_ms>=0));
CREATE UNIQUE INDEX provider_creation_operations_open_key ON runtime.provider_creation_operations(environment_id) WHERE dispatch_state IN('prepared','dispatched');
CREATE FUNCTION runtime.protect_provider_creation_operation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='DELETE' THEN RAISE EXCEPTION 'provider creation responsibility is durable'; END IF;
 IF TG_OP='INSERT' THEN
  IF NEW.dispatch_state<>'prepared' OR NEW.reported_sandbox_id IS NOT NULL THEN
   RAISE EXCEPTION 'provider creation requires a prepared intent'; END IF;
 ELSE
  IF ROW(NEW.attempt_id,NEW.execution_id,NEW.instance_id,NEW.environment_id,NEW.session_id,NEW.workspace_id,NEW.agent_run_id,NEW.authorization_digest,NEW.profile_digest,NEW.provider,NEW.provider_config_digest,NEW.request_digest,NEW.idempotency_key,NEW.processing_job_id,NEW.recorded_at_ms)
    IS DISTINCT FROM ROW(OLD.attempt_id,OLD.execution_id,OLD.instance_id,OLD.environment_id,OLD.session_id,OLD.workspace_id,OLD.agent_run_id,OLD.authorization_digest,OLD.profile_digest,OLD.provider,OLD.provider_config_digest,OLD.request_digest,OLD.idempotency_key,OLD.processing_job_id,OLD.recorded_at_ms)
   OR NOT ((OLD.dispatch_state='prepared' AND NEW.dispatch_state IN('dispatched','notDispatched') AND NEW.reported_sandbox_id IS NULL)
    OR (OLD.dispatch_state='dispatched' AND NEW.dispatch_state='dispatched' AND OLD.reported_sandbox_id IS NULL AND NEW.reported_sandbox_id IS NOT NULL))
  THEN RAISE EXCEPTION 'provider creation identity or evidence transition rejected'; END IF;
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER provider_creation_operation_facts BEFORE INSERT OR UPDATE OR DELETE ON runtime.provider_creation_operations FOR EACH ROW EXECUTE FUNCTION runtime.protect_provider_creation_operation();
"#;

pub(super) fn validate(db: &mut impl GenericClient) -> Result<(), String> {
    let cleanup = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM runtime.schema_migrations WHERE version=18)",
            &[],
        )
        .map_err(|e| e.to_string())?
        .get::<_, bool>(0);
    let typed: bool = db
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM runtime.schema_migrations WHERE version=22)",
            &[],
        )
        .map_err(|e| e.to_string())?
        .get(0);
    let guard = if typed {
        origin::schema::creation_guard()
    } else if cleanup {
        cleanup::GUARD_DDL.to_owned()
    } else {
        DDL.to_owned()
    };
    let body = guard
        .split_once("FUNCTION runtime.protect_provider_creation_operation() RETURNS trigger LANGUAGE plpgsql AS $$")
        .ok_or("provider creation function missing")?
        .1
        .split_once("$$;")
        .ok_or("provider creation function body missing")?
        .0;
    let actual = db.query_opt(
        "SELECT prosrc FROM pg_proc WHERE oid=to_regprocedure('runtime.protect_provider_creation_operation()')",
        &[],
    ).map_err(|error| error.to_string())?;
    if actual.is_none_or(|row| row.get::<_, String>(0) != body) {
        return Err("Postgres provider creation function mismatch".into());
    }
    let trigger = db
        .query_one(
            r#"SELECT count(*) FROM pg_trigger
           WHERE tgrelid='runtime.provider_creation_operations'::regclass
            AND tgname='provider_creation_operation_facts' AND tgenabled='O'
            AND NOT tgisinternal AND tgtype=31
            AND tgfoid='runtime.protect_provider_creation_operation()'::regprocedure
            AND tgqual IS NULL AND tgnargs=0"#,
            &[],
        )
        .map_err(|error| error.to_string())?
        .get::<_, i64>(0);
    if trigger != 1 {
        return Err("Postgres provider creation trigger mismatch".into());
    }
    let constraints = db
        .query_one(
            r#"SELECT count(*),bool_and(convalidated AND NOT condeferrable),
            count(*) FILTER(WHERE contype='p' AND conkey=ARRAY[1]::smallint[]),
            count(*) FILTER(WHERE contype='u' AND conkey=ARRAY[2]::smallint[]),
            count(*) FILTER(WHERE contype='u' AND conkey=ARRAY[3]::smallint[]),
            count(*) FILTER(WHERE contype='u' AND conkey=ARRAY[10,11,13]::smallint[]),
            count(*) FILTER(WHERE contype='f' AND conkey=ARRAY[14]::smallint[]
             AND confrelid='runtime.runtime_jobs'::regclass AND confkey=ARRAY[1]::smallint[]
             AND confdeltype='a' AND confupdtype='a'),
            count(*) FILTER(WHERE contype='c')
           FROM pg_constraint
           WHERE conrelid='runtime.provider_creation_operations'::regclass AND contype<>'n'"#,
            &[],
        )
        .map_err(|error| error.to_string())?;
    if constraints.get::<_, i64>(0)
        != if typed {
            9
        } else if cleanup {
            8
        } else {
            6
        }
        || !constraints.get::<_, bool>(1)
        || (2..=6).any(|column| constraints.get::<_, i64>(column) != 1)
        || constraints.get::<_, i64>(7)
            != if typed {
                3
            } else if cleanup {
                2
            } else {
                1
            }
    {
        return Err("Postgres provider creation identity constraints mismatch".into());
    }
    let facts = db
        .query_opt(
            r#"SELECT pg_get_constraintdef(oid) FROM pg_constraint
           WHERE conrelid='runtime.provider_creation_operations'::regclass
            AND conname='provider_creation_operation_facts' AND contype='c'"#,
            &[],
        )
        .map_err(|error| error.to_string())?
        .map(|row| row.get::<_, String>(0));
    let expected_facts = concat!(
        "CHECK (((provider = 'e2b'::text) AND ",
        "(dispatch_state = ANY (ARRAY['prepared'::text, 'dispatched'::text, 'notDispatched'::text])) ",
        "AND ((reported_sandbox_id IS NULL) OR (dispatch_state = 'dispatched'::text)) ",
        "AND (recorded_at_ms >= 0)))",
    );
    if facts.as_deref() != Some(expected_facts) {
        return Err(format!(
            "Postgres provider creation evidence constraint mismatch: {facts:?}"
        ));
    }
    let index = db
        .query_opt(
            r#"SELECT indisunique AND indisvalid AND indisready AND indnkeyatts=1
            AND indnatts=1 AND indkey::text='4' AND indexprs IS NULL,
            pg_get_expr(indpred,indrelid)
           FROM pg_index
           WHERE indexrelid=to_regclass('runtime.provider_creation_operations_open_key')
            AND indrelid='runtime.provider_creation_operations'::regclass"#,
            &[],
        )
        .map_err(|error| error.to_string())?;
    if index.is_none_or(|row| {
        !row.get::<_, bool>(0)
            || row.get::<_, Option<String>>(1).as_deref()
                != Some(if cleanup { "((dispatch_state = ANY (ARRAY['prepared'::text, 'dispatched'::text])) AND (settlement_id IS NULL))" } else { "(dispatch_state = ANY (ARRAY['prepared'::text, 'dispatched'::text]))" })
    }) {
        return Err("Postgres provider creation open intent index mismatch".into());
    }
    if typed {
        origin::schema::validate(db)?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn restore_previous_fixture(db: &mut postgres::Client) {
    cleanup::restore_previous_fixture(db);
    let version: i64 = db
        .query_one("SELECT max(version) FROM runtime.schema_migrations", &[])
        .unwrap()
        .get(0);
    if version < 17 {
        return;
    }
    assert_eq!(version, 17);
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM runtime.provider_creation_operations",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0,
        "cannot discard provider creation evidence"
    );
    db.batch_execute("DROP TABLE runtime.provider_creation_operations; DROP FUNCTION runtime.protect_provider_creation_operation(); DELETE FROM runtime.schema_migrations WHERE version=17;").unwrap();
}
