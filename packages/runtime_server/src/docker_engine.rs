//! Local Docker transport. Runtime semantics stay in the execution host.
mod exec;
pub(crate) use exec::{ExecChild, ExecReader, ExecRequest, ExecStatus, ExecWriter};
use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use bollard::{errors::Error, models::ContainerCreateBody, query_parameters::*, Docker};
use serde_json::Value;

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const MUTATION_TIMEOUT: Duration = Duration::from_secs(30);
static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();
static IO_RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();

fn parse_create_limit(value: Option<&str>) -> Result<usize, String> {
    let Some(value) = value else { return Ok(2) };
    value
        .parse::<usize>()
        .ok()
        .filter(|value| (1..=16).contains(value))
        .ok_or_else(|| "DOCKER_CREATE_CONCURRENCY must be an integer between 1 and 16".to_string())
}

pub(crate) fn create_limit_from_env() -> Result<usize, String> {
    match std::env::var("DOCKER_CREATE_CONCURRENCY") {
        Ok(value) => parse_create_limit(Some(&value)),
        Err(std::env::VarError::NotPresent) => parse_create_limit(None),
        Err(_) => Err("DOCKER_CREATE_CONCURRENCY must be valid Unicode".to_string()),
    }
}

struct Engine {
    docker: Docker,
    runtime: &'static tokio::runtime::Runtime,
    creates: tokio::sync::Semaphore,
}

fn shared() -> Result<&'static Engine, String> {
    ENGINE
        .get_or_init(|| {
            if std::env::var("DOCKER_CONTEXT").is_ok_and(|value| !value.is_empty()) {
                return Err(
                    "Docker API requires an explicit local socket, not DOCKER_CONTEXT".to_string(),
                );
            }
            let docker = match std::env::var("DOCKER_HOST") {
                Err(std::env::VarError::NotPresent) => Docker::connect_with_socket_defaults(),
                Ok(address)
                    if address.starts_with("unix://") || address.starts_with("npipe://") =>
                {
                    Docker::connect_with_socket(&address, 30, bollard::API_DEFAULT_VERSION)
                }
                _ => {
                    return Err(
                        "Docker API supports only a local unix socket or named pipe".to_string()
                    )
                }
            }
            .map_err(|error| error.to_string())?;
            let runtime = IO_RUNTIME
                .get_or_init(|| {
                    tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())
                })
                .as_ref()
                .map_err(Clone::clone)?;
            let mut engine = Engine {
                docker,
                runtime,
                creates: tokio::sync::Semaphore::new(create_limit_from_env()?),
            };
            engine.docker = engine.run(QUERY_TIMEOUT, async {
                engine
                    .docker
                    .clone()
                    .negotiate_version()
                    .await
                    .map_err(|error| error.to_string())
            })?;
            Ok(engine)
        })
        .as_ref()
        .map_err(Clone::clone)
}

impl Engine {
    fn run<T: Send>(
        &self,
        timeout: Duration,
        future: impl Future<Output = Result<T, String>> + Send,
    ) -> Result<T, String> {
        let run = || {
            self.runtime.block_on(async {
                tokio::time::timeout(timeout, future).await.map_err(|_| {
                    "Docker API deadline exceeded; operation outcome requires inspection"
                        .to_string()
                })?
            })
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
                return tokio::task::block_in_place(run);
            }
            return std::thread::scope(|scope| {
                scope
                    .spawn(run)
                    .join()
                    .map_err(|_| "Docker API bridge panicked".to_string())?
            });
        }
        run()
    }
}

