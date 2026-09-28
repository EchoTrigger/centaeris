pub(crate) mod view;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReviewRequest {
    pub workspace_root: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Snapshot {
    pub head: Option<String>,
    pub index_fingerprint: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct File {
    pub path: String,
    pub original_path: Option<String>,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Review {
    pub workspace_root: String,
    pub branch: Option<String>,
    pub snapshot: Snapshot,
    pub staged: Vec<File>,
    pub unstaged: Vec<File>,
    pub untracked: Vec<File>,
    pub conflicts: Vec<File>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PathRequest {
    pub workspace_root: String,
    pub path: String,
    pub expected: Snapshot,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CommitRequest {
    pub workspace_root: String,
    pub message: String,
    pub expected: Snapshot,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DiffSide {
    Staged,
    Unstaged,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DiffRequest {
    pub workspace_root: String,
    pub path: String,
    pub side: DiffSide,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CommitResult {
    pub commit_sha: String,
    pub output: String,
}

use centaeris_runtime::local_execution_host::capture_application_command;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock, Weak},
};

const OUTPUT_LIMIT: usize = 1024 * 1024;
static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();

fn repo_lock(root: &Path) -> Result<Arc<Mutex<()>>, String> {
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "workspace_git_lock_poisoned")?;
    locks.retain(|_, value| value.strong_count() > 0);
    let lock = locks.entry(root.to_path_buf()).or_default();
    if let Some(lock) = lock.upgrade() {
        return Ok(lock);
    }
    let next = Arc::new(Mutex::new(()));
    *lock = Arc::downgrade(&next);
    Ok(next)
}

fn capture(
    root: &Path,
    args: &[&str],
    input: &[u8],
) -> Result<
    (
        centaeris_runtime::local_execution_host::ApplicationCommandCapture,
        bool,
    ),
    String,
> {
    let mut command = Command::new("git");
    command
        .args([
            "--no-pager",
            "--literal-pathspecs",
            "-c",
            "core.quotepath=false",
        ])
        .args(args)
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(name);
    }
    capture_application_command(&mut command, input, 15_000, OUTPUT_LIMIT)
}

fn checked(
    root: &Path,
    args: &[&str],
    input: &[u8],
    allow_truncated: bool,
) -> Result<(String, bool), String> {
    let (output, truncated) = capture(root, args, input)?;
    if output.timed_out {
        return Err(
            "workspace_git_timed_out: outcome may be unknown; refresh before another action".into(),
        );
    }
    if output.exit_code != Some(0) {
        return Err(format!(
            "workspace_git_command_failed: {}{}",
            output.stderr, output.stdout
        ));
    }
    if truncated && !allow_truncated {
        return Err("workspace_git_output_limit: cannot use an incomplete result".into());
    }
    Ok((output.stdout, truncated))
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    Ok(checked(root, args, &[], false)?.0)
}

fn root(raw: &str) -> Result<PathBuf, String> {
    let root = super::resolve_cwd(raw)?;
    let top = git(&root, &["rev-parse", "--show-toplevel"])?;
    let top = std::fs::canonicalize(top.trim()).map_err(|error| error.to_string())?;
    if top != root {
        return Err("workspace_git_open_repository_root: select the repository root before using Git review".into());
    }
    Ok(root)
}

fn snapshot(root: &Path) -> Result<Snapshot, String> {
    let (output, truncated) = capture(root, &["rev-parse", "--verify", "--quiet", "HEAD"], &[])?;
    if output.timed_out || truncated {
        return Err("workspace_git_head_unavailable".into());
    }
    let head = match output.exit_code {
        Some(0) => Some(output.stdout.trim().into()),
        Some(1) => None,
        _ => return Err(output.stderr),
    };
    let index = git(root, &["ls-files", "--stage", "-z"])?;
    Ok(Snapshot {
        head,
        index_fingerprint: format!("{:x}", Sha256::digest(index.as_bytes())),
    })
}

fn read_review(root: &Path) -> Result<Review, String> {
    let before = snapshot(root)?;
    let output = git(
        root,
        &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
    )?;
    let branch = git(root, &["branch", "--show-current"])?;
    let mut result = Review {
        workspace_root: super::display_path(root),
        branch: if branch.trim().is_empty() {
            None
        } else {
            Some(branch.trim().into())
        },
        snapshot: before.clone(),
        staged: vec![],
        unstaged: vec![],
        untracked: vec![],
        conflicts: vec![],
    };
    let mut records = output.split('\0').filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        let kind = record.as_bytes()[0];
        if kind == b'?' {
            result.untracked.push(File {
                path: record
                    .strip_prefix("? ")
                    .ok_or("workspace_git_invalid_status")?
                    .into(),
                original_path: None,
                status: "?".into(),
            });
            continue;
        }
        let columns = match kind {
            b'1' => 9,
            b'2' => 10,
            b'u' => 11,
            _ => return Err("workspace_git_invalid_status".into()),
        };
        let parts: Vec<_> = record.splitn(columns, ' ').collect();
        if parts.len() != columns || parts[1].len() != 2 {
            return Err("workspace_git_invalid_status".into());
        }
        let original = if kind == b'2' {
            Some(
                records
                    .next()
                    .ok_or("workspace_git_missing_rename_source")?
                    .to_string(),
            )
        } else {
            None
        };
        let file = |status: u8| File {
            path: parts[columns - 1].into(),
            original_path: original.clone(),
            status: (status as char).to_string(),
        };
        if kind == b'u' {
            result.conflicts.push(file(b'U'));
            continue;
        }
        let xy = parts[1].as_bytes();
        if xy[0] != b'.' {
            result.staged.push(file(xy[0]));
        }
        if xy[1] != b'.' {
            result.unstaged.push(file(xy[1]));
        }
    }
    if snapshot(root)? != before {
        return Err("workspace_git_snapshot_changed: refresh review".into());
    }
    Ok(result)
}

pub(crate) fn review(request: ReviewRequest) -> Result<Review, String> {
    let root = root(&request.workspace_root)?;
    let lock = repo_lock(&root)?;
    let _guard = lock
        .try_lock()
        .map_err(|_| "workspace_git_busy: another workspace Git operation is running")?;
    read_review(&root)
}

fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.contains('\0')
        || path.starts_with(['/', '\\'])
        || path.as_bytes().get(1) == Some(&b':')
        || path
            .split(['/', '\\'])
            .any(|part| part == ".." || part == "." || part.eq_ignore_ascii_case(".git"))
    {
        return Err("workspace_git_invalid_relative_path".into());
    }
    Ok(())
}

fn check_expected(actual: &Snapshot, expected: &Snapshot) -> Result<(), String> {
    if actual != expected {
        return Err(
            "workspace_git_snapshot_changed: HEAD or staged contents changed; refresh review"
                .into(),
        );
    }
    Ok(())
}

fn change_index(request: PathRequest, staging: bool) -> Result<Review, String> {
    validate_path(&request.path)?;
    let root = root(&request.workspace_root)?;
    let lock = repo_lock(&root)?;
    let _guard = lock
        .try_lock()
        .map_err(|_| "workspace_git_busy: another workspace Git operation is running")?;
    let current = read_review(&root)?;
    check_expected(&current.snapshot, &request.expected)?;
    if !current.conflicts.is_empty() {
        return Err("workspace_git_unmerged: resolve conflicts with Git first".into());
    }
    let entry = if staging {
        current
            .unstaged
            .iter()
            .chain(&current.untracked)
            .find(|entry| entry.path == request.path)
    } else {
        current
            .staged
            .iter()
            .find(|entry| entry.path == request.path)
    }
    .ok_or("workspace_git_path_not_in_selected_changes")?;
    let mut paths = vec![entry.path.as_str()];
    if !staging || matches!(entry.status.as_str(), "R" | "C") {
        if let Some(original) = entry.original_path.as_deref() {
            validate_path(original)?;
            paths.push(original);
        }
    }
    if staging {
        let mut args = vec!["add", "--"];
        args.extend(paths);
        git(&root, &args)?;
    } else if current.snapshot.head.is_none() {
        let mut args = vec!["rm", "--cached", "--force", "--"];
        args.extend(paths);
        git(&root, &args)?;
    } else {
        let mut args = vec!["restore", "--staged", "--"];
        args.extend(paths);
        git(&root, &args)?;
    }
    read_review(&root)
}

pub(crate) fn stage(request: PathRequest) -> Result<Review, String> {
    change_index(request, true)
}
pub(crate) fn unstage(request: PathRequest) -> Result<Review, String> {
    change_index(request, false)
}

pub(crate) fn file_diff(
    request: DiffRequest,
) -> Result<super::WorkspaceGitFileDiffResponse, String> {
    validate_path(&request.path)?;
    let root = root(&request.workspace_root)?;
    let lock = repo_lock(&root)?;
    let _guard = lock.try_lock().map_err(|_| "workspace_git_busy")?;
    let current = read_review(&root)?;
    let entries = match request.side {
        DiffSide::Staged => &current.staged,
        DiffSide::Unstaged => &current.unstaged,
    };
    let entry = entries
        .iter()
        .find(|entry| entry.path == request.path)
        .ok_or("workspace_git_diff_no_longer_available: refresh review")?;
    let mut args = vec!["diff", "--no-ext-diff", "--no-textconv", "--no-color"];
    if matches!(request.side, DiffSide::Staged) {
        args.push("--cached");
    }
    args.extend(["--", entry.path.as_str()]);
    if let Some(original) = entry.original_path.as_deref() {
        args.push(original);
    }
    let (diff_preview, truncated) = checked(&root, &args, &[], true)?;
    Ok(super::WorkspaceGitFileDiffResponse {
        workspace_root: super::display_path(&root),
        path: request.path,
        diff_preview,
        truncated,
    })
}

pub(crate) fn commit(request: CommitRequest) -> Result<CommitResult, String> {
    if request.message.trim().is_empty()
        || request.message.len() > 64 * 1024
        || request.message.contains('\0')
    {
        return Err("workspace_git_invalid_commit_message".into());
    }
    let root = root(&request.workspace_root)?;
    let lock = repo_lock(&root)?;
    let _guard = lock
        .try_lock()
        .map_err(|_| "workspace_git_busy: another workspace Git operation is running")?;
    let current = read_review(&root)?;
    check_expected(&current.snapshot, &request.expected)?;
    if !current.conflicts.is_empty() {
        return Err("workspace_git_unmerged: resolve conflicts with Git first".into());
    }
    if current.staged.is_empty() {
        return Err("workspace_git_nothing_staged".into());
    }
    let git_dir = git(&root, &["rev-parse", "--absolute-git-dir"])?;
    let git_dir = Path::new(git_dir.trim());
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        if git_dir
            .join(marker)
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            return Err(
                "workspace_git_operation_in_progress: finish the operation with Git CLI".into(),
            );
        }
    }
    // Ordinary Git commit retains user identity, hooks and signing. This check
    // detects already-stale reviews; it does not lock out external Git writers.
    let output = checked(
        &root,
        &["commit", "--file=-"],
        request.message.as_bytes(),
        true,
    )?
    .0;
    let commit_sha = git(&root, &["rev-parse", "--verify", "HEAD"])?
        .trim()
        .to_string();
    let parents = git(&root, &["rev-list", "--parents", "-n", "1", &commit_sha])?;
    let first_parent = parents.split_whitespace().nth(1).map(str::to_string);
    if first_parent != request.expected.head {
        return Err("workspace_git_commit_outcome_changed: HEAD moved concurrently; inspect history before another commit".into());
    }
    Ok(CommitResult { commit_sha, output })
}

#[cfg(test)]
mod tests;
