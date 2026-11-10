use super::*;
use hosted_execution::protocol::SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA;
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::io::Cursor;
use std::rc::Rc;

mod bridges;

const MANIFEST_PATH: &str = "workspace-manifest.json";

fn limits() -> ArchiveLimits {
    ArchiveLimits {
        compressed_bytes: 16 * 1024 * 1024,
        expanded_bytes: 16 * 1024 * 1024,
        file_count: 32,
        manifest_bytes: 1024 * 1024,
        window_log_max: 20,
    }
}

fn file(path: &str, bytes: &[u8], executable: bool) -> SandboxWorkspaceSnapshotFile {
    SandboxWorkspaceSnapshotFile {
        path: path.into(),
        size_bytes: bytes.len() as u64,
        sha256: format!("sha256:{:x}", Sha256::digest(bytes)),
        executable,
    }
}

fn manifest(files: Vec<SandboxWorkspaceSnapshotFile>) -> SandboxWorkspaceSnapshotManifest {
    SandboxWorkspaceSnapshotManifest {
        schema: SANDBOX_WORKSPACE_SNAPSHOT_SCHEMA.into(),
        files,
    }
}

fn encoded(manifest: &SandboxWorkspaceSnapshotManifest, files: &[&[u8]]) -> Vec<u8> {
    let mut output = Vec::new();
    let mut index = 0;
    encode(
        manifest,
        |_| {
            let source = Cursor::new(files[index]);
            index += 1;
            Ok(source)
        },
        &mut output,
        limits(),
    )
    .unwrap();
    output
}

fn restored(bytes: &[u8], bounds: ArchiveLimits) -> io::Result<(ArchiveSummary, Vec<Vec<u8>>)> {
    let mut files = Vec::new();
    let summary = decode(
        Cursor::new(bytes),
        |_, source| {
            let mut bytes = Vec::new();
            source.read_to_end(&mut bytes)?;
            files.push(bytes);
            Ok(())
        },
        bounds,
    )?;
    Ok((summary, files))
}

fn tar_header(path: &str, bytes: &[u8], mode: u32, kind: tar::EntryType) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_path(path).unwrap();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_entry_type(kind);
    header.set_cksum();
    header
}

fn raw_tar(
    manifest: &SandboxWorkspaceSnapshotManifest,
    name: &str,
    body: &[u8],
    kind: tar::EntryType,
) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    let json = serde_json::to_vec(manifest).unwrap();
    tar.append(
        &tar_header(MANIFEST_PATH, &json, 0o644, tar::EntryType::Regular),
        json.as_slice(),
    )
    .unwrap();
    let mut header = tar_header(name, body, 0o644, kind);
    if kind.is_symlink() || kind.is_hard_link() {
        header.set_link_name("outside").unwrap();
        header.set_cksum();
    }
    tar.append(&header, body).unwrap();
    tar.into_inner().unwrap()
}

