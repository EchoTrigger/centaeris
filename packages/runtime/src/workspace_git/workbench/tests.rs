use super::*;
use std::{fs, path::PathBuf, process::Command};

pub(super) struct Repo(pub(super) PathBuf);
impl Repo {
    pub(super) fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "centaeris-git-workbench-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-b", "main"]);
        repo.git(&["config", "user.name", "Centaeris Test"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.git(&["config", "core.hooksPath", ".git/test-hooks"]);
        repo
    }
    pub(super) fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    pub(super) fn root(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
    fn review(&self) -> Review {
        review(ReviewRequest {
            workspace_root: self.root(),
        })
        .unwrap()
    }
    fn path(&self, path: &str) -> PathRequest {
        PathRequest {
            workspace_root: self.root(),
            path: path.into(),
            expected: self.review().snapshot,
        }
    }
    pub(super) fn write(&self, path: &str, text: &str) {
        fs::write(self.0.join(path), text).unwrap();
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn unborn_stage_unstage_and_commit_preserve_worktree() {
    let repo = Repo::new();
    repo.write("new 文件.txt", "first\n");
    assert_eq!(repo.review().untracked[0].path, "new 文件.txt");
    let staged = stage(repo.path("new 文件.txt")).unwrap();
    assert_eq!(staged.staged[0].path, "new 文件.txt");
    assert!(staged.snapshot.head.is_none());
    unstage(repo.path("new 文件.txt")).unwrap();
    assert_eq!(
        fs::read_to_string(repo.0.join("new 文件.txt")).unwrap(),
        "first\n"
    );
    stage(repo.path("new 文件.txt")).unwrap();
    let result = commit(CommitRequest {
        workspace_root: repo.root(),
        message: "Initial\n\nBody with $() and `literal`".into(),
        expected: repo.review().snapshot,
    })
    .unwrap();
    assert_eq!(result.commit_sha, repo.git(&["rev-parse", "HEAD"]).trim());
    assert!(repo
        .git(&["log", "-1", "--format=%B"])
        .contains("Body with $() and `literal`"));
    assert!(repo.review().staged.is_empty());
}

#[test]
fn partial_staging_and_external_index_changes_are_explicit() {
    let repo = Repo::new();
    repo.write("file.txt", "base\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-m", "base"]);
    repo.write("file.txt", "staged\n");
    repo.git(&["add", "file.txt"]);
    repo.write("file.txt", "working\n");
    let state = repo.review();
    assert_eq!(state.staged[0].path, "file.txt");
    assert_eq!(state.unstaged[0].path, "file.txt");
    repo.git(&["add", "file.txt"]);
    let error = commit(CommitRequest {
        workspace_root: repo.root(),
        message: "stale".into(),
        expected: state.snapshot,
    })
    .unwrap_err();
    assert!(error.contains("snapshot_changed"), "{error}");
    assert_eq!(repo.git(&["log", "-1", "--format=%s"]).trim(), "base");
}

#[test]
fn literal_paths_cannot_stage_neighbors() {
    let repo = Repo::new();
    repo.write("[one].txt", "one");
    repo.write("o.txt", "other");
    stage(repo.path("[one].txt")).unwrap();
    let state = repo.review();
    assert_eq!(state.staged.len(), 1);
    assert_eq!(state.staged[0].path, "[one].txt");
    assert!(stage(repo.path("../outside")).is_err());
}

#[test]
fn staged_and_working_diffs_are_separate_and_commit_only_takes_the_index() {
    let repo = Repo::new();
    repo.write("file.txt", "base\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-m", "base"]);
    repo.write("file.txt", "staged\n");
    stage(repo.path("file.txt")).unwrap();
    repo.write("file.txt", "working\n");
    let diff = |side| {
        file_diff(DiffRequest {
            workspace_root: repo.root(),
            path: "file.txt".into(),
            side,
        })
        .unwrap()
        .diff_preview
    };
    assert!(diff(DiffSide::Staged).contains("+staged"));
    assert!(diff(DiffSide::Unstaged).contains("+working"));
    commit(CommitRequest {
        workspace_root: repo.root(),
        message: "index only".into(),
        expected: repo.review().snapshot,
    })
    .unwrap();
    assert_eq!(repo.git(&["show", "HEAD:file.txt"]), "staged\n");
    assert_eq!(
        fs::read_to_string(repo.0.join("file.txt")).unwrap(),
        "working\n"
    );
    assert_eq!(repo.review().unstaged.len(), 1);
}

#[test]
fn rename_with_further_edits_can_stage_and_unstage_both_paths() {
    let repo = Repo::new();
    repo.write("before.txt", "one\ntwo\nthree\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-m", "base"]);
    repo.git(&["mv", "before.txt", "after.txt"]);
    repo.write("after.txt", "one\ntwo\nthree\nfour\n");
    assert_eq!(
        repo.review().staged[0].original_path.as_deref(),
        Some("before.txt")
    );
    stage(repo.path("after.txt")).unwrap();
    assert!(repo.review().unstaged.is_empty());
    unstage(repo.path("after.txt")).unwrap();
    assert!(repo.review().staged.is_empty());
    assert!(repo.0.join("after.txt").exists());
    assert!(!repo.0.join("before.txt").exists());
}

