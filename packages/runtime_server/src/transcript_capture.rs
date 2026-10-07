//! Best-effort hosted text archive. Core events and tool outcomes are unchanged.
use crate::postgres_store::PostgresRuntimeStore;
use centaeris_core::session::{SequencedSessionRecord, SessionCommitReceipt, SessionRecordType};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[derive(Default)]
pub(crate) struct CapturePublisher(Mutex<PublishQueue>);

#[derive(Default)]
struct PublishQueue {
    pending: VecDeque<(CapturedOutput, usize)>,
    bytes: usize,
    running: bool,
}

impl PublishQueue {
    fn enqueue(&mut self, outputs: Vec<CapturedOutput>, budget: usize) -> bool {
        for output in outputs {
            let cost = output
                .data
                .len()
                .saturating_add(output.record.event.payload.to_string().len())
                .saturating_add(512);
            if self.bytes.saturating_add(cost) > budget {
                eprintln!("transcript_capture_unavailable: publisher budget exhausted");
                continue;
            }
            self.bytes += cost;
            self.pending.push_back((output, cost));
        }
        if self.running || self.pending.is_empty() {
            return false;
        }
        self.running = true;
        true
    }
}

/// Called only after the fenced event commit succeeds. No failure escapes into
/// the tool safe point. One publisher per Execution; queued bytes are bounded.
pub(crate) fn publish_committed(
    store: PostgresRuntimeStore,
    execution_id: String,
    publisher: Arc<CapturePublisher>,
    capture: &mut CaptureBuffer,
    receipt: &SessionCommitReceipt,
    budget: usize,
) {
    let records = receipt
        .records
        .iter()
        .map(|record| SequencedSessionRecord {
            sequence: record.sequence,
            event: record.event.clone(),
        })
        .collect::<Vec<_>>();
    publish(
        store,
        execution_id,
        publisher,
        capture.take(&records),
        budget,
    );
}

pub(crate) fn publish(
    store: PostgresRuntimeStore,
    execution_id: String,
    publisher: Arc<CapturePublisher>,
    outputs: Vec<CapturedOutput>,
    budget: usize,
) {
    let Ok(mut queue) = publisher.0.lock() else {
        return;
    };
    if !queue.enqueue(outputs, budget) {
        return;
    }
    drop(queue);
    struct Release(Option<Arc<CapturePublisher>>);
    impl Drop for Release {
        fn drop(&mut self) {
            if let Some(publisher) = &self.0 {
                if let Ok(mut queue) = publisher.0.lock() {
                    *queue = PublishQueue::default();
                }
            }
        }
    }
    let release = Release(Some(publisher.clone()));
    if let Err(error) = std::thread::Builder::new()
        .name("transcript-capture".into())
        .spawn(move || {
            let mut release = release;
            loop {
                let item = {
                    let Ok(mut queue) = publisher.0.lock() else {
                        return;
                    };
                    match queue.pending.pop_front() {
                        Some(item) => item,
                        None => {
                            queue.running = false;
                            // Normal drain must not clear a newly enqueued batch.
                            release.0 = None;
                            return;
                        }
                    }
                };
                let (output, cost) = item;
                let _deadline = crate::postgres_store::RequestDeadline::enter(Some(
                    std::time::Instant::now() + std::time::Duration::from_secs(5),
                ));
                if let Err(error) =
                    store.with_client(|client| persist(client, &execution_id, &output))
                {
                    eprintln!(
                        "transcript_capture_unavailable event={}: {error}",
                        output.record.event.event_id
                    );
                }
                if let Ok(mut queue) = publisher.0.lock() {
                    queue.bytes = queue.bytes.saturating_sub(cost);
                }
            }
        })
    {
        eprintln!("transcript_capture_unavailable: publisher spawn failed: {error}");
    }
}

