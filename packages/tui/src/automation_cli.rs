use crate::runtime_client::RuntimeClient;
use serde_json::{json, Value};

pub(crate) const HELP: &str = "centa ssh connect <host>\ncenta ssh exec <host> <remote-command>\ncenta schedule list [cursor]\ncenta schedule create <spec.json> <operationId>\ncenta schedule update <scheduleId> <revision> <spec.json>\ncenta schedule pause|resume|delete <scheduleId>\ncenta schedule history <scheduleId> [cursor]\ncenta schedule run <scheduleId> <operationId>\ncenta schedule service start|stop";

pub(crate) fn schedule_request(args: &[String]) -> Result<Value, String> {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    match args.as_slice() {
        [] | ["list"] => Ok(json!({"action":"list"})),
        ["list", cursor] => Ok(json!({"action":"list","cursor":cursor})),
        ["history", id, cursor] => Ok(json!({"action":"history","scheduleId":id,"cursor":cursor})),
        ["create", path, operation] => {
            Ok(json!({"action":"create","operationId":operation,"spec":read_spec(path)?}))
        }
        ["update", id, revision, path] => Ok(
            json!({"action":"update","scheduleId":id,"expectedRevision":revision.parse::<u64>().map_err(|e|e.to_string())?,"spec":read_spec(path)?}),
        ),
        [action @ ("pause" | "resume" | "delete" | "history"), id] => {
            Ok(json!({"action":action,"scheduleId":id}))
        }
        ["run", id, operation] => {
            Ok(json!({"action":"run","scheduleId":id,"operationId":operation}))
        }
        ["service", action @ ("start" | "stop")] => {
            Ok(json!({"action":"service","enabled":*action == "start"}))
        }
        _ => Err(HELP.into()),
    }
}
fn read_spec(path: &str) -> Result<Value, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
fn connect() -> Result<RuntimeClient, String> {
    let mut client = RuntimeClient::start()?;
    let descriptor = client.request("initialize", json!({"request":{"clientKind":"tui","viewerId":format!("cli-{}", std::process::id())}})).map_err(|e|e.to_string())?;
    client.validate_initialize_descriptor(&descriptor)?;
    Ok(client)
}
pub(crate) fn run(args: &[String]) -> Result<i32, String> {
    let Some(first) = args.first() else {
        return Err(HELP.into());
    };
    if matches!(first.as_str(), "--help" | "help") {
        println!("{HELP}");
        return Ok(0);
    }
    if first == "ssh" && args.get(1).is_some_and(|a| a == "connect") && args.len() == 3 {
        let host = &args[2];
        centaeris_runtime::openssh::command_args(host, "validate")?;
        // Before entering raw mode, inherit the real terminal and native prompts.
        let status = std::process::Command::new("ssh")
            .args(["--", host])
            .status()
            .map_err(|e| format!("start system OpenSSH: {e}"))?;
        return Ok(status.code().unwrap_or(1));
    }
    let (method, request) = if first == "schedule" {
        ("schedule_manage", schedule_request(&args[1..])?)
    } else if first == "ssh" && args.get(1).is_some_and(|a| a == "exec") && args.len() == 4 {
        centaeris_runtime::openssh::command_args(&args[2], &args[3])?;
        let mut client = connect()?;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|e| e.to_string())?;
        let operation = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let session = client.request("session/new", json!({"request":{"operationId":format!("ssh-{operation}"),"cwd":std::env::current_dir().map_err(|e|e.to_string())?,"title":format!("SSH: {}",args[2])}})).map_err(|e|e.to_string())?;
        let list = client
            .request(
                "process_session_list",
                json!({"request":{"sessionId":session["id"]}}),
            )
            .map_err(|e| e.to_string())?;
        let result = client.request("ssh_start",json!({"request":{"sessionId":session["id"],"serviceInstanceId":list["serviceInstanceId"],"operationId":operation,"destination":args[2],"command":args[3],"timeoutMs":0}})).map_err(|e|e.to_string())?;
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
        );
        return Ok(0);
    } else {
        return Err(HELP.into());
    };
    let result = connect()?
        .request(method, json!({"request":request}))
        .map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_have_explicit_service_and_retry_identity() {
        let args = |s: &str| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            schedule_request(&args("service stop")).unwrap(),
            json!({"action":"service","enabled":false})
        );
        assert_eq!(
            schedule_request(&args("run task op")).unwrap()["operationId"],
            "op"
        );
        assert!(schedule_request(&args("run task")).is_err());
        assert!(schedule_request(&args("service maybe")).is_err());
    }
}