#[test]
fn unborn_unstage_keeps_working_edits_after_staging() {
    let repo = Repo::new();
    repo.write("file.txt", "staged");
    stage(repo.path("file.txt")).unwrap();
    repo.write("file.txt", "working");
    unstage(repo.path("file.txt")).unwrap();
    assert!(repo.review().staged.is_empty());
    assert_eq!(
        fs::read_to_string(repo.0.join("file.txt")).unwrap(),
        "working"
    );
}

#[test]
fn failing_hook_is_reported_without_retry_and_keeps_staged_contents() {
    let repo = Repo::new();
    repo.write("file.txt", "staged");
    stage(repo.path("file.txt")).unwrap();
    let hooks = repo.0.join(".git/test-hooks");
    fs::create_dir(&hooks).unwrap();
    fs::write(
        hooks.join("pre-commit"),
        "#!/bin/sh\necho hook-called >> hook-count\necho Hook-rejected >&2\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(hooks.join("pre-commit"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let error = commit(CommitRequest {
        workspace_root: repo.root(),
        message: "test".into(),
        expected: repo.review().snapshot,
    })
    .unwrap_err();
    assert!(error.contains("Hook-rejected"), "{error}");
    assert_eq!(
        fs::read_to_string(repo.0.join("hook-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(repo.review().snapshot.head.is_none());
    assert_eq!(repo.review().staged.len(), 1);
}

#[test]
fn in_progress_merge_is_left_to_git_cli_even_after_resolution() {
    let repo = Repo::new();
    repo.write("file.txt", "base");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-m", "base"]);
    let head = repo.git(&["rev-parse", "HEAD"]);
    repo.write("file.txt", "resolved");
    stage(repo.path("file.txt")).unwrap();
    fs::write(repo.0.join(".git/MERGE_HEAD"), head).unwrap();
    let error = commit(CommitRequest {
        workspace_root: repo.root(),
        message: "merge".into(),
        expected: repo.review().snapshot,
    })
    .unwrap_err();
    assert!(error.contains("operation_in_progress"), "{error}");
}

#[test]
fn desktop_shared_samples_match_rust_serialization_and_strict_requests() {
    let samples: serde_json::Value = serde_json::from_str(include_str!(
        "../../../generated/workspace-git-samples.json"
    ))
    .unwrap();
    let path: PathRequest = serde_json::from_value(samples["pathRequest"].clone()).unwrap();
    let commit: CommitRequest = serde_json::from_value(samples["commitRequest"].clone()).unwrap();
    let diff: DiffRequest = serde_json::from_value(samples["diffRequest"].clone()).unwrap();
    assert_eq!(path.workspace_root, "sample-root");
    assert_eq!(path.path, "a.txt");
    assert_eq!(commit.expected, path.expected);
    assert_eq!(commit.message, "Sample");
    assert!(matches!(diff.side, DiffSide::Staged));
    let review = Review {
        workspace_root: path.workspace_root,
        branch: Some("main".into()),
        snapshot: path.expected,
        staged: vec![File {
            path: path.path,
            original_path: None,
            status: "A".into(),
        }],
        unstaged: vec![],
        untracked: vec![],
        conflicts: vec![],
    };
    assert_eq!(serde_json::to_value(review).unwrap(), samples["review"]);
    assert_eq!(
        serde_json::to_value(CommitResult {
            commit_sha: "sha".into(),
            output: "committed".into()
        })
        .unwrap(),
        samples["commitResult"]
    );
    assert_eq!(
        serde_json::to_value(super::super::WorkspaceGitFileDiffResponse {
            workspace_root: diff.workspace_root,
            path: diff.path,
            diff_preview: "+sample".into(),
            truncated: false
        })
        .unwrap(),
        samples["diff"]
    );
    let mut invalid = samples["pathRequest"].clone();
    invalid["force"] = true.into();
    assert!(serde_json::from_value::<PathRequest>(invalid).is_err());
}

#[test]
fn real_unmerged_entries_are_separate_and_cannot_commit() {
    let repo = Repo::new();
    repo.write("file.txt", "base\n");
    repo.git(&["add", "."]);
    repo.git(&["commit", "-m", "base"]);
    repo.git(&["checkout", "-b", "other"]);
    repo.write("file.txt", "other\n");
    repo.git(&["commit", "-am", "other"]);
    repo.git(&["checkout", "main"]);
    repo.write("file.txt", "main\n");
    repo.git(&["commit", "-am", "main"]);
    let output = Command::new("git")
        .args(["merge", "other"])
        .current_dir(&repo.0)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let state = repo.review();
    assert_eq!(state.conflicts.len(), 1);
    assert!(state.staged.is_empty());
    let error = commit(CommitRequest {
        workspace_root: repo.root(),
        message: "no".into(),
        expected: state.snapshot,
    })
    .unwrap_err();
    assert!(error.contains("unmerged"));
}

#[test]
fn git_process_capture_bounds_output_and_time() {
    let mut command = Command::new("git");
    command.args(["-c", "alias.sample=!printf 12345678901234567890", "sample"]);
    let (output, truncated) = capture_application_command(&mut command, &[], 5_000, 8).unwrap();
    assert_eq!(output.exit_code, Some(0));
    assert!(truncated);
    assert!(output.stdout.len() <= 8);
    let mut command = Command::new("git");
    command.args(["-c", "alias.sample=!sleep 10", "sample"]);
    let start = std::time::Instant::now();
    let (output, _) = capture_application_command(&mut command, &[], 200, 1024).unwrap();
    assert!(output.timed_out);
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
}