fn persist(
    client: &mut postgres::Client,
    execution_id: &str,
    output: &CapturedOutput,
) -> Result<(), String> {
    let record = &output.record;
    let event = &record.event;
    if event.event_type != SessionRecordType::ToolResult
        || event
            .payload
            .get("outputComplete")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || event
            .payload
            .get("outputByteLength")
            .and_then(serde_json::Value::as_u64)
            != Some(output.data.len() as u64)
        || std::str::from_utf8(&output.data).is_err()
    {
        return Err("invalid captured text".into());
    }
    let run_id = event
        .agent_run_id
        .as_deref()
        .ok_or("capture AgentRun missing")?;
    let sequence = i32::try_from(record.sequence).map_err(|_| "capture sequence overflow")?;
    let length = i64::try_from(output.data.len()).map_err(|_| "capture length overflow")?;
    let sha = digest(&output.data);
    let mut tx = client.transaction().map_err(|e| e.to_string())?;
    // PostgreSQL 18 is the hosted database baseline. Bound the entire optional
    // transaction, including a slow chunk, rather than each statement alone.
    tx.batch_execute("SET LOCAL transaction_timeout='5s'; SET LOCAL statement_timeout='5s'")
        .map_err(|e| e.to_string())?;
    const OWNER: &str = "SELECT e.payload::text FROM app_core_session s JOIN app_core_sessionevent e ON e.session_id=s.id WHERE s.id=$1 AND s.status='active' AND s.\"purgedAt\" IS NULL AND e.\"eventId\"=$2 AND e.agent_run_id=$3 AND e.sequence=$4 AND e.payload->>'type'='tool_result' AND (SELECT x.payload #>> '{payload,executionId}' FROM app_core_sessionevent x WHERE x.session_id=s.id AND x.agent_run_id=e.agent_run_id AND x.sequence<e.sequence AND x.payload->>'type'='agent_run_execution_started' ORDER BY x.sequence DESC LIMIT 1)=$5";
    let valid = tx
        .query_opt(
            OWNER,
            &[
                &event.session_id,
                &event.event_id,
                &run_id,
                &sequence,
                &execution_id,
            ],
        )
        .map_err(|e| e.to_string())?;
    let owner = valid.ok_or("capture has no live committed owner")?;
    let mut canonical: serde_json::Value = serde_json::from_str(&owner.get::<_, String>(0))
        .map_err(|_| "capture owner payload is invalid")?;
    crate::postgres_store::decode_session_payload(&mut canonical)?;
    if canonical.get("payload") != Some(&event.payload) {
        return Err("capture has no live committed owner".into());
    }
    let inserted = tx.execute(
        "INSERT INTO app_core_transcriptoutputcapture(event_id,\"executionId\",sha256,\"byteLength\",\"chunkSize\",\"createdAt\",\"purgedAt\") VALUES($1,$2,$3,$4,65536,clock_timestamp(),NULL) ON CONFLICT(event_id) DO NOTHING",
        &[&event.event_id, &execution_id, &sha, &length],
    ).map_err(|e| e.to_string())?;
    if inserted == 0 {
        let same = tx.query_opt("SELECT event_id FROM app_core_transcriptoutputcapture WHERE event_id=$1 AND \"executionId\"=$2 AND sha256=$3 AND \"byteLength\"=$4 AND \"purgedAt\" IS NULL",
            &[&event.event_id, &execution_id, &sha, &length]).map_err(|e| e.to_string())?;
        return if same.is_some() {
            Ok(())
        } else {
            Err("capture identity conflict or purged".into())
        };
    }
    for (index, data) in output.data.chunks(65536).enumerate() {
        let index = i32::try_from(index).map_err(|_| "capture chunk index overflow")?;
        tx.execute("INSERT INTO app_core_transcriptoutputchunk(capture_id,index,data,sha256) VALUES($1,$2,$3,$4)",
            &[&event.event_id, &index, &data, &digest(data)]).map_err(|e| e.to_string())?;
    }
    // Lock only at publication: chunk I/O must not hold up the Session's next
    // Core commit. Deletion during the upload wins this final check and rolls
    // back metadata and chunks together.
    if tx
        .query_opt(
            &format!("{OWNER} FOR UPDATE OF s"),
            &[
                &event.session_id,
                &event.event_id,
                &run_id,
                &sequence,
                &execution_id,
            ],
        )
        .map_err(|e| e.to_string())?
        .is_none()
    {
        return Err("capture owner was deleted before publication".into());
    }
    tx.commit().map_err(|e| e.to_string())
}

