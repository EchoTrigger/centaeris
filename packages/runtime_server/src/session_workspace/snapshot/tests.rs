//! Characterization of the workspace stream shared by provider transports.
use super::*;

struct Fragmented<'a> {
    bytes: &'a [u8],
    reads: usize,
}
impl Read for Fragmented<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let length = self.bytes.len().min(output.len()).min(3);
        output[..length].copy_from_slice(&self.bytes[..length]);
        self.bytes = &self.bytes[length..];
        self.reads += 1;
        Ok(length)
    }
}
fn frame(content: &[u8]) -> Vec<u8> {
    let manifest = SandboxWorkspaceSnapshotManifest {
        schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
        files: vec![SandboxWorkspaceSnapshotFile {
            path: "nested/binary.dat".into(),
            size_bytes: content.len() as u64,
            sha256: format!("sha256:{:x}", Sha256::digest(content)),
            executable: true,
        }],
    };
    let metadata = serde_json::to_vec(&manifest).unwrap();
    let mut frame = (metadata.len() as u32).to_be_bytes().to_vec();
    frame.extend(metadata);
    frame.extend(content);
    frame
}

#[test]
fn snapshot_codec_contract_preserves_fragmented_binary_stream_and_frozen_spool() {
    let content = b"\0\xff\x80binary\n\r\0";
    let wire = frame(content);
    let mut source = Fragmented {
        bytes: &wire,
        reads: 0,
    };
    let mut captured = capture_snapshot_artifact(&mut source, wire.len() as u64).unwrap();
    assert!(source.reads > 10);
    assert_eq!(
        captured.descriptor.sha256,
        format!("sha256:{:x}", Sha256::digest(&wire))
    );
    assert_eq!(
        captured.descriptor.expanded_size_bytes,
        content.len() as u64
    );
    assert_eq!(captured.descriptor.file_count, 1);
    assert_eq!(
        captured.manifest.as_ref().unwrap().files[0].path,
        "nested/binary.dat"
    );
    let mut restored = Vec::new();
    captured.file.read_to_end(&mut restored).unwrap();
    assert_eq!(restored, wire);
}

#[test]
fn snapshot_codec_contract_rejects_incomplete_changed_or_trailing_bytes() {
    let wire = frame(b"original");
    let mut truncated = wire.clone();
    truncated.pop();
    let mut changed = wire.clone();
    *changed.last_mut().unwrap() ^= 1;
    let mut trailing = wire.clone();
    trailing.push(0);
    for bytes in [truncated, changed, trailing] {
        let limit = bytes.len() as u64;
        assert!(capture_snapshot_artifact(
            &mut Fragmented {
                bytes: &bytes,
                reads: 0
            },
            limit
        )
        .is_err());
    }
    assert!(capture_snapshot_artifact(&mut wire.as_slice(), wire.len() as u64 - 1).is_err());
}