pub(crate) fn info() -> Result<Value, String> {
    let engine = shared()?;
    engine.run(QUERY_TIMEOUT, async {
        serde_json::to_value(engine.docker.info().await.map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    })
}

pub(crate) fn image_id(image: &str) -> Result<String, String> {
    let engine = shared()?;
    engine.run(QUERY_TIMEOUT, async {
        engine
            .docker
            .inspect_image(image)
            .await
            .map_err(|e| e.to_string())?
            .id
            .ok_or_else(|| "Docker image identity missing".to_string())
    })
}

pub(crate) fn inspect(id: &str) -> Result<Option<Value>, String> {
    let engine = shared()?;
    engine.run(QUERY_TIMEOUT, async {
        decode_inspect(
            engine
                .docker
                .inspect_container(id, None::<InspectContainerOptions>)
                .await,
        )
    })
}

fn decode_inspect(
    result: Result<bollard::models::ContainerInspectResponse, Error>,
) -> Result<Option<Value>, String> {
    match result {
        Ok(value) => serde_json::to_value(value)
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(Error::DockerResponseServerError {
            status_code: 404, ..
        }) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn list_owned(agent_run_id: &str) -> Result<Vec<String>, String> {
    let engine = shared()?;
    engine.run(QUERY_TIMEOUT, async {
        let options = ListContainersOptions {
            all: true,
            filters: Some(std::collections::HashMap::from([(
                "label".to_string(),
                vec![
                    "centaeris.managed=true".to_string(),
                    format!("centaeris.agent_run_id={agent_run_id}"),
                ],
            )])),
            ..Default::default()
        };
        engine
            .docker
            .list_containers(Some(options))
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|container| {
                container
                    .id
                    .ok_or_else(|| "Docker list omitted container ID".to_string())
            })
            .collect()
    })
}

/// Exact supplied fields must survive the daemon's normalized inspect response.
/// Extra daemon defaults are allowed; no supplied security setting is skipped.
fn contains(actual: &Value, expected: &Value) -> bool {
    match expected {
        Value::Object(fields) => fields
            .iter()
            .all(|(key, value)| actual.get(key).is_some_and(|found| contains(found, value))),
        Value::Array(values) => actual.as_array().is_some_and(|found| {
            found.len() == values.len()
                && values
                    .iter()
                    .zip(found)
                    .all(|(expected, actual)| contains(actual, expected))
        }),
        _ => actual == expected,
    }
}

pub(crate) fn verify_created(facts: &Value, request: &Value) -> Result<String, String> {
    // Engine omits Mount.ReadOnly when false. Normalize only that documented
    // mount default, never arbitrary missing fields or explicit null values.
    let mut normalized = facts.clone();
    if let Some(mounts) = normalized
        .pointer_mut("/HostConfig/Mounts")
        .and_then(Value::as_array_mut)
    {
        for mount in mounts {
            if let Some(fields) = mount.as_object_mut() {
                fields.entry("ReadOnly").or_insert(Value::Bool(false));
            }
        }
    }
    let facts = &normalized;
    for (key, expected) in request
        .as_object()
        .ok_or("Docker create body is not an object")?
    {
        let actual = if key == "HostConfig" {
            &facts[key]
        } else if key == "Image" {
            &facts["Image"]
        } else {
            &facts["Config"][key]
        };
        if !contains(actual, expected) {
            return Err(format!(
                "Docker container configuration mismatch: {key}; refusing adoption"
            ));
        }
    }
    facts["Id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "Docker container identity missing".to_string())
}

pub(crate) fn ensure_created(name: &str, request: Value) -> Result<String, String> {
    crate::observations::timed("dockerEnsureCreated", || {
        ensure_created_inner(name, request)
    })
}

fn ensure_created_inner(name: &str, request: Value) -> Result<String, String> {
    if let Some(facts) = inspect(name)? {
        return verify_created(&facts, &request);
    }
    let engine = shared()?;
    let body: ContainerCreateBody =
        serde_json::from_value(request.clone()).map_err(|e| e.to_string())?;
    let encoded = serde_json::to_value(&body).map_err(|e| e.to_string())?;
    if !contains(&encoded, &request) {
        return Err(
            "Docker SDK cannot represent the requested container configuration".to_string(),
        );
    }
    let created = engine.run(MUTATION_TIMEOUT, async {
        let mut wait = crate::observations::Span::new("dockerCreatePermitWait");
        let permit = engine.creates.acquire().await;
        wait.finished(permit.is_ok());
        drop(wait);
        let _permit = permit.map_err(|e| e.to_string())?;
        let mut span = crate::observations::Span::new("dockerCreate");
        let result = engine
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: Some(name.to_string()),
                    ..Default::default()
                }),
                body,
            )
            .await
            .map(|response| response.id)
            .map_err(|e| e.to_string());
        span.finished(result.is_ok());
        result
    });
    // Also inspect after an error: create may have committed before the socket
    // failed. Never issue another create or change the deterministic name here.
    let lookup = created.as_deref().unwrap_or(name);
    let facts = inspect(lookup)?.ok_or_else(|| {
        created
            .as_ref()
            .err()
            .cloned()
            .unwrap_or_else(|| "Docker created container disappeared".to_string())
    })?;
    let verified = verify_created(&facts, &request);
    if verified.is_err() {
        if let Ok(id) = &created {
            if facts["Id"].as_str() == Some(id.as_str())
                && request
                    .get("Labels")
                    .is_some_and(|labels| contains(&facts["Config"]["Labels"], labels))
            {
                remove(id, true).map_err(|cleanup| {
                    format!(
                        "{}; cleanup failed: {cleanup}",
                        verified.as_ref().unwrap_err()
                    )
                })?;
            }
        }
    }
    verified
}

pub(crate) fn start(id: &str) -> Result<(), String> {
    crate::observations::timed("dockerStart", || start_inner(id))
}

fn start_inner(id: &str) -> Result<(), String> {
    let facts = inspect(id)?.ok_or("Docker container missing before start")?;
    match facts["State"]["Status"].as_str() {
        Some("running" | "exited") => return Ok(()), // Never restart a completed process.
        Some("created") => {}
        _ => return Err("Docker container is not in a startable state".to_string()),
    }
    let engine = shared()?;
    let started = engine.run(MUTATION_TIMEOUT, async {
        let mut span = crate::observations::Span::new("dockerDaemonStart");
        let result = engine
            .docker
            .start_container(id, None::<StartContainerOptions>)
            .await
            .map_err(|e| e.to_string());
        span.finished(result.is_ok());
        result
    });
    let facts = inspect(id)?.ok_or("Docker container disappeared after start")?;
    if matches!(
        facts["State"]["Status"].as_str(),
        Some("running" | "exited")
    ) {
        Ok(())
    } else {
        Err(started
            .err()
            .unwrap_or_else(|| "Docker start was not confirmed".to_string()))
    }
}