#[derive(Default)]
pub(crate) struct CaptureBuffer {
    writes: HashMap<String, Vec<u8>>,
    seen: HashSet<String>,
    bytes: usize,
    disabled: bool,
}

pub(crate) struct CapturedOutput {
    pub record: SequencedSessionRecord,
    pub data: Vec<u8>,
}

impl CaptureBuffer {
    pub fn observe_write(
        &mut self,
        operation: &centaeris_core::execution::ExecutionFileSystemOperation,
        result: &centaeris_core::execution::ExecutionFileSystemOutput,
        budget: usize,
    ) {
        use centaeris_core::execution::{ExecutionFileSystemOperation, ExecutionFileSystemOutput};
        if let (
            ExecutionFileSystemOperation::WriteFile {
                content,
                create_only: true,
                ..
            },
            ExecutionFileSystemOutput::WriteFile(write),
        ) = (operation, result)
        {
            if write.created {
                self.observe(write.identity.display_path.clone(), content, budget);
            }
        }
    }
    pub fn observe(&mut self, path: String, bytes: &[u8], budget: usize) {
        if self.disabled {
            return;
        }
        if self.seen.contains(&path) {
            if let Some(previous) = self.writes.remove(&path) {
                self.bytes = self.bytes.saturating_sub(previous.len());
            }
            return;
        }
        // Keep identities after consumption: a reused path is ambiguous even if
        // the earlier output has already been published. Account for metadata.
        let text = std::str::from_utf8(bytes).is_ok();
        let cost = (if text { bytes.len() } else { 0 })
            .saturating_add(path.len().saturating_mul(2))
            .saturating_add(128);
        if self.bytes.saturating_add(cost) > budget {
            self.writes.clear();
            self.seen.clear();
            self.disabled = true;
            eprintln!("transcript_capture_unavailable: execution buffer budget exhausted");
            return;
        }
        self.bytes += cost;
        self.seen.insert(path.clone());
        if text {
            self.writes.insert(path, bytes.to_vec());
        }
    }

