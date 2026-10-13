use super::*;
use crate::protocol::{
    SandboxSnapshotCaptureRequest, SandboxSnapshotCaptureResponse, SnapshotCaptureDisposition,
    SANDBOX_SNAPSHOT_CAPTURE_SCHEMA,
};

pub(super) fn run() -> Result<(), String> {
    observed_capture(
        &mut SnapshotObservation::from_environment(),
        &mut std::io::stderr().lock(),
        run_inner,
    )
}

fn observed_capture(
    observation: &mut SnapshotObservation,
    diagnostics: &mut impl Write,
    operation: impl FnOnce(&mut SnapshotObservation) -> Result<(), String>,
) -> Result<(), String> {
    let result = operation(observation);
    observation.emit_to(result.is_ok(), diagnostics);
    result
}

fn run_inner(observation: &mut SnapshotObservation) -> Result<(), String> {
    require_root()?;
    secure_roots()?;
    let request: SandboxSnapshotCaptureRequest = read_json(MAX_JSON_BYTES + 1024)?;
    let mut output = std::io::stdout().lock();
    capture_idle(
        &request,
        &mut output,
        observation,
        require_idle,
        |observation| collect_snapshot_manifest(&load_inventory()?, observation),
        |manifest, sink, observation| {
            for expected in &manifest.files {
                copy_snapshot_file(expected, sink, observation)?;
            }
            Ok(())
        },
    )?;
    output
        .flush()
        .map_err(|error| format!("flush snapshot capture failed: {error}"))
}

fn capture_idle(
    request: &SandboxSnapshotCaptureRequest,
    output: &mut impl Write,
    observation: &mut SnapshotObservation,
    mut idle: impl FnMut() -> Result<(), String>,
    collect: impl FnOnce(&mut SnapshotObservation) -> Result<SandboxWorkspaceSnapshotManifest, String>,
    verify: impl FnOnce(
        &SandboxWorkspaceSnapshotManifest,
        &mut dyn Write,
        &mut SnapshotObservation,
    ) -> Result<(), String>,
) -> Result<(), String> {
    validate_request(request)?;
    idle()?;
    let manifest = collect(observation)?;
    write_capture(request, &manifest, output, |sink| {
        verify(&manifest, sink, observation)?;
        // The caller excludes dispatch; independently reject live agent
        // processes before scanning and after validating all file content.
        idle()
    })
}

fn require_idle() -> Result<(), String> {
    if agent_process_ids()?.is_empty() {
        Ok(())
    } else {
        Err("snapshot capture requires no agent processes".into())
    }
}

fn validate_request(request: &SandboxSnapshotCaptureRequest) -> Result<(), String> {
    if request.schema != SANDBOX_SNAPSHOT_CAPTURE_SCHEMA
        || request.capture_id.is_empty()
        || request.capture_id.len() > 128
        || !request
            .capture_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        return Err("snapshot capture request is invalid".into());
    }
    if let Some(baseline) = &request.baseline {
        validate_snapshot_sha256(&baseline.snapshot_sha256)?;
        canonical_snapshot_manifest_bytes(&baseline.manifest)?;
    }
    Ok(())
}

fn write_capture(
    request: &SandboxSnapshotCaptureRequest,
    manifest: &SandboxWorkspaceSnapshotManifest,
    output: &mut impl Write,
    verify_and_copy: impl FnOnce(&mut dyn Write) -> Result<(), String>,
) -> Result<(), String> {
    validate_request(request)?;
    let manifest_bytes = serde_json::to_vec(manifest).map_err(|error| error.to_string())?;
    let unchanged = request
        .baseline
        .as_ref()
        .filter(|baseline| &baseline.manifest == manifest);
    if unchanged.is_some() {
        // Do not acknowledge until the same second content/metadata validation
        // used by full snapshots has finished. A stat-only shortcut is unsafe.
        verify_and_copy(&mut std::io::sink())?;
    } else {
        return write_full_capture(request, manifest, &manifest_bytes, output, verify_and_copy);
    }
    let response = SandboxSnapshotCaptureResponse {
        schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
        capture_id: request.capture_id.clone(),
        disposition: SnapshotCaptureDisposition::Unchanged,
        baseline_snapshot_sha256: unchanged.map(|baseline| baseline.snapshot_sha256.clone()),
        manifest_sha256: format!("sha256:{:x}", Sha256::digest(&manifest_bytes)),
    };
    write_response(output, &response)
}

fn write_response(
    output: &mut impl Write,
    response: &SandboxSnapshotCaptureResponse,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
    output
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .map_err(|error| error.to_string())?;
    output.write_all(&bytes).map_err(|error| error.to_string())
}

