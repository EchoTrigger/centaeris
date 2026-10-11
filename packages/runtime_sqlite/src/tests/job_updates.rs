use super::*;

struct Database {
    directory: PathBuf,
    store: SqliteRuntimeStore,
}

impl Database {
    fn new() -> Self {
        let mut nonce = 0;
        let directory = loop {
            let path = temp_db_path(&format!("job_updates_{nonce}"));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => nonce += 1,
                Err(error) => panic!("create test directory: {error}"),
            }
        };
        Self {
            store: SqliteRuntimeStore::new(directory.join("store.db")).unwrap(),
            directory,
        }
    }

    fn terminal_outbox(&self) -> i64 {
        Connection::open(self.store.db_path())
            .unwrap()
            .query_row(
                "SELECT count(*) FROM runtime_job_outbox WHERE event_type=?1",
                params![centaeris_core::session::reliability::RUNTIME_JOB_TERMINAL_EVENT],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn fail_outbox(&self) {
        Connection::open(self.store.db_path()).unwrap().execute_batch(
            "CREATE TRIGGER reject_test_outbox BEFORE INSERT ON runtime_job_outbox BEGIN SELECT RAISE(FAIL, 'injected outbox failure'); END;"
        ).unwrap();
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn completion(owner: &str, at: i64) -> CompleteRuntimeJobRequest {
    CompleteRuntimeJobRequest {
        job_id: "job".into(),
        lease_owner: owner.into(),
        output_refs: vec!["object".into()],
        completed_at_ms: at,
    }
}

fn combined_completion(
    req: CompleteRuntimeJobRequest,
) -> UpsertExternalContextLinkAndCompleteJobRequest {
    UpsertExternalContextLinkAndCompleteJobRequest {
        object: Some(external_context_object("object")),
        link: Some(ExternalContextObjectLink {
            session_id: "chat-sqlite-tx".into(),
            object_id: "object".into(),
            turn_id: Some("turn".into()),
            tool_call_id: Some("call".into()),
            source_provider_id: "test-provider".into(),
            source_tool_name: "test-tool".into(),
            linked_at_ms: 999,
        }),
        complete_job: req,
    }
}

fn combined_failure(req: FailRuntimeJobRequest) -> CreateDeadLetterAndFailJobRequest {
    CreateDeadLetterAndFailJobRequest {
        dead_letter: CreateDeadLetterRequest {
            dead_letter: dead_letter_record("dead-letter", "job"),
        },
        fail_job: req,
    }
}

#[test]
fn completion_entry_points_preserve_lease_conditions_and_results() {
    for (status, owner, at, accepted) in [
        (RuntimeJobStatus::Leased, "owner", 999, true),
        (RuntimeJobStatus::Running, "owner", 999, true),
        (RuntimeJobStatus::Running, "other", 999, false),
        (RuntimeJobStatus::Running, "owner", 1000, false),
        (RuntimeJobStatus::Succeeded, "owner", 999, false),
    ] {
        let ordinary = Database::new();
        let combined = Database::new();
        let before = runtime_job("job", status, Some("owner"));
        for db in [&ordinary, &combined] {
            db.store
                .schedule_runtime_job(
                    centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                        job: before.clone(),
                    },
                )
                .unwrap();
        }
        let first = ordinary.store.complete_runtime_job(completion(owner, at));
        let second = combined
            .store
            .upsert_external_context_link_and_complete_job(combined_completion(completion(
                owner, at,
            )));
        assert_eq!(first, second);
        assert_eq!(first.is_ok(), accepted);
        assert_eq!(
            ordinary.store.get_runtime_job("job").unwrap(),
            combined.store.get_runtime_job("job").unwrap()
        );
        assert_eq!(ordinary.terminal_outbox(), i64::from(accepted));
        assert_eq!(combined.terminal_outbox(), ordinary.terminal_outbox());
        assert_eq!(
            combined
                .store
                .load_external_context_object("object")
                .unwrap()
                .is_some(),
            accepted
        );
        if !accepted {
            assert_eq!(ordinary.store.get_runtime_job("job").unwrap(), Some(before));
        }
    }
}

#[test]
fn failure_entry_points_preserve_retry_and_terminal_outbox_rules() {
    for (status, owner, at, disposition, next, accepted) in [
        (
            RuntimeJobStatus::Leased,
            "owner",
            999,
            RuntimeJobFailureDisposition::Failed,
            None,
            true,
        ),
        (
            RuntimeJobStatus::Running,
            "owner",
            999,
            RuntimeJobFailureDisposition::DeadLettered,
            None,
            true,
        ),
        (
            RuntimeJobStatus::Running,
            "owner",
            999,
            RuntimeJobFailureDisposition::RetryScheduled,
            Some(2000),
            true,
        ),
        (
            RuntimeJobStatus::Running,
            "owner",
            999,
            RuntimeJobFailureDisposition::RetryScheduled,
            None,
            false,
        ),
        (
            RuntimeJobStatus::Running,
            "other",
            999,
            RuntimeJobFailureDisposition::Failed,
            None,
            false,
        ),
        (
            RuntimeJobStatus::Running,
            "owner",
            1000,
            RuntimeJobFailureDisposition::Failed,
            None,
            false,
        ),
        (
            RuntimeJobStatus::Failed,
            "owner",
            999,
            RuntimeJobFailureDisposition::Failed,
            None,
            false,
        ),
    ] {
        let ordinary = Database::new();
        let combined = Database::new();
        let before = runtime_job("job", status, Some("owner"));
        for db in [&ordinary, &combined] {
            db.store
                .schedule_runtime_job(
                    centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                        job: before.clone(),
                    },
                )
                .unwrap();
        }
        let req = FailRuntimeJobRequest {
            job_id: "job".into(),
            lease_owner: owner.into(),
            failed_at_ms: at,
            disposition: disposition.clone(),
            next_run_at_ms: next,
            last_error: "failure".into(),
        };
        let first = ordinary.store.fail_runtime_job(req.clone());
        let second = combined
            .store
            .create_dead_letter_and_fail_job(combined_failure(req))
            .map(|_| ());
        assert_eq!(first, second);
        assert_eq!(first.is_ok(), accepted);
        let actual = ordinary.store.get_runtime_job("job").unwrap().unwrap();
        assert_eq!(
            Some(actual.clone()),
            combined.store.get_runtime_job("job").unwrap()
        );
        assert_eq!(actual.retry_count, before.retry_count + u32::from(accepted));
        assert_eq!(
            ordinary.terminal_outbox(),
            i64::from(accepted && disposition != RuntimeJobFailureDisposition::RetryScheduled)
        );
        assert_eq!(combined.terminal_outbox(), ordinary.terminal_outbox());
        assert_eq!(
            combined
                .store
                .get_dead_letter("dead-letter")
                .unwrap()
                .is_some(),
            accepted
        );
        if !accepted {
            assert_eq!(actual, before);
        }
    }
}

#[test]
fn outbox_failure_rolls_back_job_and_all_associated_records() {
    for combined in [false, true] {
        for failure in [false, true] {
            let db = Database::new();
            let before = runtime_job("job", RuntimeJobStatus::Running, Some("owner"));
            db.store
                .schedule_runtime_job(
                    centaeris_core::session::reliability::ScheduleRuntimeJobRequest {
                        job: before.clone(),
                    },
                )
                .unwrap();
            db.fail_outbox();
            let result = if failure {
                let req = FailRuntimeJobRequest {
                    job_id: "job".into(),
                    lease_owner: "owner".into(),
                    failed_at_ms: 999,
                    disposition: RuntimeJobFailureDisposition::Failed,
                    next_run_at_ms: None,
                    last_error: "failure".into(),
                };
                if combined {
                    db.store
                        .create_dead_letter_and_fail_job(combined_failure(req))
                        .map(|_| ())
                } else {
                    db.store.fail_runtime_job(req)
                }
            } else if combined {
                db.store
                    .upsert_external_context_link_and_complete_job(combined_completion(completion(
                        "owner", 999,
                    )))
            } else {
                db.store.complete_runtime_job(completion("owner", 999))
            };
            assert!(result.unwrap_err().contains("injected outbox failure"));
            assert_eq!(db.store.get_runtime_job("job").unwrap(), Some(before));
            assert_eq!(db.terminal_outbox(), 0);
            assert!(db
                .store
                .load_external_context_object("object")
                .unwrap()
                .is_none());
            assert!(db.store.get_dead_letter("dead-letter").unwrap().is_none());
            let links: i64 = Connection::open(db.store.db_path())
                .unwrap()
                .query_row("SELECT count(*) FROM external_context_links", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(links, 0);
        }
    }
}
