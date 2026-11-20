use centaeris_agent::SessionCreateCommandRequest;
use centaeris_cli_runtime::LocalAgentHost;
use std::error::Error;
use std::process::Command;
use std::time::Duration;

#[test]
fn embedded_host_serves_native_sessions() {
    const CHILD: &str = "CENTAERIS_TEST_EMBEDDED_HOST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let runtime = tokio::runtime::Runtime::new().expect("native SDK runtime");
        let result = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(30), exercise_embedded_host()).await
        });
        runtime.shutdown_timeout(Duration::from_millis(100));
        result
            .expect("embedded host lifecycle deadline")
            .expect("native Session lifecycle");
        return;
    }

    let profile = tempfile::tempdir().expect("isolated SDK profile");
    let output = Command::new(std::env::current_exe().expect("SDK test executable"))
        .args([
            "embedded_host_serves_native_sessions",
            "--exact",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("CENTAERIS_DESKTOP_DATA_DIR", profile.path())
        .env_remove("CENTAERIS_CONFIG_PATH")
        .env_remove("CENTAERIS_MESSAGE_LOG_SESSIONS_DIR")
        .env_remove("CENTAERIS_AGENT_RUNTIME_DB_PATH")
        .output()
        .expect("run isolated SDK host");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn exercise_embedded_host() -> Result<(), Box<dyn Error>> {
    let host = LocalAgentHost::start().await?;
    let duplicate = LocalAgentHost::start()
        .await
        .err()
        .ok_or("profile admitted a second host owner")?;
    assert_eq!(duplicate.code(), "runtime_server_already_running");
    let mut client = host.connect("embedded-sdk-viewer")?;
    let workspace = std::path::PathBuf::from(
        std::env::var_os("CENTAERIS_DESKTOP_DATA_DIR").ok_or("isolated profile missing")?,
    )
    .join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let result = async {
        let session = client
            .create_session(SessionCreateCommandRequest {
                operation_id: "embedded-sdk-create".try_into()?,
                title: Some("Embedded Session".to_string()),
                cwd: workspace.to_string_lossy().into_owned(),
            })
            .await?;
        let reopened = client.open_session(session.id()).await?;
        assert_eq!(reopened.id(), session.id());
        let loaded = reopened.load().await?;
        assert_eq!(loaded.id, session.id());
        assert_eq!(loaded.title, "Embedded Session");
        assert!(loaded.messages.is_empty());
        Ok::<(), Box<dyn Error>>(())
    }
    .await;
    let shutdown = host.shutdown().await;
    result?;
    shutdown?;
    assert!(
        tokio::time::timeout(Duration::from_secs(2), client.next_event())
            .await?
            .is_none()
    );
    let error = client
        .create_session(SessionCreateCommandRequest {
            operation_id: "embedded-sdk-after-shutdown".try_into()?,
            title: None,
            cwd: workspace.to_string_lossy().into_owned(),
        })
        .await
        .err()
        .ok_or("closed host admitted a new Session")?;
    assert_eq!(error.code(), "transport_error");
    Ok(())
}
