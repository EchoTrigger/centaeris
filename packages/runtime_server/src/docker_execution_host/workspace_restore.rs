//! Translate verified stored workspace bytes into the sealed helper's frame.
use super::*;

pub(super) struct PreparedWorkspaceRestore {
    pub(super) stored_descriptor: WorkspaceSnapshotDescriptor,
    pub(super) helper_frame: ImmutableWorkspaceSnapshot,
}

const ENCODING_HEADER: &str = "x-workspace-snapshot-encoding";
const ARCHIVE_WINDOW_LOG_MAX: u32 = 20;

pub(super) fn response_encoding(headers: &reqwest::header::HeaderMap) -> Result<String, String> {
    let mut values = headers.get_all(ENCODING_HEADER).iter();
    let value = values.next().ok_or("workspace_restore_encoding_rejected")?;
    if values.next().is_some() {
        return Err("workspace_restore_encoding_rejected".into());
    }
    let value = value
        .to_str()
        .map_err(|_| "workspace_restore_encoding_rejected")?;
    require_encoding(Some(value))?;
    Ok(value.into())
}
fn require_encoding(encoding: Option<&str>) -> Result<&str, String> {
    match encoding {
        Some(value @ ("frameV1" | "tarZstdV1" | "fileManifestV1")) => Ok(value),
        _ => Err("workspace_restore_encoding_rejected".into()),
    }
}

pub(super) fn prepare_with_files(
    source: &mut impl Read,
    stored: &WorkspaceSnapshotDescriptor,
    encoding: Option<&str>,
    expanded_limit: u64,
    observation: WorkspaceProtection,
    access: Option<&workspace_files::FileAccess<'_>>,
) -> Result<PreparedWorkspaceRestore, String> {
    if require_encoding(encoding)? == workspace_files::ENCODING {
        return workspace_files::restore_from_api(
            source,
            stored,
            expanded_limit,
            observation,
            access.ok_or("workspace_restore_file_authority_missing")?,
        );
    }
    prepare(source, stored, encoding, expanded_limit, observation)
}

