//! Private sealed-runner inputs. Authority and durable receipts stay on the host.
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunnerRequest {
    pub action: RunnerAction,
    pub task_id: String,
    pub process: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RunnerAction {
    Prepare,
    Start,
    Remove,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RunnerEvent {
    Ready { pid: u32 },
    LeaderExited { wait_status: i32 },
}

impl RunnerRequest {
    pub fn validate(&self) -> Result<(), String> {
        if !self.task_id.strip_prefix("task-").is_some_and(|id| {
            id.len() == 64
                && id
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        }) || (self.action == RunnerAction::Start) != self.process.is_some()
        {
            return Err("runsc_runner_request_invalid".into());
        }
        if let Some(process) = &self.process {
            crate::host_supervisor::task_process(
                process,
                "/bin/true",
                &[],
                "/",
                &Default::default(),
            )?;
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::run;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_control_cannot_select_paths_or_replace_an_administrative_action_with_a_process() {
        let mut request = RunnerRequest {
            action: RunnerAction::Prepare,
            task_id: format!("task-{}", "a".repeat(64)),
            process: None,
        };
        request.validate().unwrap();
        request.task_id = "../other-owner".into();
        assert!(request.validate().is_err());
        request.task_id = format!("task-{}", "a".repeat(64));
        request.process = Some(serde_json::json!({}));
        assert!(request.validate().is_err());
        let mut wire = serde_json::to_value(request).unwrap();
        wire["controlRoot"] = serde_json::json!("/host");
        assert!(serde_json::from_value::<RunnerRequest>(wire).is_err());
    }
}
