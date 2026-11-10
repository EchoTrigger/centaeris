//! Run with the configured local profile, or an isolated profile selected by
//! CENTAERIS_DESKTOP_DATA_DIR. An optional argument supplies the prompt.
use centaeris_agent::{SessionCreateCommandRequest, SessionPrompt};
use centaeris_desktop_services::{dispatch_execution_helper, LocalAgentHost};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match dispatch_execution_helper() {
        Ok(Some(exit_code)) => std::process::exit(exit_code),
        Ok(None) => {}
        Err(error) => {
            eprintln!(
                "centaeris Windows host launcher failed: {}",
                error.message()
            );
            std::process::exit(1);
        }
    }
    let host = LocalAgentHost::start().await?;
    let result = async {
        let mut client = host.connect(&format!("native-example-{}", std::process::id()))?;
        let session = client
            .create_session(SessionCreateCommandRequest {
                operation_id: format!("native-example-{}", std::process::id()).try_into()?,
                title: Some("Native SDK".to_string()),
                cwd: std::env::current_dir()?.to_string_lossy().into_owned(),
            })
            .await?;
        if let Some(message) = std::env::args().nth(1) {
            let accepted = session
                .send(SessionPrompt::new(
                    format!("native-prompt-{}", std::process::id()).try_into()?,
                    message,
                ))
                .await?;
            session.subscribe(Some(&accepted.agent_run_id)).await?;
            let completed = client.wait_for_run(&accepted.agent_run_id).await?;
            if completed.status != "succeeded" {
                return Err(format!("Run ended with status {}", completed.status).into());
            }
        }
        let loaded = session.load().await?;
        println!("{}", serde_json::to_string(&loaded)?);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let shutdown = host.shutdown().await;
    result?;
    shutdown?;
    Ok(())
}
