use super::*;
use crate::runsc_task::RunnerAction;

fn virtual_target(launcher: &Option<Arc<TaskLaunchTarget>>) -> Option<&OciTarget> {
    launcher.as_deref().and_then(TaskLaunchTarget::runsc_target)
}

pub(super) fn initialize(launcher: &TaskLaunchTarget, root: &Path) -> Result<(), String> {
    if let Some(target) = launcher.runsc_target() {
        runsc::initialize(target)?;
        if !root.exists() {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root)
                .map_err(|_| "task_cgroup_create_failed")?;
        }
        crate::host_supervisor::linux::protected(root, true)
    } else {
        verify_domain(root.parent().ok_or("task_cgroup_path_invalid")?)?;
        if !root.exists() {
            fs::create_dir(root).map_err(|_| "task_cgroup_create_failed")?;
        }
        verify_domain(root)
    }
}

pub(super) fn create(
    launcher: &Option<Arc<TaskLaunchTarget>>,
    root: &Path,
    path: &Path,
) -> Result<Option<File>, String> {
    if let Some(target) = virtual_target(launcher) {
        crate::host_supervisor::linux::protected(root, true)?;
        use std::os::unix::fs::DirBuilderExt;
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                crate::host_supervisor::linux::protected(path, true)?
            }
            Err(_) => return Err("task_cgroup_create_failed".into()),
        }
        runsc::administer(target, path, RunnerAction::Prepare)?;
        Ok(None)
    } else {
        verify_domain(root)?;
        fs::create_dir(path).map_err(|_| "task_cgroup_create_failed")?;
        verify_domain(path)?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join("cgroup.procs"))
            .map(Some)
            .map_err(|_| "task_cgroup_assignment_unavailable".into())
    }
}

pub(super) fn events(
    launcher: &Option<Arc<TaskLaunchTarget>>,
    path: &Path,
) -> Result<String, String> {
    if let Some(target) = virtual_target(launcher) {
        runsc::events(target, path)
    } else {
        fs::read_to_string(path.join("cgroup.events"))
            .map_err(|_| "task_tree_observation_failed".into())
    }
}

pub(super) fn kill(launcher: &Option<Arc<TaskLaunchTarget>>, path: &Path) -> Result<bool, String> {
    if let Some(target) = virtual_target(launcher) {
        runsc::kill(target, path)
    } else {
        cgroup_kill(path)
    }
}

pub(super) fn remove(launcher: &Option<Arc<TaskLaunchTarget>>, path: &Path) -> Result<(), String> {
    if let Some(target) = virtual_target(launcher) {
        runsc::administer(target, path, RunnerAction::Remove)?;
    }
    fs::remove_dir(path).map_err(|_| "task_history_retirement_pending".into())
}
