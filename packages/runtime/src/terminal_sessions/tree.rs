#[cfg(windows)]
pub(super) struct Tree(centaeris_runtime::local_execution_host::WindowsProcessJob);
#[cfg(windows)]
impl Tree {
    pub fn new(child: &mut dyn portable_pty::Child) -> Result<Self, String> {
        let job = centaeris_runtime::local_execution_host::WindowsProcessJob::new()
            .map_err(|e| e.to_string())?;
        let raw = child
            .as_raw_handle()
            .ok_or("terminal_process_handle_missing")?;
        // SAFETY: the Child remains alive and owns this process handle throughout assignment.
        let handle = unsafe { std::os::windows::io::BorrowedHandle::borrow_raw(raw) };
        job.assign_process_handle(handle)
            .map_err(|e| e.to_string())?;
        Ok(Self(job))
    }
    pub fn terminate(&self) -> Result<(), String> {
        self.0.terminate().map_err(|e| e.to_string())
    }
}
#[cfg(unix)]
pub(super) struct Tree(u32);
#[cfg(unix)]
impl Tree {
    pub fn new(child: &mut dyn portable_pty::Child) -> Result<Self, String> {
        Ok(Self(child.process_id().ok_or("terminal_pid_missing")?))
    }
    pub fn terminate(&self) -> Result<(), String> {
        // A shell creates multiple foreground process groups within its PTY session.
        let output = std::process::Command::new("ps")
            .args(["-axo", "pid="])
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err("terminal_session_cleanup_failed".into());
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Ok(pid) = line.trim().parse::<i32>() {
                // SAFETY: these syscalls only signal members of our PTY's session.
                if unsafe { libc::getsid(pid) } == self.0 as i32
                    && unsafe { libc::kill(pid, libc::SIGKILL) } != 0
                {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error.to_string());
                    }
                }
            }
        }
        Ok(())
    }
}