pub(crate) fn remove(id: &str, volumes: bool) -> Result<(), String> {
    crate::observations::timed("dockerRemove", || remove_inner(id, volumes))
}

fn remove_inner(id: &str, volumes: bool) -> Result<(), String> {
    // Callers verify ownership and pass the immutable ID, never a mutable name.
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Docker removal requires an immutable container ID".to_string());
    }
    let engine = shared()?;
    let removed = engine.run(MUTATION_TIMEOUT, async {
        let mut span = crate::observations::Span::new("dockerDaemonRemove");
        let result = engine
            .docker
            .remove_container(
                id,
                Some(RemoveContainerOptions {
                    force: true,
                    v: volumes,
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| e.to_string());
        span.finished(result.is_ok());
        result
    });
    if inspect(id)?.is_none() {
        Ok(())
    } else {
        Err(removed
            .err()
            .unwrap_or_else(|| "Docker removal was not confirmed".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn create_limit_defaults_to_two_and_rejects_invalid_values() {
        assert_eq!(parse_create_limit(None).unwrap(), 2);
        for value in ["1", "2", "4", "16"] {
            assert_eq!(
                parse_create_limit(Some(value)).unwrap(),
                value.parse::<usize>().unwrap()
            );
        }
        for value in [
            "",
            "0",
            "17",
            "-1",
            "four",
            " 4",
            "999999999999999999999999",
        ] {
            assert!(parse_create_limit(Some(value)).is_err(), "{value}");
        }
    }
    #[test]
    fn mount_readonly_omission_is_writable_without_weakening_other_fields() {
        let request = json!({"HostConfig":{"ReadonlyRootfs":true,"Mounts":[{"Type":"volume","Source":"memory","Target":"/memory","ReadOnly":false,"VolumeOptions":{"Subpath":"tenant"}}]}});
        let mut facts = json!({"Id":"owned", "HostConfig":request["HostConfig"]});
        assert!(verify_created(&facts, &request).is_ok());
        facts["HostConfig"]["Mounts"][0]
            .as_object_mut()
            .unwrap()
            .remove("ReadOnly");
        assert!(verify_created(&facts, &request).is_ok());
        let mut readonly_request = request.clone();
        readonly_request["HostConfig"]["Mounts"][0]["ReadOnly"] = json!(true);
        assert!(verify_created(&facts, &readonly_request).is_err());
        for invalid in [json!(true), json!(null), json!("false")] {
            let mut changed = facts.clone();
            changed["HostConfig"]["Mounts"][0]["ReadOnly"] = invalid;
            assert!(verify_created(&changed, &request).is_err());
        }
        for field in ["Source", "Target", "VolumeOptions"] {
            let mut changed = facts.clone();
            changed["HostConfig"]["Mounts"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(verify_created(&changed, &request).is_err());
        }
        facts["HostConfig"]
            .as_object_mut()
            .unwrap()
            .remove("ReadonlyRootfs");
        assert!(verify_created(&facts, &request).is_err());
        assert!(!contains(&json!({}), &json!({"OtherFlag":false})));
    }
    #[test]
    fn only_an_engine_404_means_missing_container() {
        assert!(decode_inspect(Err(Error::DockerResponseServerError {
            status_code: 404,
            message: "missing".to_string()
        }))
        .unwrap()
        .is_none());
        assert!(decode_inspect(Err(Error::DockerResponseServerError {
            status_code: 503,
            message: "unavailable".to_string()
        }))
        .is_err());
    }
    #[test]
    fn adoption_requires_every_requested_identity_and_security_field() {
        let request = json!({"Image":"sha256:abc", "Labels":{"owner":"ours"}, "HostConfig":{"ReadonlyRootfs":true,"CapDrop":["ALL"],"Mounts":[{"Type":"volume","Target":"/data","VolumeOptions":{"Subpath":"tenant"}}]}});
        let facts = json!({"Id":"container-id", "Image":"sha256:abc", "Config":{"Labels":{"owner":"ours"}}, "HostConfig":request["HostConfig"]});
        assert!(verify_created(&facts, &request).is_ok());
        for field in ["ReadonlyRootfs", "CapDrop", "Mounts"] {
            let mut changed = facts.clone();
            changed["HostConfig"][field] = Value::Null;
            assert!(verify_created(&changed, &request).is_err());
        }
        let mut foreign = facts;
        foreign["Config"]["Labels"]["owner"] = json!("foreign");
        assert!(verify_created(&foreign, &request).is_err());
    }
}
