//! Bounded Docker archive transport; callers supply an already pinned owned ID.
use super::*;
use axum::body::Bytes;
use futures::TryStreamExt;

pub(crate) fn upload(id: &str, path: &str, bytes: Bytes) -> Result<(), String> {
    let engine = shared()?;
    engine
        .run(MUTATION_TIMEOUT, async {
            let options = UploadToContainerOptionsBuilder::default()
                .path(path)
                .no_overwrite_dir_non_dir("true")
                .build();
            engine
                .docker
                .upload_to_container(id, Some(options), bollard::body_full(bytes))
                .await
                .map_err(|_| "benchmark_upload_outcome_unknown".to_string())
        })
        .map_err(|_| "benchmark_upload_outcome_unknown".into())
}

fn download_error(error: Error) -> String {
    match error {
        Error::DockerResponseServerError {
            status_code: 404,
            message,
        } => if message.contains("No such container") {
            "benchmark_transfer_target_unavailable"
        } else {
            "benchmark_download_path_missing"
        }
        .into(),
        _ => "benchmark_download_unavailable".into(),
    }
}

async fn collect_download(
    stream: impl futures::Stream<Item = Result<Bytes, Error>>,
    limit: usize,
) -> Result<Vec<u8>, String> {
    futures::pin_mut!(stream);
    let mut output = Vec::with_capacity(limit);
    while let Some(chunk) = stream.try_next().await.map_err(download_error)? {
        if output.len().saturating_add(chunk.len()) > limit {
            return Err("benchmark_archive_limit_exceeded".into());
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

pub(crate) fn download(id: &str, path: &str, limit: usize) -> Result<Vec<u8>, String> {
    let engine = shared()?;
    engine
        .run(MUTATION_TIMEOUT, async {
            let options = DownloadFromContainerOptionsBuilder::default()
                .path(path)
                .build();
            let stream = engine.docker.download_from_container(id, Some(options));
            collect_download(stream, limit).await
        })
        .map_err(|error| match error.as_str() {
            "benchmark_download_path_missing"
            | "benchmark_archive_limit_exceeded"
            | "benchmark_transfer_target_unavailable" => error,
            _ => "benchmark_download_unavailable".into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_stream_enforces_the_cap_and_classifies_docker_errors_without_private_details() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let chunks = || {
                futures::stream::iter([
                    Ok(Bytes::from_static(b"abc")),
                    Ok(Bytes::from_static(b"def")),
                ])
            };
            assert_eq!(collect_download(chunks(), 6).await.unwrap(), b"abcdef");
            assert_eq!(
                collect_download(chunks(), 5).await.unwrap_err(),
                "benchmark_archive_limit_exceeded"
            );
            for (status_code, message, code) in [
                (
                    404,
                    "Could not find the file /private/path in container abc",
                    "benchmark_download_path_missing",
                ),
                (
                    404,
                    "No such container: private-id",
                    "benchmark_transfer_target_unavailable",
                ),
                (
                    500,
                    "private Docker error",
                    "benchmark_download_unavailable",
                ),
            ] {
                let stream = futures::stream::iter([Err(Error::DockerResponseServerError {
                    status_code,
                    message: message.into(),
                })]);
                assert_eq!(collect_download(stream, 6).await.unwrap_err(), code);
            }
        });
    }
}