fn write_full_capture(
    request: &SandboxSnapshotCaptureRequest,
    manifest: &SandboxWorkspaceSnapshotManifest,
    manifest_bytes: &[u8],
    output: &mut impl Write,
    verify_and_copy: impl FnOnce(&mut dyn Write) -> Result<(), String>,
) -> Result<(), String> {
    write_response(
        output,
        &SandboxSnapshotCaptureResponse {
            schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
            capture_id: request.capture_id.clone(),
            disposition: SnapshotCaptureDisposition::Snapshot,
            baseline_snapshot_sha256: None,
            manifest_sha256: format!("sha256:{:x}", Sha256::digest(manifest_bytes)),
        },
    )?;
    if !manifest.files.is_empty() {
        output
            .write_all(&(manifest_bytes.len() as u32).to_be_bytes())
            .map_err(|error| error.to_string())?;
        output
            .write_all(manifest_bytes)
            .map_err(|error| error.to_string())?;
    }
    verify_and_copy(output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SandboxSnapshotBaseline;
    use std::io::Cursor;

    fn test_manifest(content: &[u8]) -> SandboxWorkspaceSnapshotManifest {
        SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
            files: vec![SandboxWorkspaceSnapshotFile {
                path: "data.bin".into(),
                size_bytes: content.len() as u64,
                sha256: format!("sha256:{:x}", Sha256::digest(content)),
                executable: false,
            }],
        }
    }

    fn request(manifest: SandboxWorkspaceSnapshotManifest) -> SandboxSnapshotCaptureRequest {
        SandboxSnapshotCaptureRequest {
            schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
            capture_id: "capture-1".into(),
            baseline: Some(SandboxSnapshotBaseline {
                snapshot_sha256: format!("sha256:{}", "a".repeat(64)),
                manifest,
            }),
        }
    }

    fn decode(bytes: &[u8]) -> (SandboxSnapshotCaptureResponse, &[u8]) {
        let length = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        (
            serde_json::from_slice(&bytes[4..4 + length]).unwrap(),
            &bytes[4 + length..],
        )
    }

    #[test]
    fn unchanged_capture_verifies_bytes_without_emitting_snapshot_payload() {
        let content = vec![b'x'; 128 * 1024];
        let manifest = test_manifest(&content);
        let request = request(manifest.clone());
        let mut output = Vec::new();
        let mut verified_bytes = 0;
        write_capture(&request, &manifest, &mut output, |sink| {
            verified_bytes = std::io::copy(&mut Cursor::new(&content), sink).unwrap();
            Ok(())
        })
        .unwrap();
        let (response, body) = decode(&output);
        assert_eq!(
            verified_bytes,
            content.len() as u64,
            "second content validation is retained"
        );
        assert_eq!(response.disposition, SnapshotCaptureDisposition::Unchanged);
        assert_eq!(
            response.baseline_snapshot_sha256,
            Some(request.baseline.unwrap().snapshot_sha256)
        );
        assert!(body.is_empty());
        assert!(output.len() < 512);
    }

    #[test]
    fn changed_capture_keeps_the_existing_snapshot_frame() {
        let content = b"changed bytes";
        let manifest = test_manifest(content);
        let request = request(test_manifest(b"old bytes"));
        let mut output = Vec::new();
        write_capture(&request, &manifest, &mut output, |sink| {
            sink.write_all(content).map_err(|error| error.to_string())
        })
        .unwrap();
        let (response, body) = decode(&output);
        assert_eq!(response.disposition, SnapshotCaptureDisposition::Snapshot);
        let encoded = serde_json::to_vec(&manifest).unwrap();
        assert_eq!(&body[..4], (encoded.len() as u32).to_be_bytes());
        assert_eq!(&body[4..4 + encoded.len()], encoded);
        assert_eq!(&body[4 + encoded.len()..], content);
    }

    #[test]
    fn mutation_during_revalidation_cannot_emit_an_unchanged_receipt() {
        let manifest = test_manifest(b"stable");
        let mut output = Vec::new();
        let result = write_capture(&request(manifest.clone()), &manifest, &mut output, |_| {
            Err("workspace snapshot source changed".into())
        });
        assert!(result.is_err());
        assert!(
            output.is_empty(),
            "no success receipt before verification completes"
        );
    }

    #[test]
    fn second_content_pass_rejects_equal_size_mutation() {
        let expected = test_manifest(b"before").files.remove(0);
        assert!(copy_snapshot_content(
            &mut Cursor::new(b"after!"),
            &expected,
            &mut std::io::sink(),
            &mut SnapshotObservation::default()
        )
        .is_err());
        assert!(copy_snapshot_content(
            &mut Cursor::new(b"before"),
            &expected,
            &mut std::io::sink(),
            &mut SnapshotObservation::default()
        )
        .is_ok());
    }

    #[test]
    fn active_process_before_or_after_capture_never_receives_unchanged_acknowledgement() {
        for busy_check in [1, 2] {
            let manifest = test_manifest(b"stable");
            let mut checks = 0;
            let mut output = vec![];
            let result = capture_idle(
                &request(manifest.clone()),
                &mut output,
                &mut SnapshotObservation::default(),
                || {
                    checks += 1;
                    if checks == busy_check {
                        Err("agent process present".into())
                    } else {
                        Ok(())
                    }
                },
                |_| Ok(manifest),
                |_, _, _| Ok(()),
            );
            assert!(result.is_err());
            assert!(output.is_empty());
            assert_eq!(checks, busy_check);
        }
    }

    #[test]
    fn empty_and_metadata_changes_emit_full_frames() {
        let nonempty = test_manifest(b"stable");
        let empty = SandboxWorkspaceSnapshotManifest {
            schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
            files: vec![],
        };
        let mut executable = nonempty.clone();
        executable.files[0].executable = true;
        let mut renamed = nonempty.clone();
        renamed.files[0].path = "renamed.bin".into();
        // Inventory changes are reflected by the existing protected-path scan:
        // newly protected files disappear; newly unprotected files reappear.
        for (baseline, current) in [
            (None, empty.clone()),
            (Some(nonempty.clone()), empty),
            (None, nonempty.clone()),
            (Some(nonempty.clone()), executable),
            (Some(nonempty), renamed),
        ] {
            let request = SandboxSnapshotCaptureRequest {
                schema: SANDBOX_SNAPSHOT_CAPTURE_SCHEMA.into(),
                capture_id: "empty-1".into(),
                baseline: baseline.map(|manifest| SandboxSnapshotBaseline {
                    snapshot_sha256: format!("sha256:{}", "a".repeat(64)),
                    manifest,
                }),
            };
            let mut verified = false;
            let mut output = vec![];
            write_capture(&request, &current, &mut output, |_| {
                verified = true;
                Ok(())
            })
            .unwrap();
            assert!(verified, "even an empty frame must finish the idle check");
            let (response, body) = decode(&output);
            assert_eq!(response.disposition, SnapshotCaptureDisposition::Snapshot);
            assert_eq!(body.is_empty(), current.files.is_empty());
        }
    }

    #[test]
    fn malformed_baselines_and_unknown_protocol_fields_are_rejected() {
        let mut request = request(test_manifest(b"stable"));
        request.baseline.as_mut().unwrap().manifest.files[0].path = "../escape".into();
        assert!(validate_request(&request).is_err());
        request.baseline = None;
        request.schema = "workspace.snapshot.capture.unsupported".into();
        assert!(validate_request(&request).is_err());
        assert!(serde_json::from_str::<SandboxSnapshotCaptureRequest>(r#"{"schema":"workspace.snapshot.capture.v1","captureId":"1","baseline":null,"unknown":1}"#).is_err());
    }

    #[test]
    fn conditional_capture_observes_both_actual_reads_including_unchanged_and_failure() {
        for (unchanged, fail_second) in [(true, false), (false, false), (true, true)] {
            let body = b"measured content";
            let manifest = test_manifest(body);
            let mut request = request(manifest.clone());
            if !unchanged {
                request.baseline = None;
            }
            let mut observation = SnapshotObservation {
                operation_id: Some("capture-read-test".into()),
                ..Default::default()
            };
            let mut diagnostics = vec![];
            let mut output = vec![];
            let result = observed_capture(&mut observation, &mut diagnostics, |observation| {
                capture_idle(
                    &request,
                    &mut output,
                    observation,
                    || Ok(()),
                    |observation| {
                        hash_snapshot_file(&mut Cursor::new(body), observation)?;
                        observation.scanned_entry();
                        Ok(manifest)
                    },
                    |manifest, sink, observation| {
                        let content: &[u8] = if fail_second {
                            b"replaced content"
                        } else {
                            body
                        };
                        copy_snapshot_content(
                            &mut Cursor::new(content),
                            &manifest.files[0],
                            sink,
                            observation,
                        )
                    },
                )
            });
            assert_eq!(result.is_err(), fail_second);
            if unchanged && !fail_second {
                assert!(decode(&output).1.is_empty());
            }
            if unchanged && fail_second {
                assert!(output.is_empty());
            }
            let events = diagnostics
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                events.len(),
                2,
                "both counters must be reported even when the helper fails"
            );
            assert_eq!(events[0]["metric"], "scannedEntries");
            assert_eq!(events[0]["value"], 1);
            assert_eq!(events[1]["metric"], "sourceReadBytes");
            assert_eq!(events[1]["value"], 2 * body.len() as u64);
            assert_eq!(events[1]["operationId"], "capture-read-test");
            assert_eq!(
                events[1]["outcome"],
                if fail_second { "error" } else { "ok" }
            );
        }
    }

    #[test]
    fn conditional_capture_reports_zero_reads_for_early_rejection_and_disabled_is_quiet() {
        for enabled in [true, false] {
            let mut observation = SnapshotObservation {
                operation_id: enabled.then(|| "early-rejection".into()),
                ..Default::default()
            };
            let mut diagnostics = vec![];
            let result = observed_capture(&mut observation, &mut diagnostics, |_| {
                Err("agent process present".into())
            });
            assert!(result.is_err());
            if enabled {
                let events = String::from_utf8(diagnostics)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                    .collect::<Vec<_>>();
                assert_eq!(events.len(), 2);
                assert!(events
                    .iter()
                    .all(|event| event["value"] == 0 && event["outcome"] == "error"));
            } else {
                assert!(diagnostics.is_empty());
            }
        }
    }
}
