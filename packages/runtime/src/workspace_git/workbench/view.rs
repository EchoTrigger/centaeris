use super::*;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Source {
    Unstaged,
    Staged,
    Branch,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    pub workspace_root: String,
    pub source: Source,
    pub base_ref: Option<String>,
    pub path: Option<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ViewFile {
    pub path: String,
    pub original_path: Option<String>,
    pub status: String,
    pub added: Option<u64>,
    pub removed: Option<u64>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Response {
    pub workspace_root: String,
    pub branch: Option<String>,
    pub source: Source,
    pub snapshot: Snapshot,
    pub base_ref: Option<String>,
    pub files: Vec<ViewFile>,
    pub has_conflicts: bool,
    pub diff: Option<super::super::WorkspaceGitFileDiffResponse>,
}

fn resolve_commit(root: &Path, reference: &str) -> Result<String, String> {
    if reference.trim().is_empty() || reference.contains(['\0', '\n', '\r']) {
        return Err("workspace_git_invalid_base_ref".into());
    }
    Ok(git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )?
    .trim()
    .into())
}

pub(crate) fn get(request: Request) -> Result<Response, String> {
    let root = root(&request.workspace_root)?;
    let lock = repo_lock(&root)?;
    let _guard = lock.lock().map_err(|_| "workspace_git_lock_poisoned")?;
    let review = read_review(&root)?;
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--find-renames",
    ];
    let mut base_ref = None;
    let mut revisions = Vec::new();
    if request.source == Source::Branch {
        let head = review
            .snapshot
            .head
            .as_ref()
            .ok_or("workspace_git_no_commits")?;
        let reference = if let Some(reference) = request.base_ref {
            reference
        } else {
            let symbolic = capture(
                &root,
                &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
                &[],
            )?;
            let mut candidates = vec![];
            if symbolic.0.exit_code == Some(0) {
                candidates.push(symbolic.0.stdout.trim().to_string());
            }
            candidates
                .extend(["origin/main", "origin/master", "main", "master"].map(str::to_string));
            candidates
                .into_iter()
                .find(|value| resolve_commit(&root, value).is_ok())
                .ok_or("workspace_git_base_ref_required")?
        };
        let target = resolve_commit(&root, &reference)?;
        let base = git(&root, &["merge-base", head, &target])?
            .trim()
            .to_string();
        revisions.extend([base, head.clone()]);
        base_ref = Some(reference);
    } else if request.base_ref.is_some() {
        return Err("workspace_git_base_ref_requires_branch_source".into());
    }
    if request.source == Source::Staged {
        args.push("--cached");
    }
    args.extend(revisions.iter().map(String::as_str));
    let entries = match request.source {
        Source::Staged => review.staged,
        Source::Unstaged => review
            .unstaged
            .into_iter()
            .chain(review.untracked)
            .chain(review.conflicts.clone())
            .collect(),
        Source::Branch => {
            let mut command = args.clone();
            command.extend(["--name-status", "-z", "--"]);
            let output = git(&root, &command)?;
            let mut records = output.split('\0').filter(|value| !value.is_empty());
            let mut files = vec![];
            while let Some(status) = records.next() {
                let first = records.next().ok_or("workspace_git_invalid_name_status")?;
                let (path, original_path) = if status.starts_with(['R', 'C']) {
                    (
                        records
                            .next()
                            .ok_or("workspace_git_missing_rename_target")?,
                        Some(first.into()),
                    )
                } else {
                    (first, None)
                };
                files.push(File {
                    path: path.into(),
                    original_path,
                    status: status[..1].into(),
                });
            }
            files
        }
    };
    let mut command = args.clone();
    command.extend(["--numstat", "-z", "--"]);
    let output = git(&root, &command)?;
    let mut records = output.split('\0').filter(|value| !value.is_empty());
    let mut stats = HashMap::new();
    while let Some(record) = records.next() {
        let parts: Vec<_> = record.splitn(3, '\t').collect();
        if parts.len() != 3 {
            return Err("workspace_git_invalid_numstat".into());
        }
        let path = if parts[2].is_empty() {
            records
                .next()
                .ok_or("workspace_git_missing_rename_source")?;
            records
                .next()
                .ok_or("workspace_git_missing_rename_target")?
        } else {
            parts[2]
        };
        let count = |value: &str| -> Result<Option<u64>, String> {
            if value == "-" {
                Ok(None)
            } else {
                value
                    .parse()
                    .map(Some)
                    .map_err(|_| "workspace_git_invalid_count".into())
            }
        };
        stats.insert(path.to_string(), (count(parts[0])?, count(parts[1])?));
    }
    let files: Vec<_> = entries
        .into_iter()
        .map(|file| {
            let (added, removed) = if matches!(file.status.as_str(), "?" | "U") {
                (None, None)
            } else {
                stats.get(&file.path).copied().unwrap_or((None, None))
            };
            ViewFile {
                path: file.path,
                original_path: file.original_path,
                status: file.status,
                added,
                removed,
            }
        })
        .collect();
    let diff = if let Some(path) = request.path {
        validate_path(&path)?;
        let entry = files
            .iter()
            .find(|file| file.path == path)
            .ok_or("workspace_git_diff_no_longer_available")?;
        if matches!(entry.status.as_str(), "?" | "U") {
            None
        } else {
            args.extend(["--", &entry.path]);
            if let Some(original) = entry.original_path.as_deref() {
                args.push(original);
            }
            let (diff_preview, truncated) = checked(&root, &args, &[], true)?;
            Some(super::super::WorkspaceGitFileDiffResponse {
                workspace_root: super::super::display_path(&root),
                path,
                diff_preview,
                truncated,
            })
        }
    } else {
        None
    };
    check_expected(&snapshot(&root)?, &review.snapshot)?;
    Ok(Response {
        workspace_root: super::super::display_path(&root),
        branch: review.branch,
        source: request.source,
        snapshot: review.snapshot,
        base_ref,
        files,
        has_conflicts: !review.conflicts.is_empty(),
        diff,
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::Repo;
    use super::*;
    fn request(repo: &Repo, source: Source) -> Request {
        Request {
            workspace_root: repo.root(),
            source,
            base_ref: None,
            path: None,
        }
    }
    #[test]
    fn shared_view_samples_match_serialization_and_reject_unknown_fields() {
        let samples: serde_json::Value = serde_json::from_str(include_str!(
            "../../../generated/workspace-git-samples.json"
        ))
        .unwrap();
        let request: Request = serde_json::from_value(samples["viewRequest"].clone()).unwrap();
        assert_eq!(request.source, Source::Branch);
        assert_eq!(request.path.as_deref(), Some("a.txt"));
        let response = Response {
            workspace_root: request.workspace_root.clone(),
            branch: Some("feature".into()),
            source: request.source,
            snapshot: serde_json::from_value(samples["review"]["snapshot"].clone()).unwrap(),
            base_ref: request.base_ref,
            files: vec![ViewFile {
                path: "a.txt".into(),
                original_path: None,
                status: "A".into(),
                added: Some(1),
                removed: Some(0),
            }],
            has_conflicts: false,
            diff: Some(super::super::super::WorkspaceGitFileDiffResponse {
                workspace_root: request.workspace_root,
                path: "a.txt".into(),
                diff_preview: "+sample".into(),
                truncated: false,
            }),
        };
        assert_eq!(serde_json::to_value(response).unwrap(), samples["view"]);
        let mut invalid = samples["viewRequest"].clone();
        invalid["force"] = true.into();
        assert!(serde_json::from_value::<Request>(invalid).is_err());
    }
    #[test]
    fn sources_have_independent_counts_and_branch_excludes_uncommitted_edits() {
        let repo = Repo::new();
        repo.write("a.txt", "base\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-m", "base"]);
        repo.git(&["checkout", "-b", "feature"]);
        repo.write("a.txt", "committed\nextra\n");
        repo.git(&["commit", "-am", "feature"]);
        repo.write("a.txt", "staged\n");
        repo.git(&["add", "."]);
        repo.write("a.txt", "working\nextra\nthird\n");
        let staged = get(request(&repo, Source::Staged)).unwrap();
        assert_eq!(staged.files[0].added, Some(1));
        assert_eq!(staged.files[0].removed, Some(2));
        let unstaged = get(request(&repo, Source::Unstaged)).unwrap();
        assert_eq!(unstaged.files[0].added, Some(3));
        let mut req = request(&repo, Source::Branch);
        req.base_ref = Some("main".into());
        req.path = Some("a.txt".into());
        let branch = get(req).unwrap();
        assert_eq!(branch.files[0].added, Some(2));
        let diff = branch.diff.unwrap().diff_preview;
        assert!(diff.contains("+committed"));
        assert!(!diff.contains("working"));
    }
    #[test]
    fn rename_binary_and_unborn_stats_remain_explicit() {
        let repo = Repo::new();
        repo.write("new.txt", "new\n");
        let state = get(request(&repo, Source::Unstaged)).unwrap();
        assert_eq!(state.files[0].added, None);
        repo.git(&["add", "."]);
        assert_eq!(
            get(request(&repo, Source::Staged)).unwrap().files[0].added,
            Some(1)
        );
        repo.git(&["commit", "-m", "base"]);
        repo.git(&["mv", "new.txt", "renamed.txt"]);
        std::fs::write(repo.0.join("binary.bin"), b"a\0b").unwrap();
        repo.git(&["add", "."]);
        let state = get(request(&repo, Source::Staged)).unwrap();
        let rename = state
            .files
            .iter()
            .find(|f| f.path == "renamed.txt")
            .unwrap();
        assert_eq!(rename.original_path.as_deref(), Some("new.txt"));
        assert_eq!(rename.added, Some(0));
        assert_eq!(
            state
                .files
                .iter()
                .find(|f| f.path == "binary.bin")
                .unwrap()
                .added,
            None
        );
    }
}
