use super::*;
use hosted_execution::protocol::{
    SandboxSnapshotBaseline, SandboxSnapshotCaptureRequest, SandboxSnapshotCaptureResponse,
    SnapshotCaptureDisposition, SANDBOX_SNAPSHOT_CAPTURE_SCHEMA,
};

#[derive(Clone)]
pub(super) struct SnapshotCaptureCache {
    descriptor: WorkspaceSnapshotDescriptor,
    manifest: SandboxWorkspaceSnapshotManifest,
}

impl SnapshotCaptureCache {
    fn baseline_for(
        &self,
        expected: &WorkspaceSnapshotDescriptor,
    ) -> Option<SandboxSnapshotBaseline> {
        (&self.descriptor == expected).then(|| SandboxSnapshotBaseline {
            snapshot_sha256: self.descriptor.sha256.clone(),
            manifest: self.manifest.clone(),
        })
    }
}

static NEXT_CAPTURE_ID: AtomicU64 = AtomicU64::new(1);

// The cache is runner-local and holds at most one validated, bounded manifest.
// Taking it before execution also discards it on errors or unwinding. A receipt
// is never a source of new manifest/descriptor authority.
fn capture_and_cache(
    cache: &Mutex<Option<SnapshotCaptureCache>>,
    expected: &WorkspaceSnapshotDescriptor,
    collect: impl FnOnce(&SandboxSnapshotCaptureRequest) -> Result<SnapshotCaptureResult, String>,
) -> Result<SnapshotCaptureResult, String> {
    let previous = cache
        .lock()
        .map_err(|_| "snapshot capture cache poisoned")?
        .take();
    let sequence = NEXT_CAPTURE_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| "snapshot capture identity overflow")?;
    let request = SandboxSnapshotCaptureRequest {
        schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
        capture_id: format!("capture-{sequence}"),
        baseline: previous
            .as_ref()
            .and_then(|entry| entry.baseline_for(expected)),
    };
    let result = collect(&request)?;
    let next = match &result {
        SnapshotCaptureResult::Snapshot(artifact) => {
            artifact
                .manifest
                .as_ref()
                .map(|manifest| SnapshotCaptureCache {
                    descriptor: artifact.descriptor.clone(),
                    manifest: manifest.clone(),
                })
        }
        SnapshotCaptureResult::Unchanged => {
            if request.baseline.is_none() {
                return Err("snapshot capture returned an unbound receipt".into());
            }
            previous
        }
    };
    *cache
        .lock()
        .map_err(|_| "snapshot capture cache poisoned")? = next;
    Ok(result)
}

impl DockerExecutionHostRunner {
    pub(super) fn capture_snapshot(
        &self,
        capture: &SnapshotActivityGuard,
        expected: &WorkspaceSnapshotDescriptor,
        trigger: ProtectionTrigger,
        job_id: &str,
    ) -> Result<SnapshotCaptureResult, String> {
        let observation =
            WorkspaceProtection::new().with_context(trigger, &self.agent_run_id, Some(job_id));
        self.capture_snapshot_using(capture, expected, &observation, |request| {
            self.capture_snapshot_command(request, &observation)
        })
    }

    fn capture_snapshot_using(
        &self,
        _capture: &SnapshotActivityGuard,
        expected: &WorkspaceSnapshotDescriptor,
        observation: &WorkspaceProtection,
        command: impl FnOnce(&SandboxSnapshotCaptureRequest) -> Result<SnapshotCaptureResult, String>,
    ) -> Result<SnapshotCaptureResult, String> {
        observation.timed(ProtectionMetric::CaptureElapsedMs, || {
            let result = capture_and_cache(&self.snapshot_capture_cache, expected, command)?;
            let logical_bytes = match &result {
                SnapshotCaptureResult::Snapshot(artifact) => {
                    artifact.descriptor.expanded_size_bytes
                }
                SnapshotCaptureResult::Unchanged => {
                    // A verified receipt never invokes the spool factory.
                    observation.count(ProtectionMetric::SpoolWrittenBytes, 0, true);
                    expected.expanded_size_bytes
                }
            };
            observation.count(ProtectionMetric::SnapshotLogicalBytes, logical_bytes, true);
            Ok(result)
        })
    }

    pub(super) fn begin_snapshot_restore(&self) -> Result<(), String> {
        self.snapshot_capture_cache
            .lock()
            .map_err(|_| "snapshot capture cache poisoned")?
            .take();
        self.recovery_activity.invalidate_before_dispatch()?;
        Ok(())
    }

