use super::*;
use crate::session_workspace::snapshot::inspect_workspace_snapshot_manifest_for_test;

#[test]
fn cross_language_fixture_restores_identical_manifest_and_bodies() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/workspace/fixtures/workspace_archive/v1/valid.json"
    ))
    .unwrap();
    let bytes = |name: &str| -> Vec<u8> {
        let text = fixture[name].as_str().unwrap();
        assert_eq!(text.len() % 2, 0);
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
            .collect()
    };
    let manifest: SandboxWorkspaceSnapshotManifest =
        serde_json::from_value(fixture["manifest"].clone()).unwrap();
    let archive = bytes("tarZstdV1Hex");
    let frame = bytes("frameV1Hex");
    let body = bytes("bodyHex");
    let mut restored = Vec::new();
    let summary = decode_to_v1(archive.as_slice(), &mut restored, limits()).unwrap();
    assert_eq!(summary.manifest, manifest);
    assert_eq!(summary.expanded_bytes, body.len() as u64);
    assert_eq!(restored, frame);
    let prefix = 4 + serde_json::to_vec(&manifest).unwrap().len();
    assert_eq!(&frame[prefix..], body);
    assert_eq!(
        inspect_workspace_snapshot_manifest_for_test(&mut frame.as_slice()).unwrap(),
        Some(manifest)
    );
}

#[test]
#[ignore = "explicit cross-language fixture generation"]
fn generate_cross_language_archive_fixture() {
    let path = format!("nested/{}/数据.bin", "x".repeat(180));
    let script = b"#!/bin/sh\necho ready\n";
    let binary: Vec<u8> = (0..=255).cycle().take(2048).collect();
    let files = manifest(vec![
        file("a-empty", b"", false),
        file("bin/run", script, true),
        file(&path, &binary, false),
    ]);
    let body = [script.as_slice(), binary.as_slice()].concat();
    let mut archive = Vec::new();
    encode_ordered(&files, body.as_slice(), &mut archive, limits()).unwrap();
    let mut frame = Vec::new();
    decode_to_v1(archive.as_slice(), &mut frame, limits()).unwrap();
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
    let value = serde_json::json!({
        "manifest": files, "bodyHex": hex(&body),
        "frameV1Hex": hex(&frame), "tarZstdV1Hex": hex(&archive),
    });
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/workspace/fixtures/workspace_archive/v1");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("valid.json"),
        serde_json::to_vec_pretty(&value).unwrap(),
    )
    .unwrap();
}

#[test]
fn ordered_body_boundaries_match_independent_sources_and_existing_v1_parser() {
    let path = format!("z/{}", "数据".repeat(60));
    let files = manifest(vec![
        file("a-empty", b"", false),
        file("b", b"first", true),
        file("c-empty", b"", false),
        file(&path, b"second-body", false),
        file("zz-empty", b"", false),
    ]);
    let payloads: [&[u8]; 5] = [b"", b"first", b"", b"second-body", b""];
    let expected = encoded(&files, &payloads);
    let mut archive = Vec::new();
    let summary = encode_ordered(
        &files,
        b"firstsecond-body".as_slice(),
        &mut archive,
        limits(),
    )
    .unwrap();
    assert_eq!(archive, expected);
    let mut v1 = Vec::new();
    assert_eq!(
        decode_to_v1(archive.as_slice(), &mut v1, limits()).unwrap(),
        summary
    );
    assert_eq!(
        inspect_workspace_snapshot_manifest_for_test(&mut v1.as_slice()).unwrap(),
        Some(files)
    );
}

#[test]
fn ordered_source_rejects_short_long_wrong_content_and_failed_final_read() {
    let files = manifest(vec![
        file("a", b"abc", false),
        file("b-empty", b"", false),
        file("c", b"de", false),
    ]);
    encode_ordered(&files, b"abcde".as_slice(), io::sink(), limits()).unwrap();
    for source in [b"".as_slice(), b"ab", b"abc", b"abcd", b"abcdef", b"abcdf"] {
        assert!(encode_ordered(&files, source, io::sink(), limits()).is_err());
    }
    struct FailedEnd;
    impl Read for FailedEnd {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("terminal source failure"))
        }
    }
    let source = b"abcde".as_slice().chain(FailedEnd);
    let error = encode_ordered(&files, source, io::sink(), limits()).unwrap_err();
    assert!(error.to_string().contains("terminal source failure"));
}