    pub fn take(&mut self, records: &[SequencedSessionRecord]) -> Vec<CapturedOutput> {
        records
            .iter()
            .filter_map(|record| {
                if record.event.event_type != SessionRecordType::ToolResult {
                    return None;
                }
                let p = &record.event.payload;
                let path = p.get("fullOutputPath")?.as_str()?;
                let bytes = self.writes.remove(path)?;
                self.bytes = self.bytes.saturating_sub(bytes.len());
                if !p.get("outputComplete")?.as_bool()? {
                    return None;
                }
                let start = usize::try_from(p.get("outputStartByte")?.as_u64()?).ok()?;
                let length = usize::try_from(p.get("outputByteLength")?.as_u64()?).ok()?;
                let data = bytes.get(start..start.checked_add(length)?)?;
                std::str::from_utf8(data).ok()?;
                Some(CapturedOutput {
                    record: record.clone(),
                    data: data.to_vec(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaeris_core::session::SessionLogRecord;
    use serde_json::json;

    include!("transcript_capture_boundary_tests.rs");

    #[test]
    #[ignore = "Django invokes this contract against its migrated disposable PostgreSQL database"]
    fn transcript_capture_django_contract() {
        let path = std::env::var("CENTAERIS_CAPTURE_TEST_FIXTURE").expect("fixture required");
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let record: SequencedSessionRecord =
            serde_json::from_value(value["record"].clone()).unwrap();
        let text = value["text"].as_str().unwrap();
        let prefix = value["prefix"].as_str().unwrap();
        let mut buffer = CaptureBuffer::default();
        let bytes = format!("{prefix}{text}footer");
        buffer.observe(
            record.event.payload["fullOutputPath"]
                .as_str()
                .unwrap()
                .into(),
            bytes.as_bytes(),
            bytes.len() * 2 + 1024,
        );
        let output = buffer
            .take(&[record])
            .pop()
            .expect("observed text must match committed range");
        let url = std::env::var("CENTAERIS_CAPTURE_TEST_DATABASE").expect("database required");
        let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        let db: String = client
            .query_one("SELECT current_database()", &[])
            .unwrap()
            .get(0);
        assert!(
            db.starts_with("test_"),
            "only disposable test databases are allowed"
        );
        let result = persist(&mut client, value["executionId"].as_str().unwrap(), &output);
        assert_eq!(
            result.is_ok(),
            value["succeeds"].as_bool().unwrap(),
            "{result:?}"
        );
        println!("transcript-capture-django-contract-ok");
    }

    pub(super) fn record(path: &str, start: usize, length: usize) -> SequencedSessionRecord {
        SequencedSessionRecord {
            sequence: 1,
            event: SessionLogRecord {
                schema_version: "session.event.v1".into(),
                event_version: 1,
                event_type: SessionRecordType::ToolResult,
                event_id: "event-capture".into(),
                session_id: "session-capture".into(),
                turn_id: Some("turn-capture".into()),
                agent_run_id: Some("run-capture".into()),
                created_at_ms: 1,
                payload: json!({"callId":"call-capture", "fullOutputPath":path,
                "outputStartByte":start,"outputByteLength":length,"outputComplete":true}),
            },
        }
    }

    #[test]
    fn transcript_capture_uses_successful_write_bytes_and_event_range_once() {
        let mut capture = CaptureBuffer::default();
        capture.observe("spill".into(), b"prefixAAAsuffix", 1000);
        let records = [record("spill", 6, 3)];
        let outputs = capture.take(&records);
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].data, b"AAA");
        assert!(capture.take(&records).is_empty());
    }

    #[test]
    fn transcript_capture_requires_boolean_completion_and_consumes_rejected_bytes() {
        for completion in [
            Some(json!(true)),
            Some(json!(false)),
            Some(json!(null)),
            Some(json!(1)),
            Some(json!("true")),
            None,
        ] {
            let mut capture = CaptureBuffer::default();
            capture.observe("spill".into(), b"AAA", 1000);
            let mut item = record("spill", 0, 3);
            if let Some(value) = &completion {
                item.event.payload["outputComplete"] = value.clone();
            } else {
                item.event
                    .payload
                    .as_object_mut()
                    .unwrap()
                    .remove("outputComplete");
            }
            let outputs = capture.take(&[item]);
            if completion == Some(json!(true)) {
                assert_eq!(outputs.len(), 1);
                assert_eq!(outputs[0].data, b"AAA");
            } else {
                assert!(outputs.is_empty());
            }
            assert!(capture.take(&[record("spill", 0, 3)]).is_empty());
        }
    }

    #[test]
    fn transcript_capture_rejects_ambiguous_paths_binary_and_invalid_ranges() {
        let mut capture = CaptureBuffer::default();
        capture.observe("spill".into(), b"AAA", 1000);
        capture.observe("spill".into(), b"BBB", 1000);
        capture.observe("binary".into(), &[0xff], 1000);
        assert!(capture
            .take(&[record("spill", 0, 3), record("binary", 0, 1)])
            .is_empty());
        capture.observe("bad-range".into(), "世".as_bytes(), 1000);
        assert!(capture.take(&[record("bad-range", 1, 2)]).is_empty());
    }

    #[test]
    fn transcript_capture_budget_exhaustion_disables_without_rebinding() {
        let mut capture = CaptureBuffer::default();
        capture.observe("spill".into(), b"AAA", 2);
        capture.observe("spill".into(), b"B", 1000);
        assert!(capture.take(&[record("spill", 0, 1)]).is_empty());
    }

    #[test]
    fn transcript_capture_publisher_queues_bursts_within_budget_and_preserves_order() {
        let output = |text: &str| CapturedOutput {
            record: record("spill", 0, text.len()),
            data: text.as_bytes().to_vec(),
        };
        let mut queue = PublishQueue::default();
        assert!(queue.enqueue(vec![output("first")], 10000));
        assert!(!queue.enqueue(vec![output("second")], 10000));
        let bytes = queue.bytes;
        assert!(!queue.enqueue(vec![output("rejected")], bytes));
        assert_eq!(queue.bytes, bytes);
        assert_eq!(queue.pending.pop_front().unwrap().0.data, b"first");
        assert_eq!(queue.pending.pop_front().unwrap().0.data, b"second");
        assert!(queue.pending.is_empty());
    }

    struct TextHost {
        capture: std::sync::Mutex<CaptureBuffer>,
        budget: usize,
    }
    impl centaeris_core::execution::ExecutionHostRunner for TextHost {
        fn status(
            &self,
            _: &centaeris_core::execution::ExecutionPolicy,
        ) -> Result<
            centaeris_core::execution::ExecutionHostStatus,
            centaeris_core::execution::ExecutionError,
        > {
            Ok(centaeris_core::execution::ExecutionHostStatus::remote(
                true,
                centaeris_core::execution::ExecutionHostHealth::Ready,
                None,
            ))
        }
        fn run_file_system_operation(
            &self,
            request: centaeris_core::execution::ExecutionFileSystemRequest,
        ) -> Result<
            centaeris_core::execution::ExecutionFileSystemOutput,
            centaeris_core::execution::ExecutionFileSystemError,
        > {
            use centaeris_core::execution::*;
            let identity = ExecutionFileIdentity {
                key: request.model_path.clone(),
                display_path: request.model_path,
            };
            let result = match &request.operation {
                ExecutionFileSystemOperation::ReadFile { .. } => {
                    let bytes = format!("{}\n", "original界".repeat(10))
                        .repeat(1000)
                        .into_bytes();
                    ExecutionFileSystemOutput::ReadFile(ExecutionFileReadOutput {
                        identity,
                        file_hash: digest(&bytes),
                        bytes,
                    })
                }
                ExecutionFileSystemOperation::WriteFile {
                    create_only: true, ..
                } => ExecutionFileSystemOutput::WriteFile(ExecutionFileWriteOutput {
                    identity,
                    created: true,
                }),
                _ => panic!("unexpected operation"),
            };
            self.capture
                .lock()
                .unwrap()
                .observe_write(&request.operation, &result, self.budget);
            Ok(result)
        }
        fn run_host_command(
            &self,
            _: Option<&str>,
            _: centaeris_core::execution::ExecutionCommandRequest,
            _: Option<&centaeris_core::execution::ExecutionCancellationProbe>,
        ) -> Result<
            centaeris_core::execution::ExecutionHostCommandOutput,
            centaeris_core::execution::ExecutionError,
        > {
            panic!("read must not invoke bash")
        }
    }

    #[tokio::test]
    async fn transcript_capture_observes_real_core_spill_without_changing_tool_outcome() {
        use centaeris_core::execution::*;
        use centaeris_core::extension::skills::SkillCatalogLoadConfig;
        use centaeris_core::tool::layer::{ToolInvocationRequest, ToolLayer};
        for budget in [1, 1_000_000] {
            let host = Arc::new(TextHost {
                capture: Default::default(),
                budget,
            });
            let workspace = std::env::temp_dir();
            let binding = Arc::new(
                ExecutionHostBinding::new(
                    ExecutionHostMode::Remote,
                    host.clone(),
                    workspace.clone(),
                    ExecutionPolicy::workspace_write_no_network(workspace),
                )
                .unwrap(),
            );
            let layer = ToolLayer::try_new_with_skill_catalog_config_and_execution_host_binding(
                SkillCatalogLoadConfig::default(),
                binding,
            )
            .unwrap()
            .with_session_id("capture-test");
            let result = layer
                .execute_async(ToolInvocationRequest {
                    tool_call_id: "call-capture".into(),
                    tool_name: "read".into(),
                    args_json: json!({"path":"large.txt"}).to_string(),
                })
                .await;
            assert_eq!(result.status, "ok", "{result:?}");
            let metadata = &result.details["toolResultCapture"];
            assert_eq!(metadata["outputComplete"], true);
            let path = metadata["fullOutputPath"]
                .as_str()
                .expect("real Core spill required");
            let record = record(
                path,
                metadata["outputStartByte"].as_u64().unwrap() as usize,
                metadata["outputByteLength"].as_u64().unwrap() as usize,
            );
            let outputs = host.capture.lock().unwrap().take(&[record]);
            if budget == 1 {
                assert!(outputs.is_empty());
            } else {
                assert_eq!(outputs.len(), 1);
                let text = std::str::from_utf8(&outputs[0].data).unwrap();
                assert!(text.contains("original界"));
                assert!(text.len() > 50 * 1024);
                assert!(result.content.len() < text.len());
            }
        }
    }
}
