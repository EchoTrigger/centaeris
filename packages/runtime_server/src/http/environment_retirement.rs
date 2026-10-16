//! Internal maintenance authority is independent of Run execute/cleanup grants.
use super::*;
use crate::postgres_store::residency::retirement::{RetirementRequest, RetirementTarget};

fn maintenance_authorized(
    configured: Option<&str>,
    internal: Option<&str>,
    supplied: Option<&str>,
) -> bool {
    configured.is_some_and(|key| key.len() >= 32 && Some(key) != internal && supplied == Some(key))
}

pub(super) fn retire(
    state: HttpServerState,
    request: Request<Bytes>,
) -> Result<Response<Body>, String> {
    let key = env::var("ENVIRONMENT_MAINTENANCE_TOKEN").ok();
    let internal = env::var("INTERNAL_API_TOKEN").ok();
    if !maintenance_authorized(
        key.as_deref(),
        internal.as_deref(),
        request
            .headers()
            .get("x-environment-maintenance-token")
            .and_then(|v| v.to_str().ok()),
    ) {
        return json_error_response(403, "environment_maintenance_authority_required");
    }
    let request: RetirementRequest = match serde_json::from_slice(request.body()) {
        Ok(r) => r,
        Err(_) => return json_error_response(400, "environment_retirement_request_invalid"),
    };
    if request.validate().is_err() {
        return json_error_response(400, "environment_retirement_request_invalid");
    }
    // Host identity comes from the same local daemon used for allocation.
    let host = crate::docker_execution_host::resident_host_context().map(|(id, _)| id);
    if host.as_ref() != Ok(&request.host_id) {
        return json_error_response(503, "environment_retirement_host_unavailable");
    }
    match state
        .job_store
        .retire_retained_instance(&request, retire_physical)
    {
        Ok(result) => Ok(http_response(
            200,
            "application/json",
            serde_json::to_vec(&result).map_err(|e| e.to_string())?,
        )),
        Err(_) => json_error_response(409, "environment_retirement_rejected"),
    }
}

fn physical_identity(facts: &serde_json::Value, target: &RetirementTarget) -> bool {
    let labels = &facts["Config"]["Labels"];
    facts["Id"].as_str() == Some(target.container_id.as_str())
        && facts["Image"].as_str() == Some(target.image_digest.as_str())
        && [
            ("centaeris.managed", "true"),
            ("centaeris.environment_id", target.environment_id.as_str()),
            ("centaeris.instance_id", target.instance_id.as_str()),
            (
                "centaeris.execution_id",
                target.allocation_execution_id.as_str(),
            ),
            (
                "centaeris.agent_run_id",
                target.allocation_agent_run_id.as_str(),
            ),
            (
                "centaeris.environment_authorization_digest",
                target.binding_digest.as_str(),
            ),
        ]
        .iter()
        .all(|(name, value)| labels[*name].as_str() == Some(*value))
}

fn retire_physical(target: &RetirementTarget) -> Result<bool, String> {
    retire_observed(target, crate::docker_engine::inspect, |id| {
        crate::docker_engine::remove(id, false)
    })
}

fn retire_observed(
    target: &RetirementTarget,
    mut inspect: impl FnMut(&str) -> Result<Option<serde_json::Value>, String>,
    remove: impl FnOnce(&str) -> Result<(), String>,
) -> Result<bool, String> {
    if let Some(facts) = inspect(&target.container_id)? {
        if !physical_identity(&facts, target) {
            return Err("retirement physical identity rejected".into());
        }
        // Docker force-removes this exact container and its instance-owned tree.
        // A lost mutation ACK is resolved only by an independent inspect.
        let _ack = remove(&target.container_id);
    }
    Ok(inspect(&target.container_id)?.is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    fn target() -> RetirementTarget {
        RetirementTarget {
            allocation_execution_id: "execution".into(),
            allocation_agent_run_id: "origin".into(),
            environment_id: "environment".into(),
            instance_id: "instance".into(),
            container_id: "physical".into(),
            image_digest: "sha256:image".into(),
            binding_digest: "sha256:binding".into(),
        }
    }
    fn facts(t: &RetirementTarget) -> serde_json::Value {
        json!({"Id":t.container_id,"Image":t.image_digest,"Config":{"Labels":{"centaeris.managed":"true","centaeris.environment_id":t.environment_id,"centaeris.instance_id":t.instance_id,"centaeris.execution_id":t.allocation_execution_id,"centaeris.agent_run_id":t.allocation_agent_run_id,"centaeris.environment_authorization_digest":t.binding_digest}}})
    }
    #[test]
    fn run_token_never_confers_maintenance_authority() {
        let key = "maintenance-key-with-at-least-32-bytes";
        assert!(!maintenance_authorized(None, Some("run"), Some(key)));
        assert!(!maintenance_authorized(Some(key), Some(key), Some(key)));
        assert!(!maintenance_authorized(Some(key), Some("run"), Some("run")));
        assert!(maintenance_authorized(Some(key), Some("run"), Some(key)));
    }
    #[test]
    fn lost_delete_ack_is_resolved_by_exact_absence() {
        let t = target();
        let calls = Cell::new(0);
        assert!(retire_observed(
            &t,
            |id| {
                assert_eq!(id, "physical");
                let n = calls.get();
                calls.set(n + 1);
                Ok(if n == 0 { Some(facts(&t)) } else { None })
            },
            |id| {
                assert_eq!(id, "physical");
                Err("lost ack".into())
            }
        )
        .unwrap());
        assert_eq!(calls.get(), 2);
    }
    #[test]
    fn unobservable_or_foreign_instance_is_never_success_or_removed() {
        let t = target();
        let removed = Cell::new(false);
        let mut foreign = facts(&t);
        foreign["Config"]["Labels"]["centaeris.instance_id"] = json!("other");
        assert!(retire_observed(
            &t,
            |_| Ok(Some(foreign.clone())),
            |_| {
                removed.set(true);
                Ok(())
            }
        )
        .is_err());
        assert!(!removed.get());
        let calls = Cell::new(0);
        assert!(retire_observed(
            &t,
            |_| {
                let n = calls.get();
                calls.set(n + 1);
                if n == 0 {
                    Ok(Some(facts(&t)))
                } else {
                    Err("daemon unavailable".into())
                }
            },
            |_| Ok(())
        )
        .is_err());
        assert!(!retire_observed(&t, |_| Ok(Some(facts(&t))), |_| Ok(())).unwrap());
    }
}
