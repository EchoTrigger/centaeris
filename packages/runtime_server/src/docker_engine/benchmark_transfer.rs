//! Bounded Docker archive transport; callers supply an already pinned owned ID.
use super::*;
use futures::TryStreamExt;

pub(crate) fn upload(id: &str, path: &str, bytes: Vec<u8>) -> Result<(), String> {
    let engine = shared()?;
    engine.run(MUTATION_TIMEOUT, async {
        let options = UploadToContainerOptionsBuilder::default()
            .path(path)
            .no_overwrite_dir_non_dir("true")
            .build();
        engine
            .docker
            .upload_to_container(id, Some(options), bollard::body_full(bytes.into()))
            .await
            .map_err(|_| "benchmark_upload_outcome_unknown".to_string())
    })
}

pub(crate) fn download(id: &str, path: &str, limit: usize) -> Result<Vec<u8>, String> {
    let engine = shared()?;
    engine.run(MUTATION_TIMEOUT, async {
        let options = DownloadFromContainerOptionsBuilder::default()
            .path(path)
            .build();
        let stream = engine.docker.download_from_container(id, Some(options));
        futures::pin_mut!(stream);
        let mut output = Vec::new();
        while let Some(chunk) = stream
            .try_next()
            .await
            .map_err(|_| "benchmark_download_unavailable")?
        {
            if output.len().saturating_add(chunk.len()) > limit {
                return Err("benchmark_archive_limit_exceeded".into());
            }
            output.extend_from_slice(&chunk);
        }
        Ok(output)
    })
}
