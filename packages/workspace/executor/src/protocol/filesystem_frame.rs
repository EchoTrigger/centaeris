//! Bounded file bytes travel separately from JSON metadata and signed requests.
use super::SandboxFileSystemResult;
use centaeris_core::execution::{
    ExecutionFileSystemOperation, ExecutionFileSystemOutput, ExecutionFileSystemRequest,
    MAX_EXECUTION_INPUT_BYTES, MAX_PUBLISHED_ARTIFACT_BYTES,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};

pub const FILESYSTEM_FRAME_HEADER_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSystemFrameMetadata {
    pub request: ExecutionFileSystemRequest,
    pub content_length: u64,
    pub content_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSystemResponseFrameMetadata {
    pub result: SandboxFileSystemResult,
    pub content_length: u64,
    pub content_digest: String,
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|v| {
        v.len() == 64
            && v.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

impl FileSystemFrameMetadata {
    pub fn from_request(mut request: ExecutionFileSystemRequest) -> (Self, Vec<u8>) {
        let bytes = match &mut request.operation {
            ExecutionFileSystemOperation::WriteFile { content, .. } => std::mem::take(content),
            _ => Vec::new(),
        };
        (
            Self {
                request,
                content_length: bytes.len() as u64,
                content_digest: digest(&bytes),
            },
            bytes,
        )
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.content_length > MAX_EXECUTION_INPUT_BYTES || !valid_digest(&self.content_digest) {
            return Err("filesystem_frame_invalid".into());
        }
        match &self.request.operation {
            ExecutionFileSystemOperation::WriteFile { content, .. } if content.is_empty() => Ok(()),
            ExecutionFileSystemOperation::WriteFile { .. } => {
                Err("filesystem_frame_invalid".into())
            }
            _ if self.content_length == 0 && self.content_digest == digest(&[]) => Ok(()),
            _ => Err("filesystem_frame_invalid".into()),
        }
    }
    pub fn into_request(mut self, bytes: Vec<u8>) -> Result<ExecutionFileSystemRequest, String> {
        self.validate()?;
        check_content(&bytes, self.content_length, &self.content_digest)?;
        if let ExecutionFileSystemOperation::WriteFile { content, .. } = &mut self.request.operation
        {
            *content = bytes;
        }
        Ok(self.request)
    }
}

impl FileSystemResponseFrameMetadata {
    pub fn from_result(mut result: SandboxFileSystemResult) -> Result<(Self, Vec<u8>), String> {
        if result.output.is_some() == result.error.is_some() {
            return Err("filesystem_result_invalid".into());
        }
        let bytes = match &mut result.output {
            Some(ExecutionFileSystemOutput::ReadFile(read)) => std::mem::take(&mut read.bytes),
            _ => Vec::new(),
        };
        if bytes.len() as u64 > MAX_PUBLISHED_ARTIFACT_BYTES {
            return Err("filesystem_frame_invalid".into());
        }
        Ok((
            Self {
                result,
                content_length: bytes.len() as u64,
                content_digest: digest(&bytes),
            },
            bytes,
        ))
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.result.output.is_some() == self.result.error.is_some()
            || self.content_length > MAX_PUBLISHED_ARTIFACT_BYTES
            || !valid_digest(&self.content_digest)
        {
            return Err("filesystem_result_invalid".into());
        }
        match &self.result.output {
            Some(ExecutionFileSystemOutput::ReadFile(read)) if read.bytes.is_empty() => Ok(()),
            Some(ExecutionFileSystemOutput::ReadFile(_)) => Err("filesystem_result_invalid".into()),
            _ if self.content_length == 0 && self.content_digest == digest(&[]) => Ok(()),
            _ => Err("filesystem_result_invalid".into()),
        }
    }
    pub fn into_result(mut self, bytes: Vec<u8>) -> Result<SandboxFileSystemResult, String> {
        self.validate()?;
        check_content(&bytes, self.content_length, &self.content_digest)?;
        if let Some(ExecutionFileSystemOutput::ReadFile(read)) = &mut self.result.output {
            read.bytes = bytes;
        }
        Ok(self.result)
    }
}

fn check_content(bytes: &[u8], length: u64, expected: &str) -> Result<(), String> {
    if bytes.len() as u64 != length || digest(bytes) != expected {
        Err("filesystem_frame_content_mismatch".into())
    } else {
        Ok(())
    }
}

pub fn write_filesystem_request(
    writer: &mut impl Write,
    request: ExecutionFileSystemRequest,
) -> Result<(), String> {
    let (header, bytes) = FileSystemFrameMetadata::from_request(request);
    header.validate()?;
    write_frame(writer, &header, &bytes)
}
pub fn read_filesystem_request(
    reader: &mut impl Read,
) -> Result<ExecutionFileSystemRequest, String> {
    let header: FileSystemFrameMetadata = read_header(reader)?;
    header.validate()?;
    let bytes = read_content(reader, header.content_length)?;
    header.into_request(bytes)
}
pub fn write_filesystem_result(
    writer: &mut impl Write,
    result: SandboxFileSystemResult,
) -> Result<(), String> {
    let (header, bytes) = FileSystemResponseFrameMetadata::from_result(result)?;
    write_frame(writer, &header, &bytes)
}
pub fn read_filesystem_result(reader: &mut impl Read) -> Result<SandboxFileSystemResult, String> {
    let header: FileSystemResponseFrameMetadata = read_header(reader)?;
    header.validate()?;
    let bytes = read_content(reader, header.content_length)?;
    header.into_result(bytes)
}
fn write_frame(
    writer: &mut impl Write,
    header: &impl Serialize,
    bytes: &[u8],
) -> Result<(), String> {
    let header = serde_json::to_vec(header).map_err(|_| "filesystem_frame_invalid")?;
    if header.is_empty() || header.len() > FILESYSTEM_FRAME_HEADER_LIMIT {
        return Err("filesystem_frame_header_too_large".into());
    }
    writer
        .write_all(&(header.len() as u32).to_be_bytes())
        .and_then(|()| writer.write_all(&header))
        .and_then(|()| writer.write_all(bytes))
        .map_err(|_| "filesystem_frame_unavailable".into())
}
fn read_header<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T, String> {
    let mut length = [0u8; 4];
    reader
        .read_exact(&mut length)
        .map_err(|_| "filesystem_frame_incomplete")?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > FILESYSTEM_FRAME_HEADER_LIMIT {
        return Err("filesystem_frame_header_too_large".into());
    }
    let mut header = vec![0; length];
    reader
        .read_exact(&mut header)
        .map_err(|_| "filesystem_frame_incomplete")?;
    serde_json::from_slice(&header).map_err(|_| "filesystem_frame_invalid".into())
}
fn read_content(reader: &mut impl Read, length: u64) -> Result<Vec<u8>, String> {
    let length = usize::try_from(length).map_err(|_| "filesystem_frame_invalid")?;
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| "filesystem_frame_incomplete")?;
    let mut extra = [0u8; 1];
    if reader
        .read(&mut extra)
        .map_err(|_| "filesystem_frame_incomplete")?
        != 0
    {
        return Err("filesystem_frame_trailing_bytes".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::execution::*;
    fn request(bytes: Vec<u8>) -> ExecutionFileSystemRequest {
        ExecutionFileSystemRequest {
            operation_id: None,
            cwd: "/project".into(),
            policy: ExecutionPolicy::workspace_write_no_network("/project"),
            model_path: "verifier.bin".into(),
            operation: ExecutionFileSystemOperation::WriteFile {
                content: bytes,
                observed_file_hash: None,
                create_only: true,
            },
        }
    }
    #[test]
    fn filesystem_binary_upload_and_download_keep_large_content_out_of_json() {
        let bytes: Vec<u8> = (0..11_506_423).map(|n| (n % 251) as u8).collect();
        let expected = digest(&bytes);
        let mut frame = Vec::new();
        write_filesystem_request(&mut frame, request(bytes.clone())).unwrap();
        let header_length = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        assert!(header_length < 4096);
        assert_eq!(&frame[header_length + 4..], bytes);
        let decoded = read_filesystem_request(&mut frame.as_slice()).unwrap();
        let ExecutionFileSystemOperation::WriteFile { content, .. } = decoded.operation else {
            panic!("write expected")
        };
        assert_eq!(digest(&content), expected);
        let result = SandboxFileSystemResult::from(Ok(ExecutionFileSystemOutput::ReadFile(
            ExecutionFileReadOutput {
                identity: ExecutionFileIdentity {
                    key: "file".into(),
                    display_path: "verifier.bin".into(),
                },
                bytes: content,
                file_hash: expected.clone(),
            },
        )));
        let mut frame = Vec::new();
        write_filesystem_result(&mut frame, result).unwrap();
        assert!(u32::from_be_bytes(frame[..4].try_into().unwrap()) < 4096);
        let ExecutionFileSystemOutput::ReadFile(read) =
            read_filesystem_result(&mut frame.as_slice())
                .unwrap()
                .into_result()
                .unwrap()
        else {
            panic!("read expected")
        };
        assert_eq!(read.bytes, bytes);
        assert_eq!(read.file_hash, expected);
    }
    #[test]
    fn filesystem_frames_reject_corrupt_truncated_trailing_and_oversized_input() {
        let mut frame = Vec::new();
        write_filesystem_request(&mut frame, request(b"safe bytes".to_vec())).unwrap();
        let mut changed = frame.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(read_filesystem_request(&mut changed.as_slice()).is_err());
        assert!(read_filesystem_request(&mut &frame[..frame.len() - 1]).is_err());
        frame.push(0);
        assert!(read_filesystem_request(&mut frame.as_slice()).is_err());
        assert!(read_filesystem_request(&mut u32::MAX.to_be_bytes().as_slice()).is_err());
        let (mut metadata, _) = FileSystemFrameMetadata::from_request(request(vec![]));
        metadata.content_length = MAX_EXECUTION_INPUT_BYTES + 1;
        assert!(metadata.validate().is_err());
    }
}