fn compressed(tar: &[u8]) -> Vec<u8> {
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    encoder.window_log(20).unwrap();
    encoder.include_checksum(true).unwrap();
    encoder.write_all(tar).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn actual_tar_zstd_roundtrip_preserves_manifest_order_content_and_executable_bits() {
    let long_path = format!("nested/{}/数据.txt", "x".repeat(180));
    let large = vec![b'x'; 512 * 1024];
    let files = manifest(vec![
        file("a-empty", b"", false),
        file("bin/run", b"#!/bin/sh", true),
        file(&long_path, &large, false),
    ]);
    let payloads: [&[u8]; 3] = [b"", b"#!/bin/sh", &large];
    let bytes = encoded(&files, &payloads);
    assert!(
        bytes.len() < large.len() / 20,
        "must actually compress repeated data"
    );
    assert_eq!(&bytes[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
    let unpacked = zstd::stream::decode_all(bytes.as_slice()).unwrap();
    let mut tar = tar::Archive::new(unpacked.as_slice());
    let names: Vec<_> = tar
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path_bytes().into_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            MANIFEST_PATH.as_bytes().to_vec(),
            b"workspace/a-empty".to_vec(),
            b"workspace/bin/run".to_vec(),
            format!("workspace/{long_path}").into_bytes()
        ]
    );
    let (summary, output) = restored(&bytes, limits()).unwrap();
    assert_eq!(summary.manifest, files);
    assert_eq!(
        output,
        payloads
            .iter()
            .map(|body| body.to_vec())
            .collect::<Vec<_>>()
    );
    assert_eq!(summary.compressed_bytes, bytes.len() as u64);
    assert_eq!(
        summary.compressed_sha256,
        format!("sha256:{:x}", Sha256::digest(&bytes))
    );
    assert_eq!(summary.expanded_bytes, (large.len() + 9) as u64);
    assert_eq!(summary.file_count, 3);
    assert_eq!(
        encoded(&files, &payloads),
        bytes,
        "identical input must have deterministic headers"
    );
}

#[test]
fn file_content_hash_short_and_long_sources_are_rejected() {
    let files = manifest(vec![file("file", b"abc", false)]);
    for source in [b"abd".as_slice(), b"ab", b"abcd"] {
        let result = encode(&files, |_| Ok(Cursor::new(source)), io::sink(), limits());
        assert!(result.is_err());
    }
    let bad = compressed(&raw_tar(
        &files,
        "workspace/file",
        b"abd",
        tar::EntryType::Regular,
    ));
    assert!(restored(&bad, limits()).is_err());
}

#[test]
fn shared_manifest_semantics_reject_invalid_paths_order_and_hashes_before_source_open() {
    for path in [
        "../escape",
        "/absolute",
        "a//b",
        "a\\b",
        "a/./b",
        "bad\nname",
        "e\u{301}",
    ] {
        let files = manifest(vec![file(path, b"x", false)]);
        assert!(encode(
            &files,
            |_| -> io::Result<io::Empty> { panic!("invalid manifest must not open files") },
            io::sink(),
            limits()
        )
        .is_err());
        let bad = compressed(&raw_tar(
            &files,
            "workspace/file",
            b"x",
            tar::EntryType::Regular,
        ));
        assert!(decode(
            bad.as_slice(),
            |_, _| panic!("invalid manifest must not reach consumer"),
            limits()
        )
        .is_err());
    }
    for files in [
        manifest(vec![]),
        manifest(vec![file("b", b"x", false), file("a", b"x", false)]),
        manifest(vec![file("a", b"x", false), file("a/b", b"x", false)]),
    ] {
        assert!(encode(&files, |_| Ok(io::empty()), io::sink(), limits()).is_err());
    }
}

#[test]
fn decoder_rejects_links_special_nodes_wrong_order_size_and_mode() {
    let files = manifest(vec![file("file", b"abc", false)]);
    for kind in [
        tar::EntryType::Symlink,
        tar::EntryType::Link,
        tar::EntryType::Directory,
        tar::EntryType::Fifo,
        tar::EntryType::GNUSparse,
        tar::EntryType::XGlobalHeader,
    ] {
        assert!(restored(
            &compressed(&raw_tar(&files, "workspace/file", b"abc", kind)),
            limits()
        )
        .is_err());
    }
    for name in ["workspace/other", "other/file"] {
        assert!(restored(
            &compressed(&raw_tar(&files, name, b"abc", tar::EntryType::Regular)),
            limits()
        )
        .is_err());
    }
    assert!(restored(
        &compressed(&raw_tar(
            &files,
            "workspace/file",
            b"ab",
            tar::EntryType::Regular
        )),
        limits()
    )
    .is_err());
    let mut tar = raw_tar(&files, "workspace/file", b"abc", tar::EntryType::Regular);
    let mut header = tar::Header::from_byte_slice(&tar[1024..1536]).clone();
    header.set_mode(0o755);
    header.set_cksum();
    tar[1024..1536].copy_from_slice(header.as_bytes());
    assert!(restored(&compressed(&tar), limits()).is_err());
}

#[test]
fn declared_resource_limits_fail_before_consumer_and_compressed_writer_cannot_exceed_budget() {
    let files = manifest(vec![file("file", b"abc", false)]);
    let bytes = encoded(&files, &[b"abc"]);
    for bounds in [
        ArchiveLimits {
            expanded_bytes: 2,
            ..limits()
        },
        ArchiveLimits {
            file_count: 0,
            ..limits()
        },
        ArchiveLimits {
            manifest_bytes: 8,
            ..limits()
        },
        ArchiveLimits {
            compressed_bytes: 8,
            ..limits()
        },
    ] {
        assert!(decode(
            bytes.as_slice(),
            |_, _| panic!("quota rejection must precede consumer"),
            bounds
        )
        .is_err());
        let mut output = Vec::new();
        assert!(encode(&files, |_| Ok(Cursor::new(b"abc")), &mut output, bounds).is_err());
        assert!(output.len() as u64 <= bounds.compressed_bytes);
    }
}

#[test]
fn decoder_rejects_truncated_corrupt_and_concatenated_zstd_or_trailing_tar_data() {
    let files = manifest(vec![file("file", b"abc", false)]);
    let bytes = encoded(&files, &[b"abc"]);
    for end in [0, 1, bytes.len() / 2, bytes.len() - 1] {
        assert!(restored(&bytes[..end], limits()).is_err(), "cut at {end}");
    }
    let mut corrupt = bytes.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(restored(&corrupt, limits()).is_err());
    for suffix in [bytes.clone(), b"trailing".to_vec()] {
        let mut extra = bytes.clone();
        extra.extend(suffix);
        assert!(restored(&extra, limits()).is_err());
    }
    let tar = raw_tar(&files, "workspace/file", b"abc", tar::EntryType::Regular);
    for extra in [vec![0; 512], b"junk".to_vec(), tar.clone()] {
        let mut trailing = tar.clone();
        trailing.extend(extra);
        assert!(restored(&compressed(&trailing), limits()).is_err());
    }
    for cut in [512, 1024, 1025] {
        assert!(restored(&compressed(&tar[..tar.len() - cut]), limits()).is_err());
    }
}

#[test]
fn consumer_must_consume_the_complete_file_and_failure_never_returns_a_summary() {
    let files = manifest(vec![file("file", b"abc", false)]);
    let bytes = encoded(&files, &[b"abc"]);
    assert!(decode(bytes.as_slice(), |_, _| Ok(()), limits()).is_err());
    assert!(decode(
        bytes.as_slice(),
        |_, _| Err(io::Error::other("consumer failed")),
        limits()
    )
    .is_err());
}

struct ProgressReader {
    source: Cursor<Vec<u8>>,
    output_bytes: Rc<Cell<usize>>,
}

impl Read for ProgressReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        assert!(buffer.len() <= 64 * 1024, "bounded content copy buffer");
        if self.source.position() > 1024 * 1024 {
            assert!(
                self.output_bytes.get() > 64 * 1024,
                "compression must make output progress before source EOF"
            );
        }
        self.source.read(buffer)
    }
}

