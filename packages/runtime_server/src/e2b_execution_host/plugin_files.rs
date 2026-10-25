//! File-tool access to the current Run's frozen native package projection.
use super::*;

impl E2bExecutionHostRunner {
    pub(super) fn run_plugin_file_operation(
        &self,
        request: &ExecutionFileSystemRequest,
    ) -> Result<ExecutionFileSystemOutput, ExecutionFileSystemError> {
        let denied = || {
            ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::PermissionDenied,
                "e2b_plugin_path_not_authorized",
            )
        };
        let projection = self
            .plugin_projection()
            .map_err(|_| fs_unavailable("e2b_plugin_state_unavailable"))?
            .ok_or_else(denied)?;
        let path = request.model_path.as_str();
        if !projection.contains_path(path) {
            return Err(denied());
        }
        let identity = ExecutionFileIdentity {
            key: format!("{}:{path}", projection.digest),
            display_path: path.into(),
        };
        let _activity = self
            .begin_activity()
            .map_err(|_| fs_unavailable("e2b_activity_unavailable"))?;
        if !self.helper_effects_known() {
            return Err(fs_unavailable("e2b_prior_helper_effects_unknown"));
        }
        match request.operation {
            ExecutionFileSystemOperation::ReadFile { max_bytes } => {
                let expected = projection.files.get(path).ok_or_else(|| {
                    ExecutionFileSystemError::new(
                        ExecutionFileSystemErrorKind::NotFile,
                        "e2b_plugin_not_file",
                    )
                })?;
                if expected.bytes.len() > max_bytes {
                    return Err(ExecutionFileSystemError::new(
                        ExecutionFileSystemErrorKind::TooLarge,
                        "e2b_plugin_read_budget_exceeded",
                    ));
                }
                let output = self
                    .dispatch(|| {
                        self.transport
                            .read_plugin(&self.binding, path, expected.bytes.len().max(1))
                            .map(E2bDispatchResult::Helper)
                    })
                    .map_err(|_| fs_unavailable("e2b_plugin_read_unconfirmed"))?;
                let E2bDispatchResult::Helper(bytes) = output else {
                    return Err(fs_unavailable("e2b_plugin_read_unconfirmed"));
                };
                if bytes != expected.bytes {
                    return Err(fs_unavailable("e2b_plugin_content_mismatch"));
                }
                Ok(ExecutionFileSystemOutput::ReadFile(
                    ExecutionFileReadOutput {
                        identity,
                        file_hash: format!("sha256:{:x}", Sha256::digest(&bytes)),
                        bytes,
                    },
                ))
            }
            ExecutionFileSystemOperation::ListDirectory {
                recursive,
                max_entries,
            } => {
                if projection.files.contains_key(path) {
                    return Err(ExecutionFileSystemError::new(
                        ExecutionFileSystemErrorKind::NotDirectory,
                        "e2b_plugin_not_directory",
                    ));
                }
                let prefix = format!("{path}/");
                let included = |candidate: &str| {
                    candidate
                        .strip_prefix(&prefix)
                        .is_some_and(|suffix| recursive || !suffix.contains('/'))
                };
                let mut expected = projection
                    .files
                    .iter()
                    .filter(|(path, _)| included(path))
                    .map(|(path, file)| ExecutionDirectoryEntry {
                        path: path.clone(),
                        kind: ExecutionDirectoryEntryKind::File,
                        size_bytes: Some(file.bytes.len() as u64),
                    })
                    .collect::<Vec<_>>();
                expected.extend(projection.directories.iter().filter(|p| included(p)).map(
                    |path| ExecutionDirectoryEntry {
                        path: path.clone(),
                        kind: ExecutionDirectoryEntryKind::Directory,
                        size_bytes: None,
                    },
                ));
                if expected.len() > max_entries || max_entries == 0 || max_entries > 10_000 {
                    return Err(ExecutionFileSystemError::new(
                        ExecutionFileSystemErrorKind::TooLarge,
                        "e2b_plugin_list_budget_exceeded",
                    ));
                }
                expected.sort_by(|a, b| a.path.cmp(&b.path));
                let result = self
                    .dispatch(|| {
                        let entries = self.transport.list_plugin(
                            &self.binding,
                            path,
                            recursive,
                            max_entries,
                        )?;
                        serde_json::to_vec(&entries)
                            .map(E2bDispatchResult::Helper)
                            .map_err(|_| E2bHostError::Unknown("e2b_plugin_list_invalid"))
                    })
                    .map_err(|_| fs_unavailable("e2b_plugin_list_unconfirmed"))?;
                let E2bDispatchResult::Helper(bytes) = result else {
                    return Err(fs_unavailable("e2b_plugin_list_unconfirmed"));
                };
                let mut entries: Vec<ExecutionDirectoryEntry> = serde_json::from_slice(&bytes)
                    .map_err(|_| fs_unavailable("e2b_plugin_list_invalid"))?;
                for entry in &mut entries {
                    if entry.kind == ExecutionDirectoryEntryKind::Directory {
                        entry.size_bytes = None;
                    }
                }
                entries.sort_by(|a, b| a.path.cmp(&b.path));
                if entries != expected {
                    return Err(fs_unavailable("e2b_plugin_list_mismatch"));
                }
                Ok(ExecutionFileSystemOutput::ListDirectory(
                    ExecutionDirectoryListOutput { identity, entries },
                ))
            }
            _ => Err(ExecutionFileSystemError::new(
                ExecutionFileSystemErrorKind::PermissionDenied,
                "e2b_plugin_files_read_only",
            )),
        }
    }
}
