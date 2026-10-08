use std::thread::JoinHandle;

pub(crate) struct PendingRecoveryUpload<T> {
    worker: Option<JoinHandle<Result<T, String>>>,
}

impl<T: Send + 'static> PendingRecoveryUpload<T> {
    pub(crate) fn start(upload: impl FnOnce() -> Result<T, String> + Send + 'static) -> Self {
        Self {
            worker: Some(std::thread::spawn(upload)),
        }
    }

    pub(crate) fn take(&mut self, wait: bool) -> Option<Result<T, String>> {
        if !wait && !self.worker.as_ref()?.is_finished() {
            return None;
        }
        Some(
            self.worker
                .take()?
                .join()
                .unwrap_or_else(|_| Err("recovery workspace upload worker panicked".into())),
        )
    }
}

impl<T> Drop for PendingRecoveryUpload<T> {
    fn drop(&mut self) {
        // The HTTP upload has its own bounded timeout. Never detach a worker
        // across terminal save, cancellation, or a failed Session commit.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn model_can_proceed_while_upload_waits_but_tool_fence_waits_for_object() {
        let (release, blocked) = mpsc::channel();
        let (entered, ready) = mpsc::channel();
        let mut upload = PendingRecoveryUpload::start(move || {
            entered.send(()).unwrap();
            blocked.recv().unwrap();
            Ok("verified-object")
        });
        ready.recv().unwrap();
        assert!(
            upload.take(false).is_none(),
            "model request must not wait on upload"
        );
        release.send(()).unwrap();
        assert_eq!(upload.take(true).unwrap().unwrap(), "verified-object");
        assert!(upload.take(true).is_none(), "publish once");
    }

    #[test]
    fn failed_upload_never_becomes_a_publishable_checkpoint() {
        let mut upload = PendingRecoveryUpload::<()>::start(|| Err("stage rejected".into()));
        assert_eq!(upload.take(true).unwrap().unwrap_err(), "stage rejected");
        assert!(upload.take(true).is_none());
    }

    #[test]
    fn terminal_exit_drains_worker_even_without_a_completion_safe_point() {
        let completed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_completed = completed.clone();
        let (release, wait) = mpsc::channel();
        let upload = PendingRecoveryUpload::start(move || {
            wait.recv().unwrap();
            worker_completed.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        });
        release.send(()).unwrap();
        drop(upload);
        assert!(
            completed.load(std::sync::atomic::Ordering::SeqCst),
            "terminal save must not race an orphan workspace upload"
        );
    }
}