struct ProgressWriter(Rc<Cell<usize>>);

impl Write for ProgressWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.set(self.0.get() + bytes.len());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn nonseekable_source_and_sink_make_bounded_progress_without_materializing_a_tar() {
    let mut state = 0x1234_5678_u32;
    let data: Vec<u8> = (0..2 * 1024 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    let files = manifest(vec![file("file", &data, false)]);
    let written = Rc::new(Cell::new(0));
    let result = encode(
        &files,
        |_| {
            Ok(ProgressReader {
                source: Cursor::new(data.clone()),
                output_bytes: written.clone(),
            })
        },
        ProgressWriter(written.clone()),
        limits(),
    )
    .unwrap();
    assert_eq!(result.expanded_bytes, data.len() as u64);
    assert_eq!(result.compressed_bytes, written.get() as u64);
}

#[test]
fn legal_maximum_path_and_exact_resource_limits_roundtrip() {
    let path = "x".repeat(4096);
    let files = manifest(vec![file(&path, b"abc", true)]);
    let bytes = encoded(&files, &[b"abc"]);
    let exact = ArchiveLimits {
        compressed_bytes: bytes.len() as u64,
        expanded_bytes: 3,
        file_count: 1,
        manifest_bytes: serde_json::to_vec(&files).unwrap().len(),
        ..limits()
    };
    assert_eq!(restored(&bytes, exact).unwrap().1, vec![b"abc".to_vec()]);
    let mut output = Vec::new();
    let summary = encode(&files, |_| Ok(Cursor::new(b"abc")), &mut output, exact).unwrap();
    assert_eq!(output, bytes);
    assert_eq!(summary.file_count, 1);
}

#[test]
fn file_count_and_decoder_window_limits_reject_before_any_file_consumer() {
    let files = manifest(vec![file("a", b"a", false), file("b", b"b", false)]);
    let bytes = encoded(&files, &[b"a", b"b"]);
    assert!(decode(
        bytes.as_slice(),
        |_, _| panic!("file count limit must precede file writes"),
        ArchiveLimits {
            file_count: 1,
            ..limits()
        }
    )
    .is_err());
    let files = manifest(vec![file("file", b"abc", false)]);
    let raw = raw_tar(&files, "workspace/file", b"abc", tar::EntryType::Regular);
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    encoder.window_log(21).unwrap();
    encoder.write_all(&raw).unwrap();
    let wide = encoder.finish().unwrap();
    assert!(decode(
        wide.as_slice(),
        |_, _| panic!("window limit must precede file writes"),
        limits()
    )
    .is_err());
}

#[test]
fn noncanonical_manifest_unknown_fields_and_oversized_long_name_are_rejected() {
    let files = manifest(vec![file("file", b"abc", false)]);
    for json in [
        serde_json::to_string_pretty(&files).unwrap().into_bytes(),
        br#"{"schema":"workspace.snapshot.v1","files":[],"unexpected":true}"#.to_vec(),
    ] {
        let mut tar = tar::Builder::new(Vec::new());
        tar.append(
            &tar_header(MANIFEST_PATH, &json, 0o644, tar::EntryType::Regular),
            json.as_slice(),
        )
        .unwrap();
        let bytes = compressed(&tar.into_inner().unwrap());
        assert!(decode(
            bytes.as_slice(),
            |_, _| panic!("manifest must be validated first"),
            limits()
        )
        .is_err());
    }
    let mut raw = raw_tar(&files, "workspace/file", b"abc", tar::EntryType::Regular);
    let mut long = tar_header("././@LongLink", b"", 0o644, tar::EntryType::GNULongName);
    long.set_size(u64::MAX / 2);
    long.set_cksum();
    raw[1024..1536].copy_from_slice(long.as_bytes());
    let bytes = compressed(&raw);
    assert!(decode(
        bytes.as_slice(),
        |_, _| panic!("long name bound must precede file writes"),
        limits()
    )
    .is_err());
}

#[test]
fn encoder_propagates_source_and_sink_errors_without_success_metadata() {
    struct FailedSource;
    impl Read for FailedSource {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("source failed"))
        }
    }
    struct FailedSink;
    impl Write for FailedSink {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("sink failed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let files = manifest(vec![file("file", b"abc", false)]);
    assert!(encode(&files, |_| Ok(FailedSource), io::sink(), limits()).is_err());
    assert!(encode(&files, |_| Ok(Cursor::new(b"abc")), FailedSink, limits()).is_err());
}

#[test]
fn large_generated_source_streams_directly_into_one_compressed_file() {
    let size = 8 * 1024 * 1024;
    let mut digest = Sha256::new();
    for _ in 0..128 {
        digest.update([b'x'; 64 * 1024]);
    }
    let expected_sha = format!("sha256:{:x}", digest.finalize());
    let files = manifest(vec![SandboxWorkspaceSnapshotFile {
        path: "large.bin".into(),
        size_bytes: size,
        sha256: expected_sha.clone(),
        executable: false,
    }]);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("workspace.tar.zst");
    let output = std::fs::File::create(&path).unwrap();
    let summary = encode(
        &files,
        |_| Ok(io::repeat(b'x').take(size)),
        output,
        limits(),
    )
    .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        summary.compressed_bytes
    );
    assert!(summary.compressed_bytes < size / 100);
    let mut restored_digest = Sha256::new();
    let mut restored_size = 0;
    let decoded = decode(
        std::fs::File::open(&path).unwrap(),
        |_, source| {
            let mut buffer = [0; 64 * 1024];
            loop {
                let count = source.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                restored_size += count as u64;
                restored_digest.update(&buffer[..count]);
            }
            Ok(())
        },
        limits(),
    )
    .unwrap();
    assert_eq!(decoded, summary);
    assert_eq!(restored_size, size);
    assert_eq!(
        format!("sha256:{:x}", restored_digest.finalize()),
        expected_sha
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}
