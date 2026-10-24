use super::*;

pub(super) fn task_profile() -> EnvironmentProfile {
    let mut value = serde_json::to_value(profile(OVERHEAD + 1024)).unwrap();
    value["workspaceInitialization"] = json!("taskImage");
    value["lifetime"] = json!("retained");
    value["workspaceRoot"] = json!("/app");
    value["user"] = json!("root");
    serde_json::from_value(value).unwrap()
}
fn task_service(url: &str, helper: Arc<Helper>) -> E2bSessionWorkspace {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    E2bSessionWorkspace::new(
        task_profile(),
        SessionWorkspaceApi {
            client: &client,
            api_url: url,
            api_token: "fixture-only",
            agent_run_id: "run-a",
            authorization_digest: &digest("run-a"),
        },
        helper,
    )
    .expect("explicit taskImage supports original root and /app")
}

#[test]
fn task_image_empty_archive_keeps_provider_files_without_restore_or_quiescence() {
    let helper = Helper::new(b"native task image files", Fault::None);
    let server = Server::new(|request| {
        assert_eq!(
            request.path,
            "/internal/agent-runs/session-workspace/resolve"
        );
        Some(resolved(&empty(), "empty"))
    });
    let workspace = task_service(&server.url, helper.clone());
    assert_eq!(
        workspace.restore(&lease("run-a"), &empty(), 0),
        Ok(SessionWorkspaceResolution::Empty)
    );
    assert!(helper.events.lock().unwrap().is_empty());
    assert!(helper.restored.lock().unwrap().is_empty());
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn task_image_rejects_old_archive_and_inputs_before_network_or_helper_effects() {
    let helper = Helper::new(b"previous archive", Fault::None);
    let workspace = task_service("http://127.0.0.1:1", helper.clone());
    let old = frozen(&helper.wire, b"previous archive".len());
    for (snapshot, inputs, expected) in [
        (&old, 0, "e2b_task_image_archive_unsupported"),
        (&empty(), 1, "e2b_task_image_inputs_unsupported"),
    ] {
        assert_eq!(
            workspace.restore(&lease("run-a"), snapshot, inputs),
            Err(SessionWorkspaceApiError::Rejected(expected.into()))
        );
        assert_eq!(
            workspace
                .collect_and_commit(&lease("run-a"), snapshot, inputs)
                .unwrap(),
            SessionWorkspaceCommitOutcome::Rejected(expected.into())
        );
    }
    assert!(helper.events.lock().unwrap().is_empty());
}

#[test]
fn task_image_terminal_retention_returns_verified_binding_without_archive_commit() {
    let helper = Helper::new(b"native result", Fault::None);
    let server = Server::new(|request| {
        assert!(request.path.ends_with("/resolve"));
        Some(resolved(&empty(), "empty"))
    });
    let workspace = task_service(&server.url, helper.clone());
    let receipt = RetainedProviderWorkspaceBinding {
        environment_id: "environment-owned".into(),
        instance_id: "instance-owned".into(),
        sandbox_id: "sandbox-owned".into(),
        profile_digest: task_profile().digest().unwrap(),
    };
    assert_eq!(
        workspace
            .collect_and_commit(&lease("run-a"), &empty(), 0)
            .unwrap(),
        SessionWorkspaceCommitOutcome::RetainedProvider(receipt)
    );
    assert_eq!(
        *helper.events.lock().unwrap(),
        vec!["exclusive", "verifyRetainedBinding", "release"]
    );
    assert_eq!(server.finish().len(), 1);
}
