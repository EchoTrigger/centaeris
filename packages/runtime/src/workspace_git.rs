use crate::processes::configure_background_command;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

const WORKSPACE_GIT_DIFF_MAX_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceGitRootRequest {
    pub(crate) workspace_root: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceGitFileDiffRequest {
    pub(crate) workspace_root: String,
    pub(crate) path: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceGitChangedFile {
    pub(crate) path: String,
    pub(crate) status: String,
    pub(crate) added: u64,
    pub(crate) removed: u64,
    pub(crate) diff_available: bool,
    pub(crate) diff_unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceGitStatusResponse {
    pub(crate) workspace_root: String,
    pub(crate) branch: Option<String>,
    pub(crate) changed_files: Vec<WorkspaceGitChangedFile>,
    pub(crate) total_added: u64,
    pub(crate) total_removed: u64,
    pub(crate) is_git_repository: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceGitDiffResponse {
    pub(crate) workspace_root: String,
    pub(crate) diff_preview: String,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceGitFileDiffResponse {
    pub(crate) workspace_root: String,
    pub(crate) path: String,
    pub(crate) diff_preview: String,
    pub(crate) truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WorkspaceGitHubCliStatusResponse {
    pub(crate) available: bool,
    pub(crate) summary: String,
}

pub(crate) fn status(
    request: WorkspaceGitRootRequest,
) -> Result<WorkspaceGitStatusResponse, String> {
    let workspace_root = resolve_cwd(request.workspace_root.as_str())?;
    ensure_git_repository(workspace_root.as_path())?;
    let branch = current_branch(workspace_root.as_path())?;
    let stat_by_path = diff_numstat(workspace_root.as_path())?;
    let changed_files = status_files(workspace_root.as_path(), &stat_by_path)?;
    let total_added = changed_files.iter().map(|file| file.added).sum();
    let total_removed = changed_files.iter().map(|file| file.removed).sum();
    Ok(WorkspaceGitStatusResponse {
        workspace_root: display_path(workspace_root.as_path()),
        branch,
        changed_files,
        total_added,
        total_removed,
        is_git_repository: true,
    })
}

pub(crate) fn diff(request: WorkspaceGitRootRequest) -> Result<WorkspaceGitDiffResponse, String> {
    let workspace_root = resolve_cwd(request.workspace_root.as_str())?;
    ensure_git_repository(workspace_root.as_path())?;
    let output = run_git(
        workspace_root.as_path(),
        &["diff", "--no-ext-diff", "HEAD", "--"],
    )?;
    let (diff_preview, truncated) = bounded_stdout(output.stdout.as_slice());
    Ok(WorkspaceGitDiffResponse {
        workspace_root: display_path(workspace_root.as_path()),
        diff_preview,
        truncated,
    })
}

pub(crate) fn file_diff(
    request: WorkspaceGitFileDiffRequest,
) -> Result<WorkspaceGitFileDiffResponse, String> {
    let workspace_root = resolve_cwd(request.workspace_root.as_str())?;
    ensure_git_repository(workspace_root.as_path())?;
    let relative_path = validate_relative_git_path(request.path.as_str())?;
    let output = run_git(
        workspace_root.as_path(),
        &[
            "diff",
            "--no-ext-diff",
            "HEAD",
            "--",
            relative_path.as_str(),
        ],
    )?;
    let (diff_preview, truncated) = bounded_stdout(output.stdout.as_slice());
    Ok(WorkspaceGitFileDiffResponse {
        workspace_root: display_path(workspace_root.as_path()),
        path: relative_path,
        diff_preview,
        truncated,
    })
}

pub(crate) fn github_cli_status() -> WorkspaceGitHubCliStatusResponse {
    let mut command = Command::new("gh");
    configure_background_command(&mut command);
    match command.arg("--version").output() {
        Ok(output) if output.status.success() => WorkspaceGitHubCliStatusResponse {
            available: true,
            summary: String::from("GitHub CLI 可用"),
        },
        Ok(_) => WorkspaceGitHubCliStatusResponse {
            available: false,
            summary: String::from("GitHub CLI 不可用"),
        },
        Err(_) => WorkspaceGitHubCliStatusResponse {
            available: false,
            summary: String::from("GitHub CLI 不可用"),
        },
    }
}

fn resolve_cwd(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(String::from("workspace_git_workspace_root_required"));
    }
    let normalized = trimmed.trim_start_matches(r"\\?\");
    let root = std::fs::canonicalize(normalized)
        .map_err(|error| format!("workspace_git_resolve_root_failed: {error}"))?;
    if !root.is_dir() {
        return Err(String::from("workspace_git_root_not_directory"));
    }
    Ok(root)
}

fn ensure_git_repository(workspace_root: &Path) -> Result<(), String> {
    let output = run_git(workspace_root, &["rev-parse", "--is-inside-work-tree"])?;
    let value = String::from_utf8_lossy(output.stdout.as_slice())
        .trim()
        .to_string();
    if value == "true" {
        return Ok(());
    }
    Err(String::from("workspace_git_not_repository"))
}

fn current_branch(workspace_root: &Path) -> Result<Option<String>, String> {
    let output = run_git(workspace_root, &["branch", "--show-current"])?;
    let branch = String::from_utf8_lossy(output.stdout.as_slice())
        .trim()
        .to_string();
    if !branch.is_empty() {
        return Ok(Some(branch));
    }
    let output = run_git(workspace_root, &["rev-parse", "--short", "HEAD"])?;
    let head = String::from_utf8_lossy(output.stdout.as_slice())
        .trim()
        .to_string();
    if head.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!("detached {head}")))
    }
}

fn diff_numstat(
    workspace_root: &Path,
) -> Result<std::collections::HashMap<String, (u64, u64)>, String> {
    let output = run_git(workspace_root, &["diff", "--numstat", "HEAD", "--"])?;
    let text = String::from_utf8_lossy(output.stdout.as_slice());
    let mut stats = std::collections::HashMap::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        let added = parse_numstat_count(parts.next());
        let removed = parse_numstat_count(parts.next());
        let Some(path) = parts.next() else {
            continue;
        };
        let normalized_path = normalize_git_output_path(path);
        if !normalized_path.is_empty() {
            stats.insert(normalized_path, (added, removed));
        }
    }
    Ok(stats)
}

fn status_files(
    workspace_root: &Path,
    stat_by_path: &std::collections::HashMap<String, (u64, u64)>,
) -> Result<Vec<WorkspaceGitChangedFile>, String> {
    let output = run_git(workspace_root, &["status", "--porcelain=v1"])?;
    let text = String::from_utf8_lossy(output.stdout.as_slice());
    let mut files = Vec::new();
    for line in text.lines() {
        if line.len() < 4 {
            continue;
        }
        let status_code = line[..2].trim().to_string();
        let raw_path = line[3..].trim();
        let path = normalize_status_path(raw_path);
        if path.is_empty() {
            continue;
        }
        let (added, removed) = stat_by_path.get(path.as_str()).copied().unwrap_or((0, 0));
        let is_untracked = status_code == "??";
        files.push(WorkspaceGitChangedFile {
            path,
            status: status_label(status_code.as_str()).to_string(),
            added,
            removed,
            diff_available: !is_untracked,
            diff_unavailable_reason: if is_untracked {
                Some(String::from("未跟踪"))
            } else {
                None
            },
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn run_git(workspace_root: &Path, args: &[&str]) -> Result<Output, String> {
    let mut command = Command::new("git");
    configure_background_command(&mut command);
    let output = command
        .args(args)
        .current_dir(workspace_root)
        .output()
        .map_err(|error| format!("workspace_git_executable_unavailable: {error}"))?;
    if output.status.success() {
        return Ok(output);
    }
    let stderr = String::from_utf8_lossy(output.stderr.as_slice())
        .trim()
        .to_string();
    let stdout = String::from_utf8_lossy(output.stdout.as_slice())
        .trim()
        .to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    Err(format!("workspace_git_command_failed: {detail}"))
}

fn bounded_stdout(bytes: &[u8]) -> (String, bool) {
    let truncated = bytes.len() > WORKSPACE_GIT_DIFF_MAX_BYTES;
    let slice = if truncated {
        &bytes[..WORKSPACE_GIT_DIFF_MAX_BYTES]
    } else {
        bytes
    };
    (
        String::from_utf8_lossy(slice)
            .trim_start_matches('\u{feff}')
            .to_string(),
        truncated,
    )
}

fn validate_relative_git_path(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(String::from("workspace_git_file_path_required"));
    }
    let normalized = trimmed.replace('\\', "/");
    let has_windows_drive = normalized.as_bytes().get(1) == Some(&b':')
        && normalized.as_bytes()[0].is_ascii_alphabetic();
    let path = PathBuf::from(normalized.as_str());
    if has_windows_drive
        || path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::ParentDir
            )
        })
    {
        return Err(String::from(
            "workspace_git_file_path_must_be_workspace_relative",
        ));
    }
    Ok(normalized)
}

fn parse_numstat_count(value: Option<&str>) -> u64 {
    value
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn normalize_status_path(raw_path: &str) -> String {
    normalize_git_output_path(raw_path.rsplit(" -> ").next().unwrap_or(raw_path))
}

fn normalize_git_output_path(raw_path: &str) -> String {
    raw_path.trim().trim_matches('"').replace('\\', "/")
}

fn status_label(status_code: &str) -> &'static str {
    match status_code {
        "??" => "untracked",
        "A" | "A?" | "A " | " A" => "added",
        "M" | "M " | " M" | "MM" => "modified",
        "D" | "D " | " D" => "deleted",
        "R" | "R " | " R" => "renamed",
        "C" | "C " | " C" => "copied",
        _ => "changed",
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{status, validate_relative_git_path, WorkspaceGitRootRequest};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn rejects_absolute_or_parent_file_diff_paths() {
        assert!(validate_relative_git_path("../secret.txt").is_err());
        assert!(validate_relative_git_path(r"src\..\secret.txt").is_err());
        assert!(validate_relative_git_path(r"C:\Users\secret.txt").is_err());
        assert!(validate_relative_git_path(r"C:secret.txt").is_err());
        assert_eq!(
            validate_relative_git_path(r"src\main.rs").expect("relative path is accepted"),
            "src/main.rs"
        );
    }

    #[test]
    fn status_reads_branch_and_diff_stats() {
        let repo = temp_repo_path("centaeris-git-status");
        fs::create_dir_all(repo.as_path()).expect("create temp repo");
        let result = (|| {
            run_git(repo.as_path(), &["init"])?;
            run_git(repo.as_path(), &["checkout", "-b", "main"])?;
            fs::write(repo.join("file.txt"), "old\n").map_err(|error| error.to_string())?;
            run_git(repo.as_path(), &["add", "file.txt"])?;
            run_git(
                repo.as_path(),
                &[
                    "-c",
                    "user.name=Centaeris Test",
                    "-c",
                    "user.email=centaeris@example.invalid",
                    "commit",
                    "-m",
                    "initial",
                ],
            )?;
            fs::write(repo.join("file.txt"), "new\nline\n").map_err(|error| error.to_string())?;
            let response = status(WorkspaceGitRootRequest {
                workspace_root: repo.to_string_lossy().to_string(),
            })?;
            assert_eq!(response.branch.as_deref(), Some("main"));
            assert_eq!(response.total_added, 2);
            assert_eq!(response.total_removed, 1);
            assert_eq!(response.changed_files.len(), 1);
            assert_eq!(response.changed_files[0].path, "file.txt");
            Ok::<(), String>(())
        })();
        let _ = fs::remove_dir_all(repo.as_path());
        result.expect("git status projection succeeds");
    }

    #[test]
    fn status_marks_untracked_files_as_not_diff_available() {
        let repo = temp_repo_path("centaeris-git-untracked");
        fs::create_dir_all(repo.as_path()).expect("create temp repo");
        let result = (|| {
            run_git(repo.as_path(), &["init"])?;
            run_git(repo.as_path(), &["checkout", "-b", "main"])?;
            fs::write(repo.join("tracked.txt"), "old\n").map_err(|error| error.to_string())?;
            run_git(repo.as_path(), &["add", "tracked.txt"])?;
            run_git(
                repo.as_path(),
                &[
                    "-c",
                    "user.name=Centaeris Test",
                    "-c",
                    "user.email=centaeris@example.invalid",
                    "commit",
                    "-m",
                    "initial",
                ],
            )?;
            fs::write(repo.join("new.txt"), "new\n").map_err(|error| error.to_string())?;
            let response = status(WorkspaceGitRootRequest {
                workspace_root: repo.to_string_lossy().to_string(),
            })?;
            assert_eq!(response.changed_files.len(), 1);
            assert_eq!(response.changed_files[0].path, "new.txt");
            assert_eq!(response.changed_files[0].status, "untracked");
            assert_eq!(response.changed_files[0].added, 0);
            assert_eq!(response.changed_files[0].removed, 0);
            assert!(!response.changed_files[0].diff_available);
            assert_eq!(
                response.changed_files[0].diff_unavailable_reason.as_deref(),
                Some("未跟踪")
            );
            Ok::<(), String>(())
        })();
        let _ = fs::remove_dir_all(repo.as_path());
        result.expect("untracked status is explicit");
    }

    fn temp_repo_path(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time is monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}"))
    }

    fn run_git(repo: &Path, args: &[&str]) -> Result<(), String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .map_err(|error| format!("git executable unavailable: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        Err(String::from_utf8_lossy(output.stderr.as_slice()).to_string())
    }
}