    fn capture_snapshot_command(
        &self,
        capture_request: &SandboxSnapshotCaptureRequest,
        observation: &WorkspaceProtection,
    ) -> Result<SnapshotCaptureResult, String> {
        let bytes =
            serde_json::to_vec(capture_request).map_err(|_| "snapshot_capture_request_invalid")?;
        let mut input = tempfile::tempfile().map_err(|_| "snapshot_capture_spool_unavailable")?;
        input
            .write_all(&bytes)
            .map_err(|_| "snapshot_capture_spool_unavailable")?;
        let (mut output, stderr, exit) = self.supervised_helper_file(
            hosted_execution::host_supervisor::TrustedHelperKind::SnapshotCapture,
            &mut input,
            bytes.len() as u64,
            &format!("sha256:{:x}", Sha256::digest(&bytes)),
        )?;
        let result = read_capture(&mut output, capture_request, |source| {
            capture_snapshot_artifact_observed(
                source,
                self.resources
                    .data_tmpfs_bytes
                    .saturating_add(SESSION_WORKSPACE_MANIFEST_LIMIT as u64 + 4),
                observation.clone(),
            )
        });
        finish_observed_capture(result, Ok(exit), &stderr, observation)
    }
}

fn finish_observed_capture(
    result: Result<SnapshotCaptureResult, String>,
    exit_code: Result<Option<i32>, String>,
    stderr: &[u8],
    observation: &WorkspaceProtection,
) -> Result<SnapshotCaptureResult, String> {
    // Preserve valid measurements even if parsing, validation or helper exit
    // fails. The existing forwarder rejects unrelated IDs and private text.
    observation.forward_sandbox(stderr);
    let exit_code =
        exit_code.map_err(|error| format!("wait for snapshot capture helper failed: {error}"))?;
    finish_capture(result, exit_code, stderr)
}

fn finish_capture(
    result: Result<SnapshotCaptureResult, String>,
    exit_code: Option<i32>,
    stderr: &[u8],
) -> Result<SnapshotCaptureResult, String> {
    if exit_code != Some(0) {
        return Err(helper_failure("snapshot-capture", exit_code, stderr));
    }
    result
}

impl From<&SessionWorkspace> for WorkspaceSnapshotDescriptor {
    fn from(value: &SessionWorkspace) -> Self {
        Self {
            sha256: value.snapshot_sha256.clone(),
            size_bytes: value.snapshot_size_bytes,
            expanded_size_bytes: value.expanded_size_bytes,
            file_count: value.file_count,
        }
    }
}

impl From<&RecoveryWorkspaceSnapshotV1> for WorkspaceSnapshotDescriptor {
    fn from(value: &RecoveryWorkspaceSnapshotV1) -> Self {
        Self {
            sha256: value.snapshot_sha256.clone(),
            size_bytes: value.snapshot_size_bytes,
            expanded_size_bytes: value.expanded_size_bytes,
            file_count: value.file_count,
        }
    }
}

pub(super) enum SnapshotCaptureResult {
    Snapshot(ImmutableWorkspaceSnapshot),
    Unchanged,
}

fn read_capture<R: Read>(
    source: &mut R,
    request: &SandboxSnapshotCaptureRequest,
    spool: impl FnOnce(&mut R) -> Result<ImmutableWorkspaceSnapshot, String>,
) -> Result<SnapshotCaptureResult, String> {
    let mut length = [0_u8; 4];
    source
        .read_exact(&mut length)
        .map_err(|error| format!("read snapshot capture response length failed: {error}"))?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > 1024 {
        return Err("snapshot capture response length is invalid".into());
    }
    let mut bytes = vec![0; length];
    source
        .read_exact(&mut bytes)
        .map_err(|error| format!("read snapshot capture response failed: {error}"))?;
    let response: SandboxSnapshotCaptureResponse = serde_json::from_slice(&bytes)
        .map_err(|error| format!("decode snapshot capture response failed: {error}"))?;
    if response.schema != SANDBOX_SNAPSHOT_CAPTURE_SCHEMA
        || response.capture_id != request.capture_id
    {
        return Err("snapshot capture response binding mismatch".into());
    }
    match response.disposition {
        SnapshotCaptureDisposition::Unchanged => {
            let baseline = request
                .baseline
                .as_ref()
                .ok_or("snapshot capture receipt has no baseline")?;
            if response.baseline_snapshot_sha256.as_deref()
                != Some(baseline.snapshot_sha256.as_str())
                || response.manifest_sha256 != manifest_digest(&baseline.manifest)?
            {
                return Err("snapshot capture receipt baseline mismatch".into());
            }
            let mut trailing = [0; 1];
            if source
                .read(&mut trailing)
                .map_err(|error| format!("read snapshot capture trailing bytes failed: {error}"))?
                != 0
            {
                return Err("snapshot capture receipt has trailing bytes".into());
            }
            Ok(SnapshotCaptureResult::Unchanged)
        }
        SnapshotCaptureDisposition::Snapshot => {
            if response.baseline_snapshot_sha256.is_some() {
                return Err("full snapshot capture response has a baseline receipt".into());
            }
            let artifact = spool(source)?;
            let empty = SandboxWorkspaceSnapshotManifest {
                schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
                files: vec![],
            };
            if response.manifest_sha256
                != manifest_digest(artifact.manifest.as_ref().unwrap_or(&empty))?
            {
                return Err("snapshot capture manifest binding mismatch".into());
            }
            Ok(SnapshotCaptureResult::Snapshot(artifact))
        }
    }
}

