//! Host-owned extraction and verified, rewindable publication bytes.
use std::fs::File;
use std::io::{Read, Seek, Write};

use centaeris_core::execution::MAX_PUBLISHED_ARTIFACT_BYTES;
use hosted_execution::host_supervisor::TrustedHelperKind;
use hosted_execution::protocol::{
    SandboxArtifactMetadata, SandboxArtifactRequest, SandboxArtifactStreamHeader,
    SANDBOX_ARTIFACT_STREAM_DIGEST_BYTES, SANDBOX_ARTIFACT_STREAM_HEADER_LIMIT,
    SANDBOX_ARTIFACT_STREAM_SCHEMA,
};
use sha2::{Digest, Sha256};

use super::DockerExecutionHostRunner;

pub(crate) struct PreparedArtifact {
    pub(crate) metadata: SandboxArtifactMetadata,
    body: File,
}

impl PreparedArtifact {
    fn read(source: &mut dyn Read) -> Result<Self, String> {
        let incomplete = |_| "artifact helper frame is truncated";
        let mut length = [0_u8; 4];
        source.read_exact(&mut length).map_err(incomplete)?;
        let length = u32::from_be_bytes(length) as usize;
        if !(1..=SANDBOX_ARTIFACT_STREAM_HEADER_LIMIT).contains(&length) {
            return Err("artifact helper frame header is invalid".into());
        }
        let mut header = vec![0; length];
        source.read_exact(&mut header).map_err(incomplete)?;
        let header: SandboxArtifactStreamHeader = serde_json::from_slice(&header)
            .map_err(|_| "artifact helper frame header is invalid")?;
        if header.schema != SANDBOX_ARTIFACT_STREAM_SCHEMA
            || header.size_bytes > MAX_PUBLISHED_ARTIFACT_BYTES
        {
            return Err("artifact helper frame header is invalid".into());
        }
        let mut body = tempfile::tempfile().map_err(|_| "artifact spool unavailable")?;
        let mut hash = Sha256::new();
        let mut remaining = header.size_bytes;
        let mut buffer = [0_u8; 64 * 1024];
        while remaining != 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            let chunk = &mut buffer[..count];
            source.read_exact(chunk).map_err(incomplete)?;
            body.write_all(chunk)
                .map_err(|_| "artifact spool write failed")?;
            hash.update(chunk);
            remaining -= count as u64;
        }
        let mut trailer = [0_u8; SANDBOX_ARTIFACT_STREAM_DIGEST_BYTES];
        source.read_exact(&mut trailer).map_err(incomplete)?;
        let digest = hash.finalize();
        if digest[..] != trailer {
            return Err("artifact helper result integrity mismatch".into());
        }
        Ok(Self {
            metadata: SandboxArtifactMetadata {
                filename: header.filename,
                size_bytes: header.size_bytes,
                sha256: format!("sha256:{digest:x}"),
            },
            body,
        })
    }

    pub(crate) fn reader(&self) -> Result<impl Read + Send + 'static, String> {
        // Publication attempts are synchronous. Rewind this private file only
        // after the previous HTTP attempt has finished using its reader.
        let mut reader = self
            .body
            .try_clone()
            .map_err(|_| "artifact spool unavailable")?;
        reader.rewind().map_err(|_| "artifact spool unavailable")?;
        Ok(reader.take(self.metadata.size_bytes))
    }
}

impl DockerExecutionHostRunner {
    pub(crate) fn read_artifact(&self, path: &str) -> Result<PreparedArtifact, String> {
        let request = serde_json::to_vec(&SandboxArtifactRequest {
            path: path.to_string(),
            max_bytes: MAX_PUBLISHED_ARTIFACT_BYTES,
        })
        .map_err(|error| format!("encode artifact helper request failed: {error}"))?;
        self.with_helper_dispatch("read-artifact", || {
            let output =
                self.supervised_helper_output(TrustedHelperKind::ReadArtifact, &request, None)?;
            let artifact = output.read_stdout(PreparedArtifact::read)?;
            if path.rsplit('/').next() != Some(artifact.metadata.filename.as_str()) {
                return Err("artifact helper filename mismatch".into());
            }
            Ok(artifact)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(bytes: &[u8]) -> Vec<u8> {
        let header = serde_json::to_vec(&SandboxArtifactStreamHeader {
            schema: SANDBOX_ARTIFACT_STREAM_SCHEMA.into(),
            filename: "report.bin".into(),
            size_bytes: bytes.len() as u64,
        })
        .unwrap();
        let mut frame = (header.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&header);
        frame.extend_from_slice(bytes);
        frame.extend_from_slice(&Sha256::digest(bytes));
        frame
    }

    #[test]
    fn host_stream_verifies_an_artifact_once_and_keeps_rewindable_bytes() {
        let bytes = vec![0xff; 130_001];
        let frame = frame(&bytes);
        let artifact = PreparedArtifact::read(&mut frame.as_slice()).unwrap();
        assert_eq!(artifact.metadata.filename, "report.bin");
        assert_eq!(
            artifact.metadata.sha256,
            format!("sha256:{:x}", Sha256::digest(&bytes))
        );
        for _ in 0..2 {
            let mut actual = Vec::new();
            artifact.reader().unwrap().read_to_end(&mut actual).unwrap();
            assert_eq!(actual, bytes);
        }
    }

    #[test]
    fn host_stream_rejects_truncated_and_corrupt_artifacts() {
        let mut frame = frame(b"report body");
        assert!(PreparedArtifact::read(&mut &frame[..frame.len() - 1]).is_err());
        let content_start =
            frame.len() - SANDBOX_ARTIFACT_STREAM_DIGEST_BYTES - b"report body".len();
        frame[content_start] ^= 1;
        assert!(PreparedArtifact::read(&mut frame.as_slice()).is_err());
    }
}