/// Validate the complete stored object before decoding into a private helper
/// frame. The API/Core descriptor remains the original stored-byte identity.
pub(super) fn prepare(
    source: &mut impl Read,
    stored: &WorkspaceSnapshotDescriptor,
    encoding: Option<&str>,
    expanded_limit: u64,
    observation: WorkspaceProtection,
) -> Result<PreparedWorkspaceRestore, String> {
    let encoding = require_encoding(encoding)?;
    let helper_limit = hosted_execution::host_supervisor::HELPER_STREAM_LIMIT;
    if stored.expanded_size_bytes > expanded_limit || stored.size_bytes > helper_limit {
        return Err("workspace_restore_budget_exceeded".into());
    }
    let mut input = tempfile::tempfile().map_err(|_| "workspace_restore_spool_unavailable")?;
    let mut measured = MeasuredWriter {
        inner: &mut input,
        written: 0,
    };
    let verified = copy_exact(source, &mut measured, stored.size_bytes, &stored.sha256);
    observation.count(
        ProtectionMetric::SpoolWrittenBytes,
        measured.written,
        verified.is_ok(),
    );
    verified.map_err(|_| "workspace_restore_stored_binding_rejected")?;
    input
        .rewind()
        .map_err(|_| "workspace_restore_spool_unavailable")?;
    let mut frame = match encoding {
        "frameV1" => input,
        "tarZstdV1" => {
            let mut frame =
                tempfile::tempfile().map_err(|_| "workspace_restore_spool_unavailable")?;
            let mut measured = MeasuredWriter {
                inner: &mut frame,
                written: 0,
            };
            let decoded = crate::workspace_archive::decode_to_v1(
                &mut input,
                &mut measured,
                crate::workspace_archive::ArchiveLimits {
                    compressed_bytes: stored.size_bytes,
                    expanded_bytes: stored.expanded_size_bytes,
                    file_count: stored.file_count,
                    manifest_bytes: SESSION_WORKSPACE_MANIFEST_LIMIT,
                    window_log_max: ARCHIVE_WINDOW_LOG_MAX,
                },
            );
            observation.count(
                ProtectionMetric::SpoolWrittenBytes,
                measured.written,
                decoded.is_ok(),
            );
            let decoded = decoded.map_err(|_| "workspace_restore_archive_rejected")?;
            if decoded.compressed_bytes != stored.size_bytes
                || decoded.compressed_sha256 != stored.sha256
                || decoded.expanded_bytes != stored.expanded_size_bytes
                || decoded.file_count != stored.file_count
            {
                return Err("workspace_restore_stored_binding_rejected".into());
            }
            frame
                .rewind()
                .map_err(|_| "workspace_restore_spool_unavailable")?;
            frame
        }
        _ => return Err("workspace_restore_encoding_rejected".into()),
    };
    let (descriptor, manifest) = inspect_workspace_snapshot_with_manifest(&mut frame)?;
    if descriptor.size_bytes > helper_limit
        || descriptor.expanded_size_bytes != stored.expanded_size_bytes
        || descriptor.file_count != stored.file_count
        || (encoding == "frameV1" && &descriptor != stored)
    {
        return Err("workspace_restore_stored_binding_rejected".into());
    }
    frame
        .rewind()
        .map_err(|_| "workspace_restore_spool_unavailable")?;
    Ok(PreparedWorkspaceRestore {
        stored_descriptor: stored.clone(),
        helper_frame: ImmutableWorkspaceSnapshot {
            file: frame,
            file_source: None,
            descriptor,
            manifest,
            observation,
        },
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Vec<u8>, Vec<u8>, WorkspaceSnapshotDescriptor) {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/workspace/fixtures/workspace_archive/v1/valid.json"
        ))
        .unwrap();
        let bytes = |name: &str| -> Vec<u8> {
            let hex = fixture[name].as_str().unwrap();
            (0..hex.len())
                .step_by(2)
                .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
                .collect()
        };
        let frame = bytes("frameV1Hex");
        let archive = bytes("tarZstdV1Hex");
        let descriptor = inspect_workspace_snapshot(&mut Cursor::new(&frame)).unwrap();
        (frame, archive, descriptor)
    }
    fn stored_archive(
        frame: &WorkspaceSnapshotDescriptor,
        archive: &[u8],
    ) -> WorkspaceSnapshotDescriptor {
        WorkspaceSnapshotDescriptor {
            sha256: format!("sha256:{:x}", Sha256::digest(archive)),
            size_bytes: archive.len() as u64,
            ..frame.clone()
        }
    }
    fn frame_bytes(mut prepared: PreparedWorkspaceRestore) -> Vec<u8> {
        let mut bytes = Vec::new();
        prepared.helper_frame.file.read_to_end(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn shared_restore_adapter_keeps_stored_tar_identity_and_derives_exact_helper_frame() {
        let (frame, archive, frame_descriptor) = fixture();
        let stored = stored_archive(&frame_descriptor, &archive);
        let prepared = prepare(
            &mut Cursor::new(&archive),
            &stored,
            Some("tarZstdV1"),
            frame_descriptor.expanded_size_bytes,
            WorkspaceProtection::new(),
        )
        .expect("valid legacy tarZstdV1 must reach the helper as frameV1");
        assert_eq!(
            prepared.stored_descriptor, stored,
            "the API/Core object identity must remain the compressed bytes"
        );
        assert_eq!(prepared.helper_frame.descriptor, frame_descriptor);
        assert_ne!(
            prepared.helper_frame.descriptor.sha256,
            prepared.stored_descriptor.sha256
        );
        assert_eq!(
            frame_bytes(prepared),
            frame,
            "binary contents, long paths and executable flags survive translation"
        );
    }
    #[test]
    fn recovery_preparation_accepts_stored_tar_without_rebinding_the_checkpoint() {
        let (frame, archive, frame_descriptor) = fixture();
        let stored = stored_archive(&frame_descriptor, &archive);
        let snapshot = RecoveryWorkspaceSnapshotV1 {
            object_ref: Some("stored-legacy-object".into()),
            snapshot_sha256: stored.sha256.clone(),
            snapshot_size_bytes: stored.size_bytes,
            expanded_size_bytes: stored.expanded_size_bytes,
            file_count: stored.file_count,
        };
        let original = snapshot.clone();
        let mut artifact = prepare_recovery_restore_artifact(
            &mut Cursor::new(&archive),
            &snapshot,
            Some("tarZstdV1"),
            frame_descriptor.expanded_size_bytes,
            WorkspaceProtection::new(),
            None,
        )
        .expect("recovery preparation must not parse compressed archive bytes as a frame header");
        let mut bytes = Vec::new();
        artifact.file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, frame);
        assert_eq!(artifact.descriptor, frame_descriptor);
        assert_eq!(
            snapshot, original,
            "durable checkpoint references must not be rewritten during adaptation"
        );
    }
    #[test]
    fn frame_restore_preserves_its_existing_bytes_and_descriptor() {
        let (frame, _, stored) = fixture();
        let prepared = prepare(
            &mut Cursor::new(&frame),
            &stored,
            Some("frameV1"),
            stored.expanded_size_bytes,
            WorkspaceProtection::new(),
        )
        .unwrap();
        assert_eq!(prepared.stored_descriptor, stored);
        assert_eq!(prepared.helper_frame.descriptor, stored);
        assert_eq!(frame_bytes(prepared), frame);
    }
    #[test]
    fn missing_or_unknown_encoding_is_rejected_before_consuming_the_stored_object() {
        let (frame, _, stored) = fixture();
        for encoding in [
            None,
            Some(""),
            Some("FrameV1"),
            Some("frameV1 "),
            Some("zipV1"),
        ] {
            let mut source = Cursor::new(&frame);
            assert!(
                prepare(
                    &mut source,
                    &stored,
                    encoding,
                    stored.expanded_size_bytes,
                    WorkspaceProtection::new()
                )
                .is_err(),
                "unsupported {encoding:?}"
            );
            assert_eq!(
                source.position(),
                0,
                "encoding must be decided from the exact authenticated response header"
            );
        }
    }
    #[test]
    fn stored_integrity_is_verified_before_a_helper_frame_can_be_returned() {
        let (frame, archive, frame_descriptor) = fixture();
        for (bytes, encoding) in [(&frame, "frameV1"), (&archive, "tarZstdV1")] {
            let stored = if encoding == "frameV1" {
                frame_descriptor.clone()
            } else {
                stored_archive(&frame_descriptor, bytes)
            };
            for mismatch in ["hash", "short", "long"] {
                let mut expected = stored.clone();
                match mismatch {
                    "hash" => expected.sha256 = format!("sha256:{}", "0".repeat(64)),
                    "short" => expected.size_bytes -= 1,
                    "long" => expected.size_bytes += 1,
                    _ => unreachable!(),
                }
                assert!(
                    prepare(
                        &mut Cursor::new(bytes),
                        &expected,
                        Some(encoding),
                        stored.expanded_size_bytes,
                        WorkspaceProtection::new()
                    )
                    .is_err(),
                    "{encoding} {mismatch}"
                );
            }
        }
    }
    #[test]
    fn corrupt_truncated_or_over_expanded_archives_never_return_publishable_helper_input() {
        let (frame, archive, frame_descriptor) = fixture();
        for bytes in [
            archive[..archive.len() - 1].to_vec(),
            vec![0; archive.len()],
        ] {
            let stored = stored_archive(&frame_descriptor, &bytes);
            assert!(prepare(
                &mut Cursor::new(&bytes),
                &stored,
                Some("tarZstdV1"),
                frame_descriptor.expanded_size_bytes,
                WorkspaceProtection::new()
            )
            .is_err());
        }
        for (bytes, encoding) in [(&frame, "frameV1"), (&archive, "tarZstdV1")] {
            let stored = if encoding == "frameV1" {
                frame_descriptor.clone()
            } else {
                stored_archive(&frame_descriptor, bytes)
            };
            assert!(
                prepare(
                    &mut Cursor::new(bytes),
                    &stored,
                    Some(encoding),
                    stored.expanded_size_bytes - 1,
                    WorkspaceProtection::new()
                )
                .is_err(),
                "{encoding} logical budget"
            );
        }
        let mut stored = stored_archive(&frame_descriptor, &archive);
        stored.expanded_size_bytes -= 1;
        assert!(
            prepare(
                &mut Cursor::new(&archive),
                &stored,
                Some("tarZstdV1"),
                frame_descriptor.expanded_size_bytes,
                WorkspaceProtection::new()
            )
            .is_err(),
            "archive manifest may not expand beyond the authoritative stored descriptor"
        );
    }
    #[test]
    fn authenticated_download_encoding_header_is_exact_unique_and_required() {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        assert!(response_encoding(&headers).is_err());
        for encoding in ["frameV1", "tarZstdV1"] {
            headers.insert(ENCODING_HEADER, HeaderValue::from_static(encoding));
            assert_eq!(response_encoding(&headers).unwrap(), encoding);
        }
        for encoding in ["", "FrameV1", "frameV1 ", "zipV1"] {
            headers.insert(ENCODING_HEADER, HeaderValue::from_static(encoding));
            assert!(response_encoding(&headers).is_err());
        }
        headers.insert(ENCODING_HEADER, HeaderValue::from_static("frameV1"));
        headers.append(ENCODING_HEADER, HeaderValue::from_static("tarZstdV1"));
        assert!(response_encoding(&headers).is_err());
    }
}
