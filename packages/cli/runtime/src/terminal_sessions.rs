//! User terminals are Host resources, independent of Agent runs and pipe tasks.
use base64::{engine::general_purpose::STANDARD, Engine};
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

const OUTPUT_CAP: usize = 1024 * 1024;
#[derive(Default)]
struct Log {
    chunks: VecDeque<(u64, Vec<u8>)>,
    next: u64,
    bytes: usize,
}
impl Log {
    fn append(&mut self, bytes: &[u8]) {
        for part in bytes.chunks(4096) {
            self.chunks.push_back((self.next, part.to_vec()));
            self.next += 1;
            self.bytes += part.len();
            while self.bytes > OUTPUT_CAP || self.chunks.len() > 4096 {
                self.bytes -= self.chunks.pop_front().unwrap().1.len();
            }
        }
    }
    fn first(&self) -> u64 {
        self.chunks.front().map_or(self.next, |c| c.0)
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Start {
    operation_id: String,
    service_instance_id: String,
    workspace_root: String,
    session_id: Option<String>,
    shell: Option<String>,
    cols: u16,
    rows: u16,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Target {
    terminal_id: String,
    service_instance_id: String,
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
enum Request {
    List {
        #[serde(rename = "workspaceRoot")]
        workspace_root: String,
    },
    Start {
        request: Start,
    },
    Read {
        target: Target,
        cursor: String,
    },
    Write {
        target: Target,
        #[serde(rename = "dataBase64")]
        data_base64: String,
    },
    Resize {
        target: Target,
        cols: u16,
        rows: u16,
    },
    Stop {
        target: Target,
    },
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    terminal_id: String,
    workspace_root: String,
    session_id: Option<String>,
    shell: String,
    state: &'static str,
    exit_code: Option<u32>,
    output_complete: bool,
    error: Option<String>,
}
struct Entry {
    snapshot: Mutex<Snapshot>,
    log: Mutex<Log>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    input: mpsc::SyncSender<Vec<u8>>,
    stop: AtomicBool,
}
#[derive(Default)]
struct Registry {
    entries: HashMap<String, Arc<Entry>>,
    order: VecDeque<String>,
    receipts: crate::local_work_store::Receipts,
    closing: bool,
}
#[derive(Default)]
pub(crate) struct Manager {
    registry: Mutex<Registry>,
}
pub(crate) fn manager() -> &'static Manager {
    static M: OnceLock<Manager> = OnceLock::new();
    M.get_or_init(Manager::default)
}
fn instance() -> &'static str {
    crate::process_sessions::manager().service_instance_id()
}
fn size(cols: u16, rows: u16) -> Result<PtySize, String> {
    if cols == 0 || rows == 0 || cols > 500 || rows > 300 {
        return Err("terminal_invalid_size".into());
    }
    Ok(PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    })
}
fn workspace(root: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(root);
    if !p.is_absolute() || !p.is_dir() {
        return Err("terminal_workspace_unavailable".into());
    }
    p.canonicalize().map_err(|e| e.to_string())
}
fn shell(requested: &Option<String>) -> Result<String, String> {
    if let Some(s) = requested {
        if s.trim().is_empty() || s.len() > 4096 {
            return Err("terminal_invalid_shell".into());
        }
        return Ok(s.clone());
    }
    if let Ok(s) = std::env::var("CENTAERIS_TERMINAL_SHELL") {
        if !s.trim().is_empty() {
            return Ok(s);
        }
    }
    #[cfg(windows)]
    {
        let found = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .map(|p| p.join("pwsh.exe"))
            .find(|p| p.is_file());
        Ok(found
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "powershell.exe".into()))
    }
    #[cfg(unix)]
    {
        let preferred = std::env::var("SHELL")
            .ok()
            .filter(|s| PathBuf::from(s).is_file());
        if let Some(s) = preferred {
            return Ok(s);
        }
        // Use the reentrant account lookup because Host requests run concurrently.
        let login = unsafe {
            let mut account: libc::passwd = std::mem::zeroed();
            let mut result = std::ptr::null_mut();
            let mut buffer = vec![0u8; 65536];
            let code = libc::getpwuid_r(
                libc::getuid(),
                &mut account,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            );
            if code != 0 || result.is_null() || account.pw_shell.is_null() {
                None
            } else {
                Some(
                    std::ffi::CStr::from_ptr(account.pw_shell)
                        .to_string_lossy()
                        .into_owned(),
                )
            }
        };
        Ok(login
            .filter(|s| PathBuf::from(s).is_file())
            .unwrap_or_else(|| "/bin/sh".into()))
    }
}
impl Manager {
    fn target(&self, target: &Target) -> Result<Arc<Entry>, String> {
        if target.service_instance_id != instance() {
            return Err("terminal_service_instance_changed".into());
        }
        self.registry
            .lock()
            .unwrap()
            .entries
            .get(&target.terminal_id)
            .cloned()
            .ok_or_else(|| "terminal_expired".into())
    }
    fn start(&self, request: Start) -> Result<serde_json::Value, String> {
        if request.service_instance_id != instance() {
            return Err("terminal_service_instance_changed".into());
        }
        if request.operation_id.trim().is_empty() || request.operation_id.len() > 128 {
            return Err("terminal_invalid_operation_id".into());
        }
        let mut r = self.registry.lock().unwrap();
        if r.closing {
            return Err("terminal_service_stopping".into());
        }
        let namespace = format!("terminal:{}", instance());
        let digest = crate::operation_receipts::request_digest(&request)?;
        if let Some(result) = r
            .receipts
            .get(&namespace, &request.operation_id, &digest)
            .map_err(|e| {
                if e == "operation request conflict" {
                    "terminal_operation_conflict".into()
                } else {
                    e
                }
            })?
        {
            let id = result?;
            return r
                .entries
                .get(&id)
                .map(|e| serde_json::json!(*e.snapshot.lock().unwrap()))
                .ok_or_else(|| "terminal_expired".into());
        }
        let max_active = std::env::var("CENTAERIS_TERMINAL_MAX_ACTIVE")
            .ok()
            .map(|s| {
                s.parse::<usize>()
                    .map_err(|_| "invalid CENTAERIS_TERMINAL_MAX_ACTIVE".to_string())
            })
            .transpose()?
            .unwrap_or(8);
        if max_active == 0 {
            return Err("CENTAERIS_TERMINAL_MAX_ACTIVE must be positive".into());
        }
        if r.entries
            .values()
            .filter(|e| e.snapshot.lock().unwrap().state != "exited")
            .count()
            >= max_active
        {
            return Err("terminal_capacity_reached".into());
        }
        while r.entries.len() >= 64.max(max_active) {
            let Some(i) = r.order.iter().position(|id| {
                let s = r.entries[id].snapshot.lock().unwrap();
                s.state == "exited" && s.output_complete
            }) else {
                return Err("terminal_capacity_reached".into());
            };
            let id = r.order.remove(i).unwrap();
            r.entries.remove(&id);
        }
        r.receipts.put(
            &namespace,
            &request.operation_id,
            &digest,
            &Err("terminal_start_outcome_unknown".into()),
        )?;
        let result = (|| {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|e| e.to_string())?;
            let id = random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            let cwd = workspace(&request.workspace_root)?;
            let shell = shell(&request.shell)?;
            let pair = portable_pty::native_pty_system()
                .openpty(size(request.cols, request.rows)?)
                .map_err(|e| e.to_string())?;
            let mut command = CommandBuilder::new(&shell);
            command.cwd(&cwd);
            command.env("TERM", "xterm-256color");
            #[cfg(unix)]
            command.arg("-l");
            // Detached Windows service launch inherits an ignore-Ctrl+C flag.
            // Clear that inheritable flag before creating an interactive shell;
            // this does not remove any registered service control handlers.
            #[cfg(windows)]
            if unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(None, 0) } == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let mut child = pair
                .slave
                .spawn_command(command)
                .map_err(|e| e.to_string())?;
            drop(pair.slave);
            let setup = (|| {
                let tree = tree::Tree::new(child.as_mut())?;
                let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
                let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
                Ok::<_, String>((tree, reader, writer))
            })();
            let (tree, mut reader, mut writer) = match setup {
                Ok(v) => v,
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e);
                }
            };
            let (send, recv) = mpsc::sync_channel::<Vec<u8>>(32);
            let entry = Arc::new(Entry {
                snapshot: Mutex::new(Snapshot {
                    terminal_id: id.clone(),
                    workspace_root: cwd.to_string_lossy().into_owned(),
                    session_id: request.session_id.clone(),
                    shell,
                    state: "running",
                    exit_code: None,
                    output_complete: false,
                    error: None,
                }),
                log: Mutex::new(Log::default()),
                master: Mutex::new(Some(pair.master)),
                input: send,
                stop: AtomicBool::new(false),
            });
            let e = entry.clone();
            thread::spawn(move || {
                let mut buf = [0; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => e.log.lock().unwrap().append(&buf[..n]),
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                e.snapshot.lock().unwrap().output_complete = true;
            });
            let e = entry.clone();
            thread::spawn(move || {
                while !e.stop.load(Ordering::SeqCst) {
                    match recv.recv_timeout(Duration::from_millis(100)) {
                        Ok(bytes) => {
                            if let Err(err) = writer.write_all(&bytes).and_then(|_| writer.flush())
                            {
                                e.snapshot.lock().unwrap().error = Some(err.to_string());
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => break,
                    }
                }
            });
            let e = entry.clone();
            thread::spawn(move || loop {
                if e.stop.load(Ordering::SeqCst) {
                    if let Err(err) = tree.terminate() {
                        e.snapshot.lock().unwrap().error = Some(err);
                    }
                    let _ = child.kill();
                }
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if let Err(err) = tree.terminate() {
                            let mut snapshot = e.snapshot.lock().unwrap();
                            snapshot.error = Some(err);
                            snapshot.state = "stopping";
                            drop(snapshot);
                            e.stop.store(true, Ordering::SeqCst);
                            thread::sleep(Duration::from_millis(100));
                            continue;
                        }
                        e.stop.store(true, Ordering::SeqCst);
                        e.master.lock().unwrap().take();
                        let mut s = e.snapshot.lock().unwrap();
                        s.state = "exited";
                        s.exit_code = Some(status.exit_code());
                        break;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(25)),
                    Err(err) => {
                        e.snapshot.lock().unwrap().error = Some(err.to_string());
                        e.stop.store(true, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(25));
                    }
                }
            });
            r.order.push_back(id.clone());
            r.entries.insert(id.clone(), entry);
            Ok(id)
        })();
        r.receipts
            .put(&namespace, &request.operation_id, &digest, &result)?;
        result.map(|id| serde_json::json!(*r.entries[&id].snapshot.lock().unwrap()))
    }
    fn execute(&self, request: Request) -> Result<serde_json::Value, String> {
        match request {
            Request::Start { request } => self.start(request),
            Request::List { workspace_root } => {
                let cwd = workspace(&workspace_root)?.to_string_lossy().into_owned();
                let r = self.registry.lock().unwrap();
                let entries: Vec<_> = r
                    .order
                    .iter()
                    .filter_map(|id| {
                        let s = r.entries[id].snapshot.lock().unwrap();
                        (s.workspace_root == cwd).then(|| s.clone())
                    })
                    .collect();
                Ok(serde_json::json!({"serviceInstanceId":instance(),"terminals":entries}))
            }
            Request::Stop { target } => {
                let e = self.target(&target)?;
                e.stop.store(true, Ordering::SeqCst);
                let mut s = e.snapshot.lock().unwrap();
                if s.state == "running" {
                    s.state = "stopping";
                }
                Ok(serde_json::json!(*s))
            }
            Request::Resize { target, cols, rows } => {
                let e = self.target(&target)?;
                let m = e.master.lock().unwrap();
                m.as_ref()
                    .ok_or("terminal_exited")?
                    .resize(size(cols, rows)?)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({"accepted":true}))
            }
            Request::Write {
                target,
                data_base64,
            } => {
                if data_base64.len() > 24 * 1024 {
                    return Err("terminal_input_too_large".into());
                }
                let bytes = STANDARD
                    .decode(data_base64)
                    .map_err(|_| "terminal_invalid_input")?;
                let e = self.target(&target)?;
                if e.stop.load(Ordering::SeqCst) {
                    return Err("terminal_exited".into());
                }
                e.input
                    .send(bytes)
                    .map_err(|_| "terminal_input_not_accepted")?;
                Ok(serde_json::json!({"accepted":true}))
            }
            Request::Read { target, cursor } => {
                let e = self.target(&target)?;
                let cursor = cursor
                    .parse::<u64>()
                    .map_err(|_| "terminal_invalid_cursor")?;
                let log = e.log.lock().unwrap();
                if cursor > log.next {
                    return Err("terminal_invalid_cursor".into());
                }
                let chunks:Vec<_>=log.chunks.iter().filter(|(n,_)|*n>=cursor).take(16).map(|(n,b)|serde_json::json!({"cursor":n.to_string(),"dataBase64":STANDARD.encode(b)})).collect();
                let next = chunks.last().map_or(cursor.max(log.first()), |c| {
                    c["cursor"].as_str().unwrap().parse::<u64>().unwrap() + 1
                });
                Ok(
                    serde_json::json!({"terminal":*e.snapshot.lock().unwrap(),"chunks":chunks,"nextCursor":next.to_string(),"gap":cursor<log.first(),"hasMore":next<log.next}),
                )
            }
        }
    }
    pub(crate) fn has_active(&self) -> bool {
        self.registry
            .lock()
            .unwrap()
            .entries
            .values()
            .any(|e| e.snapshot.lock().unwrap().state != "exited")
    }
    pub(crate) fn begin_shutdown(&self) {
        let mut r = self.registry.lock().unwrap();
        r.closing = true;
        for e in r.entries.values() {
            e.stop.store(true, Ordering::SeqCst);
        }
    }
    pub(crate) fn shutdown(&self) -> Result<(), String> {
        self.begin_shutdown();
        let end = Instant::now() + Duration::from_secs(5);
        while self.has_active() {
            if Instant::now() >= end {
                return Err("terminal_shutdown_not_confirmed".into());
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    }
}
pub(crate) fn handle(
    payload: serde_json::Value,
) -> Result<serde_json::Value, crate::errors::RuntimeHostError> {
    let request = crate::runtime_bridge::deserialize_request(payload)?;
    manager()
        .execute(request)
        .map_err(|e| crate::errors::RuntimeHostError::new("terminal_failed", e))
}
#[cfg(test)]
mod tests;
mod tree;
