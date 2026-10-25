//! Reuse one idle maintenance SDK process without serializing independent operations.
use crate::e2b_adapter::bulk::SpoolingAdapterProcess;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(super) struct PowerAdapterCache {
    idle: Mutex<Option<Arc<SpoolingAdapterProcess>>>,
}

impl PowerAdapterCache {
    pub(super) fn take(&self) -> Option<Arc<SpoolingAdapterProcess>> {
        let process = { self.idle.lock().ok()?.take()? };
        // Inspect and discard stale children outside the cache lock, before any
        // request is sent. This never retries a dispatched or Unknown operation.
        process.is_running().then_some(process)
    }

    pub(super) fn put(&self, process: Arc<SpoolingAdapterProcess>) -> bool {
        let mut unused = Some(process);
        if let Ok(mut idle) = self.idle.lock() {
            if idle.is_none() {
                *idle = unused.take();
            }
        }
        let retained = unused.is_none();
        // Closing a surplus child may wait for IO threads. Never hold the cache
        // lock while doing so: another operation must remain free to borrow.
        drop(unused);
        retained
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2b_adapter::AdapterFailure;
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    fn launch() -> Arc<SpoolingAdapterProcess> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let python = std::env::var_os("E2B_ADAPTER_TEST_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                root.join(if cfg!(windows) {
                    ".venv/Scripts/python.exe"
                } else {
                    ".venv/bin/python"
                })
            });
        let mut command = Command::new(python);
        command.args(["-u", "-c", r#"
import json,os,sys,time
from pathlib import Path
for raw in sys.stdin:
 r=json.loads(raw)
 if r['operation']=='lose':
  Path(r['payload']['proof']).write_text('effect-entered')
  sys.exit(1)
 if r['operation']=='block':
  Path(r['payload']['entered']).write_text('entered')
  while not Path(r['payload']['release']).exists():time.sleep(.005)
 print(json.dumps({'schema':'runtime.e2b.adapter.response.v1','requestId':r['requestId'],'outcome':'succeeded','result':{'pid':os.getpid(),'operation':r['operation']}}),flush=True)
"#]);
        Arc::new(SpoolingAdapterProcess::spawn(command).unwrap())
    }
    fn call(process: &SpoolingAdapterProcess, id: &str, op: &str, payload: Value) -> Value {
        process
            .call(id, op, payload, Duration::from_secs(5))
            .unwrap()
            .result
            .unwrap()
    }
    #[test]
    fn sequential_power_operations_reuse_the_same_sdk_process() {
        let cache = PowerAdapterCache::default();
        let first = cache.take().unwrap_or_else(launch);
        let before = call(&first, "pause-a", "pause", json!({}));
        cache.put(first);
        let second = cache.take().unwrap_or_else(launch);
        let after = call(&second, "resume-b", "connect", json!({}));
        assert_eq!(before["pid"], after["pid"]);
        assert_eq!(after["operation"], "connect");
    }
    #[cfg(windows)]
    #[test]
    fn externally_exited_idle_child_is_replaced_before_the_next_power_request() {
        let cache = PowerAdapterCache::default();
        let first = launch();
        let before = call(&first, "pause-before-exit", "pause", json!({}));
        let pid = before["pid"].as_u64().unwrap();
        assert!(cache.put(first));
        // Kill only this fixture's child, independently of AdapterProcess::close,
        // and wait for exit so the assertion is not a process-scheduling race.
        let status = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!("$ownedChild = Get-Process -Id {pid} -ErrorAction Stop; $ownedChild.Kill(); $ownedChild.WaitForExit()"))
            .status()
            .unwrap();
        assert!(status.success());
        let next = cache.take().unwrap_or_else(launch);
        let after = call(&next, "resume-after-exit", "connect", json!({}));
        assert_ne!(before["pid"], after["pid"]);
        assert_eq!(after["operation"], "connect");
    }

    #[test]
    fn borrowed_power_process_does_not_block_an_independent_control_lane() {
        let cache = PowerAdapterCache::default();
        let first = cache.take().unwrap_or_else(launch);
        let directory = tempfile::tempdir().unwrap();
        let entered = directory.path().join("entered");
        let release = directory.path().join("release");
        let owned = first.clone();
        let input = json!({"entered":entered,"release":release});
        let blocked = std::thread::spawn(move || call(&owned, "busy", "block", input));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !entered.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(entered.exists());
        let second = cache.take().unwrap_or_else(launch);
        let independent = call(&second, "independent", "inspect", json!({}));
        std::fs::write(release, b"release").unwrap();
        let first_reply = blocked.join().unwrap();
        assert_ne!(independent["pid"], first_reply["pid"]);
        cache.put(first);
        cache.put(second);
        let retained = cache.take().expect("one reusable child");
        assert!(cache.take().is_none(), "idle cache must remain bounded");
        assert_eq!(
            call(&retained, "later", "pause", json!({}))["pid"],
            first_reply["pid"]
        );
    }
    #[test]
    fn lost_reply_is_not_replayed_and_a_later_operation_uses_a_new_child() {
        let cache = PowerAdapterCache::default();
        let process = cache.take().unwrap_or_else(launch);
        let proof = tempfile::tempdir().unwrap();
        let path = proof.path().join("sent");
        let result = process.call(
            "lost",
            "lose",
            json!({"proof":path}),
            Duration::from_secs(5),
        );
        assert!(matches!(result, Err(AdapterFailure::Unknown(_))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "effect-entered");
        // The caller discards an unconfirmed lane, never puts it back or replays it.
        drop(process);
        assert!(cache.take().is_none());
        let next = launch();
        assert_eq!(
            call(&next, "new-authorized-operation", "pause", json!({}))["operation"],
            "pause"
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "effect-entered");
    }
    #[test]
    fn dropping_runtime_cache_closes_its_idle_sdk_child() {
        let cache = PowerAdapterCache::default();
        let process = launch();
        let weak = Arc::downgrade(&process);
        cache.put(process);
        assert!(weak.upgrade().is_some());
        drop(cache);
        assert!(weak.upgrade().is_none());
    }
}