fn manifest_digest(manifest: &SandboxWorkspaceSnapshotManifest) -> Result<String, String> {
    let bytes = serde_json::to_vec(manifest)
        .map_err(|error| format!("encode snapshot capture manifest failed: {error}"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> SandboxWorkspaceSnapshotManifest {
        SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
            files: vec![SandboxWorkspaceSnapshotFile {
                path: "note.txt".into(),
                size_bytes: 3,
                sha256: format!("sha256:{:x}", Sha256::digest(b"abc")),
                executable: false,
            }],
        }
    }

    fn request() -> SandboxSnapshotCaptureRequest {
        SandboxSnapshotCaptureRequest {
            schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
            capture_id: "capture-1".into(),
            baseline: Some(SandboxSnapshotBaseline {
                snapshot_sha256: format!("sha256:{}", "a".repeat(64)),
                manifest: manifest(),
            }),
        }
    }

    fn receipt(request: &SandboxSnapshotCaptureRequest) -> SandboxSnapshotCaptureResponse {
        SandboxSnapshotCaptureResponse {
            schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
            capture_id: request.capture_id.clone(),
            disposition: SnapshotCaptureDisposition::Unchanged,
            baseline_snapshot_sha256: Some(
                request.baseline.as_ref().unwrap().snapshot_sha256.clone(),
            ),
            manifest_sha256: format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&manifest()).unwrap())
            ),
        }
    }

    fn encoded(response: &SandboxSnapshotCaptureResponse) -> Vec<u8> {
        let json = serde_json::to_vec(response).unwrap();
        let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
        bytes.extend(json);
        bytes
    }

    fn frame(manifest: &SandboxWorkspaceSnapshotManifest, contents: &[u8]) -> Vec<u8> {
        let json = serde_json::to_vec(manifest).unwrap();
        let mut frame = (json.len() as u32).to_be_bytes().to_vec();
        frame.extend(json);
        frame.extend(contents);
        frame
    }

    fn artifact() -> ImmutableWorkspaceSnapshot {
        capture_snapshot_artifact(&mut Cursor::new(frame(&manifest(), b"abc")), 4096).unwrap()
    }

    fn full_response(
        request: &SandboxSnapshotCaptureRequest,
        manifest: &SandboxWorkspaceSnapshotManifest,
    ) -> SandboxSnapshotCaptureResponse {
        SandboxSnapshotCaptureResponse {
            schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
            capture_id: request.capture_id.clone(),
            disposition: SnapshotCaptureDisposition::Snapshot,
            baseline_snapshot_sha256: None,
            manifest_sha256: manifest_digest(manifest).unwrap(),
        }
    }

    #[test]
    fn unchanged_receipt_does_not_create_or_write_a_snapshot_spool() {
        let request = request();
        let result = read_capture(
            &mut Cursor::new(encoded(&receipt(&request))),
            &request,
            |_| panic!("unchanged content must not allocate a full snapshot spool"),
        )
        .unwrap();
        assert!(matches!(result, SnapshotCaptureResult::Unchanged));
    }

    #[test]
    fn cache_requires_all_authoritative_descriptor_fields() {
        let descriptor = WorkspaceSnapshotDescriptor {
            sha256: format!("sha256:{}", "a".repeat(64)),
            size_bytes: 300,
            expanded_size_bytes: 3,
            file_count: 1,
        };
        let cache = SnapshotCaptureCache {
            descriptor: descriptor.clone(),
            manifest: manifest(),
        };
        assert!(cache.baseline_for(&descriptor).is_some());
        for field in ["hash", "size", "expanded", "count"] {
            let mut other = descriptor.clone();
            match field {
                "hash" => other.sha256 = format!("sha256:{}", "b".repeat(64)),
                "size" => other.size_bytes += 1,
                "expanded" => other.expanded_size_bytes += 1,
                _ => other.file_count += 1,
            }
            assert!(cache.baseline_for(&other).is_none(), "{field}");
        }
    }

    #[test]
    fn failed_capture_preserves_nonzero_exit_status_even_without_output() {
        let error = finish_capture(Err("missing response".into()), Some(7), &[])
            .err()
            .unwrap();
        assert!(error.contains("7"), "{error}");
        assert!(error.contains("snapshot-capture"), "{error}");
    }

    #[test]
    fn full_capture_keeps_exact_v1_bytes_and_rejects_forged_manifest_or_content() {
        let request = request();
        let response = full_response(&request, &manifest());
        let frame = frame(&manifest(), b"abc");
        let mut bytes = encoded(&response);
        bytes.extend(&frame);
        let mut spools = 0;
        let SnapshotCaptureResult::Snapshot(mut artifact) =
            read_capture(&mut Cursor::new(&bytes), &request, |source| {
                spools += 1;
                capture_snapshot_artifact(source, 4096)
            })
            .unwrap()
        else {
            panic!("must provide full frame")
        };
        let mut persisted = vec![];
        artifact.file.read_to_end(&mut persisted).unwrap();
        assert_eq!(persisted, frame);
        assert_eq!(spools, 1);
        let mut forged = response;
        forged.manifest_sha256 = format!("sha256:{}", "f".repeat(64));
        let mut bad = encoded(&forged);
        bad.extend(&frame);
        assert!(read_capture(&mut Cursor::new(bad), &request, |source| {
            capture_snapshot_artifact(source, 4096)
        })
        .is_err());
        *bytes.last_mut().unwrap() = b'x';
        assert!(read_capture(&mut Cursor::new(bytes), &request, |source| {
            capture_snapshot_artifact(source, 4096)
        })
        .is_err());
    }

    #[test]
    fn empty_capture_remains_an_empty_persisted_snapshot_without_a_reuse_baseline() {
        let request = request();
        let empty = SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
            files: vec![],
        };
        let bytes = encoded(&full_response(&request, &empty));
        let SnapshotCaptureResult::Snapshot(artifact) =
            read_capture(&mut Cursor::new(bytes), &request, |source| {
                capture_snapshot_artifact(source, 4096)
            })
            .unwrap()
        else {
            panic!("empty snapshot is a full result")
        };
        assert_eq!(artifact.descriptor.size_bytes, 0);
        assert!(artifact.manifest.is_none());
        assert_eq!(artifact.file.metadata().unwrap().len(), 0);
    }

    #[test]
    fn cache_seeds_only_after_success_and_is_bounded_to_one_runner_local_manifest() {
        let cache = Mutex::new(None);
        let descriptor = artifact().descriptor;
        capture_and_cache(&cache, &descriptor, |request| {
            assert!(request.baseline.is_none());
            Ok(SnapshotCaptureResult::Snapshot(artifact()))
        })
        .unwrap();
        assert!(cache.lock().unwrap().is_some());
        let mut first_capture = String::new();
        capture_and_cache(&cache, &descriptor, |request| {
            first_capture = request.capture_id.clone();
            assert!(request.baseline.is_some());
            Ok(SnapshotCaptureResult::Unchanged)
        })
        .unwrap();
        // An apparently valid full frame followed by a failed helper exit does
        // not seed the cache, and discards the previous cache entry.
        assert!(capture_and_cache(&cache, &descriptor, |request| {
            assert_ne!(request.capture_id, first_capture);
            finish_capture(
                Ok(SnapshotCaptureResult::Snapshot(artifact())),
                Some(7),
                &[],
            )
        })
        .is_err());
        assert!(cache.lock().unwrap().is_none());
        capture_and_cache(&cache, &descriptor, |_| {
            Ok(SnapshotCaptureResult::Snapshot(artifact()))
        })
        .unwrap();
        let other_runner_cache = Mutex::new(None);
        capture_and_cache(&other_runner_cache, &descriptor, |request| {
            assert!(request.baseline.is_none());
            Ok(SnapshotCaptureResult::Snapshot(artifact()))
        })
        .unwrap();
        let mut changed_manifest = manifest();
        changed_manifest.files[0].executable = true;
        let replacement =
            capture_snapshot_artifact(&mut Cursor::new(frame(&changed_manifest, b"abc")), 4096)
                .unwrap();
        let replacement_descriptor = replacement.descriptor.clone();
        capture_and_cache(&cache, &descriptor, |_| {
            Ok(SnapshotCaptureResult::Snapshot(replacement))
        })
        .unwrap();
        let guard = cache.lock().unwrap();
        let cached = guard.as_ref().unwrap();
        assert!(cached.baseline_for(&descriptor).is_none());
        assert!(cached.baseline_for(&replacement_descriptor).is_some());
    }

    #[test]
    fn oversized_unknown_and_truncated_capture_responses_are_rejected() {
        let request = request();
        for bytes in [
            (1025_u32).to_be_bytes().to_vec(),
            vec![],
            vec![0, 0, 0, 2, b'{'],
        ] {
            assert!(read_capture(&mut Cursor::new(bytes), &request, |_| panic!(
                "must reject before spooling"
            ))
            .is_err());
        }
        let mut value = serde_json::to_value(receipt(&request)).unwrap();
        value["unexpected"] = serde_json::json!(true);
        let json = serde_json::to_vec(&value).unwrap();
        let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
        bytes.extend(json);
        assert!(read_capture(&mut Cursor::new(bytes), &request, |_| panic!(
            "must reject before spooling"
        ))
        .is_err());
    }

    #[test]
    fn repeated_runner_capture_reuses_only_after_verified_full_capture_and_restore_clears_it() {
        let runner = super::super::tests::test_docker_execution_host_runner();
        let descriptor = artifact().descriptor;
        let run_capture = |expect_baseline: bool| {
            // This is the same capture/cache boundary used by both recovery
            // preparation and final collection; only Docker transport is replaced.
            runner
                .capture_prepared_recovery_workspace_with_observer(
                    |_| Ok(0),
                    |guard| {
                        let result = runner.capture_snapshot_using(
                            guard,
                            &descriptor,
                            &WorkspaceProtection::new(),
                            |request| {
                                assert_eq!(request.baseline.is_some(), expect_baseline);
                                let response = if expect_baseline {
                                    receipt(request)
                                } else {
                                    full_response(request, &manifest())
                                };
                                let mut bytes = encoded(&response);
                                if !expect_baseline {
                                    bytes.extend(frame(&manifest(), b"abc"));
                                }
                                read_capture(&mut Cursor::new(bytes), request, |source| {
                                    assert!(
                                        !expect_baseline,
                                        "a warm capture must not create a spool"
                                    );
                                    capture_snapshot_artifact(source, 4096)
                                })
                            },
                        )?;
                        assert_eq!(
                            matches!(result, SnapshotCaptureResult::Unchanged),
                            expect_baseline
                        );
                        Ok(PreparedRecoveryContent::Unchanged(
                            RecoveryWorkspaceSnapshotV1 {
                                object_ref: Some("verified-baseline".into()),
                                snapshot_sha256: descriptor.sha256.clone(),
                                snapshot_size_bytes: descriptor.size_bytes,
                                expanded_size_bytes: descriptor.expanded_size_bytes,
                                file_count: descriptor.file_count,
                            },
                        ))
                    },
                )
                .unwrap()
                .unwrap()
        };
        let first = run_capture(false);
        let second = run_capture(true);
        assert_eq!(
            first.evidence().snapshot_host_instance,
            second.evidence().snapshot_host_instance
        );
        runner.begin_snapshot_restore().unwrap();
        run_capture(false);
        run_capture(true);
        let replacement = super::super::tests::test_docker_execution_host_runner();
        assert!(replacement.snapshot_capture_cache.lock().unwrap().is_none());
    }

    #[test]
    fn conditional_capture_forwards_only_bound_counters_on_failure() {
        let (_, events) = crate::observations::capture_protection(|| {
            let observation = WorkspaceProtection::new().with_context(
                ProtectionTrigger::RecoveryCheckpoint,
                "run-metrics",
                Some("job-metrics"),
            );
            let operation_id = observation.operation_id();
            let event = serde_json::json!({"schema":"workspace.perf.v1","source":"sandbox","phase":"workspaceProtection","operationId":operation_id,"metric":"sourceReadBytes","value":17,"outcome":"error"});
            let mut bytes = serde_json::to_vec(&event).unwrap();
            bytes.extend_from_slice(b"\nprivate diagnostic must not become metrics\n");
            let mut foreign = event.clone();
            foreign["operationId"] = "other-operation".into();
            bytes.extend(serde_json::to_vec(&foreign).unwrap());
            bytes.push(b'\n');
            assert!(finish_observed_capture(
                Err("invalid response".into()),
                Ok(Some(7)),
                &bytes,
                &observation
            )
            .is_err());
        });
        assert_eq!(events.len(), 1, "only the valid bound counter is forwarded");
        assert_eq!(events[0]["value"], 17);
        assert_eq!(events[0]["outcome"], "error");
        assert_eq!(events[0]["trigger"], "recoveryCheckpoint");
        assert_eq!(events[0]["runId"], "run-metrics");
        assert_eq!(events[0]["jobId"], "job-metrics");
    }

    #[test]
    fn conditional_capture_observes_zero_spool_for_warm_runner_and_preserves_operation_identity() {
        let (frame_bytes, events) = crate::observations::capture_protection(|| {
            let runner = super::super::tests::test_docker_execution_host_runner();
            let expected = artifact().descriptor;
            for warm in [false, true] {
                let observation = WorkspaceProtection::new().with_context(
                    ProtectionTrigger::TerminalCommit,
                    "run-same",
                    Some("job-same"),
                );
                let guard = runner.recovery_activity.wait_snapshot_activity().unwrap();
                runner
                    .capture_snapshot_using(&guard, &expected, &observation, |request| {
                        let response = if warm {
                            receipt(request)
                        } else {
                            full_response(request, &manifest())
                        };
                        let mut bytes = encoded(&response);
                        if !warm {
                            bytes.extend(frame(&manifest(), b"abc"));
                        }
                        read_capture(&mut Cursor::new(bytes), request, |source| {
                            capture_snapshot_artifact_observed(source, 4096, observation.clone())
                        })
                    })
                    .unwrap();
            }
            expected.size_bytes
        });
        let spool = events
            .iter()
            .filter(|event| {
                event["metric"] == "runtimeSpoolWrittenBytes"
                    && event["trigger"] == "terminalCommit"
            })
            .collect::<Vec<_>>();
        assert_eq!(spool.len(), 2);
        assert_eq!(spool[0]["value"], frame_bytes);
        assert_eq!(spool[1]["value"], 0);
        for event in spool {
            let operation_id = &event["operationId"];
            assert_eq!(
                events
                    .iter()
                    .filter(|event| &event["operationId"] == operation_id
                        && event["metric"] == "snapshotLogicalBytes"
                        && event["value"] == 3)
                    .count(),
                1
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| &event["operationId"] == operation_id
                        && event["metric"] == "captureElapsedMs")
                    .count(),
                1
            );
        }
    }

    #[test]
    fn unchanged_receipt_requires_the_current_capture_and_exact_manifest_binding() {
        let request = request();
        for field in ["capture", "manifest", "snapshot", "schema"] {
            let mut response = receipt(&request);
            match field {
                "capture" => response.capture_id = "late-capture".into(),
                "manifest" => response.manifest_sha256 = format!("sha256:{}", "b".repeat(64)),
                "snapshot" => {
                    response.baseline_snapshot_sha256 = Some(format!("sha256:{}", "b".repeat(64)))
                }
                _ => response.schema = "unsupported".into(),
            }
            assert!(
                read_capture(&mut Cursor::new(encoded(&response)), &request, |_| panic!(
                    "invalid receipt must not spool"
                ))
                .is_err(),
                "{field}"
            );
        }
        let mut without_baseline = request.clone();
        without_baseline.baseline = None;
        assert!(read_capture(
            &mut Cursor::new(encoded(&receipt(&request))),
            &without_baseline,
            |_| panic!("unbound receipt must not spool")
        )
        .is_err());
        let mut trailing = encoded(&receipt(&request));
        trailing.push(1);
        assert!(
            read_capture(&mut Cursor::new(trailing), &request, |_| panic!(
                "trailing bytes must not spool"
            ))
            .is_err()
        );
    }
}