#[test]
fn v1_bridge_rejects_archive_failure_and_destination_failure_after_partial_output() {
    let files = manifest(vec![file("file", b"abc", false)]);
    let valid = encoded(&files, &[b"abc"]);
    decode_to_v1(valid.as_slice(), io::sink(), limits()).unwrap();
    let mut trailing = valid.clone();
    trailing.extend_from_slice(&valid);
    let corrupt_body = compressed(&raw_tar(
        &files,
        "workspace/file",
        b"abd",
        tar::EntryType::Regular,
    ));
    for bytes in [&valid[..valid.len() - 1], &trailing, &corrupt_body] {
        let mut staging = Vec::new();
        assert!(decode_to_v1(bytes, &mut staging, limits()).is_err());
        assert!(!staging.is_empty(), "partial output must not imply success");
    }
    struct FailedSink;
    impl Write for FailedSink {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("v1 destination failed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let prefix_size = 4 + serde_json::to_vec(&files).unwrap().len();
    let mut prefix = vec![0; prefix_size];
    let sink = io::Cursor::new(prefix.as_mut_slice());
    // A fixed-size destination accepts the manifest then fails on the body.
    assert!(decode_to_v1(valid.as_slice(), sink, limits()).is_err());
    assert_eq!(
        decode_to_v1(valid.as_slice(), FailedSink, limits())
            .unwrap_err()
            .to_string(),
        "v1 destination failed"
    );
}

#[test]
fn eight_mib_nonseekable_body_transcodes_through_one_compressed_file_to_valid_v1() {
    const SIZE: u64 = 8 * 1024 * 1024;
    struct BoundedSource {
        remaining: u64,
        output_bytes: Rc<Cell<u64>>,
    }
    impl Read for BoundedSource {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            assert!(bytes.len() <= 64 * 1024, "bounded streaming read");
            if self.remaining < SIZE - 1024 * 1024 {
                assert!(
                    self.output_bytes.get() > 0,
                    "output must progress before EOF"
                );
            }
            let count = bytes.len().min(self.remaining as usize);
            bytes[..count].fill(b'x');
            self.remaining -= count as u64;
            Ok(count)
        }
    }
    struct FileSink {
        file: std::fs::File,
        bytes: Rc<Cell<u64>>,
    }
    impl Write for FileSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let count = self.file.write(bytes)?;
            self.bytes.set(self.bytes.get() + count as u64);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }
    let mut digest = Sha256::new();
    for _ in 0..128 {
        digest.update([b'x'; 64 * 1024]);
    }
    let files = manifest(vec![SandboxWorkspaceSnapshotFile {
        path: "large.bin".into(),
        size_bytes: SIZE,
        sha256: format!("sha256:{:x}", digest.finalize()),
        executable: false,
    }]);
    let directory = tempfile::tempdir().unwrap();
    let archive_path = directory.path().join("workspace.tar.zst");
    let output_bytes = Rc::new(Cell::new(0));
    let summary = encode_ordered(
        &files,
        BoundedSource {
            remaining: SIZE,
            output_bytes: output_bytes.clone(),
        },
        FileSink {
            file: std::fs::File::create(&archive_path).unwrap(),
            bytes: output_bytes.clone(),
        },
        limits(),
    )
    .unwrap();
    assert!(summary.compressed_bytes < SIZE / 100);
    assert_eq!(summary.compressed_bytes, output_bytes.get());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    let v1_path = directory.path().join("restore.frame");
    assert_eq!(
        decode_to_v1(
            std::fs::File::open(&archive_path).unwrap(),
            std::fs::File::create(&v1_path).unwrap(),
            limits(),
        )
        .unwrap(),
        summary
    );
    assert_eq!(summary.expanded_bytes, SIZE);
    assert_eq!(
        inspect_workspace_snapshot_manifest_for_test(&mut std::fs::File::open(&v1_path).unwrap())
            .unwrap(),
        Some(files)
    );
}
