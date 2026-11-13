use super::*;
use crate::agent_run_authorization::WorkspaceAgentRunAuthorization;
use crate::environment_authorization::{ControllerBinding, EnvironmentBinding};
use crate::postgres_store::session_deletion::SessionPurgeRequest;
use centaeris_core::runtime::subagent::{build_subagent_run_job, SubagentRunJobRequest};
use sha2::{Digest, Sha256};

fn packet(child: &str) -> serde_json::Value {
    let contract = centaeris_core::tool::list_tool_contracts()
        .into_iter()
        .find(|contract| contract.name == "read")
        .unwrap();
    serde_json::json!({
        "run_context":{"schema":"agent_run_context_v1","sessionId":"sess_1","branchId":child,"turnId":format!("turn-{child}"),"agentRunId":format!("run-{child}"),"agentRef":{"agentId":child,"agentRunId":format!("run-{child}")},"parentAgentRef":{"agentId":"main","agentRunId":"run-purge"},"parentTurnId":"parent-turn","depth":1,"cwd":std::env::current_dir().unwrap().display().to_string(),"cancellationScopeId":format!("scope-{child}"),"parentCancellationScopeId":"parent-scope","cancelOnParentCancel":true,"createdAtMs":1},
        "task_brief":{"task_id":null,"objective":"Child task","success_criteria":[],"constraints":[],"output_hint":null},
        "hot_view":{"summary":"","recent_message_ids":[],"state_kv":{}},"object_refs":[],
        "allowedTools":["read"],"delegatedToolContracts":[{"name":"read","providerId":contract.provider_id,"contractDigest":contract.contract_digest().unwrap(),"concurrencySafe":contract.concurrency_safe}],"writablePathPrefixes":[],
        "output_contract":{"response_mode":"summary","expected_sections":[],"require_artifact_refs":false,"max_summary_chars":null},"parent_checkpoint_id":null,"context_mode":"Borrow"
    })
}

fn object(store: &PostgresRuntimeStore, id: &str, content: &str) {
    store
        .upsert_external_context_object(ExternalContextObject {
            schema_version: "external_context.v1".into(),
            object_id: id.into(),
            object_kind: "subagent_result".into(),
            source_provider_id: "centaeris.core".into(),
            source_tool_name: "agent".into(),
            title: "Child result".into(),
            content: content.into(),
            metadata: serde_json::json!({}),
            updated_at_ms: 1,
        })
        .unwrap();
}

fn body_row(db: &mut Client, id: &str, kind: &str, payload: serde_json::Value) {
    let wire = serde_json::json!({"schemaVersion":"session.event.v1","eventVersion":1,
        "eventId":id,"type":kind,"sequence":1,"sessionId":"sess_1","agentRunId":"run-purge",
        "turnId":"parent-turn","createdAtMs":1,"payload":payload});
    db.execute("INSERT INTO public.app_core_sessionevent(workspace_id,session_id,agent_run_id,session_level,payload,\"eventId\",projects_to_agent_run_stream) VALUES('ws_1','sess_1','run-purge',false,$2::text::jsonb,$1,true)", &[&id,&wire.to_string()]).unwrap();
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_session_purge_erases_descendants_atomically_and_preserves_receipts() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let (_url, store, mut db) = retained_instance_tests::setup();
    db.batch_execute(r#"
        UPDATE public.app_core_session SET owner_id='1';
        UPDATE public.app_core_workspacemembership SET user_id='1';
        ALTER TABLE public.app_core_session ADD COLUMN status text DEFAULT 'active',ADD COLUMN purpose text DEFAULT 'interactive',ADD COLUMN title text DEFAULT 'private title',ADD COLUMN "archivedAt" timestamptz,ADD COLUMN "deletedBy_id" text,ADD COLUMN "purgedAt" timestamptz,ADD COLUMN "purgeRequestedAt" timestamptz,ADD COLUMN "updatedAt" timestamptz;
        ALTER TABLE public.app_core_agentrun ADD COLUMN IF NOT EXISTS prompt text DEFAULT 'private prompt',ADD COLUMN IF NOT EXISTS agent_instructions text DEFAULT 'private instructions';
        ALTER TABLE public.app_core_sessionevent ADD COLUMN "eventId" text PRIMARY KEY,ADD COLUMN projects_to_agent_run_stream boolean;
        DROP TABLE IF EXISTS public.app_core_transcriptoutputchunk,public.app_core_transcriptoutputcapture,public.app_core_materialresultsnapshot,public.app_core_materialevidencereceipt,public.app_core_runmaterialadmission,public.app_core_sessioncitationprojection,public.app_core_modelrunlog,public.app_core_agentrunenvironmentrelease CASCADE;
        CREATE TABLE public.app_core_transcriptoutputcapture(event_id text PRIMARY KEY REFERENCES public.app_core_sessionevent("eventId"),"executionId" text,sha256 text,"byteLength" bigint,"chunkSize" integer,"purgedAt" timestamptz);
        CREATE TABLE public.app_core_transcriptoutputchunk(capture_id text REFERENCES public.app_core_transcriptoutputcapture(event_id),data bytea);
        CREATE TABLE public.app_core_materialresultsnapshot(call_id text REFERENCES public.app_core_sessionevent("eventId"));
        CREATE TABLE public.app_core_materialevidencereceipt(call_id text REFERENCES public.app_core_sessionevent("eventId"));
        CREATE TABLE public.app_core_runmaterialadmission(origin_call_id text REFERENCES public.app_core_sessionevent("eventId"));
        CREATE TABLE public.app_core_sessioncitationprojection(session_id text);
        CREATE TABLE public.app_core_modelrunlog("agentRunId" text,error text);
        CREATE TABLE public.app_core_agentrunenvironmentrelease(agent_run_id text,environment_id text,authorization_digest text,controller_epoch bigint);
        INSERT INTO public.app_core_workspace VALUES('ws-other','active');
        INSERT INTO public.app_core_workspacemembership VALUES('member-other','ws-other','1','member'),('member-second','ws_1','2','member');
        INSERT INTO public.app_core_agentrun(id,session_id,environment_id,workspace_id,user_id,membership_ref,status,prompt,agent_instructions,"modelConfig_id","thinkingMode") VALUES('run-purge','sess_1','environment','ws_1','1','member','running','private prompt','private instructions','model_1','high');
    "#).unwrap();
    let mut authorization: WorkspaceAgentRunAuthorization = serde_json::from_str(include_str!(
        "../../../../../../tests/workspace/fixtures/agent_run_authorization/v2/valid.json"
    ))
    .unwrap();
    authorization.agent_run_id = "run-purge".into();
    authorization.id = "grant-purge".into();
    authorization.user_id = "1".into();
    let digest = authorization.digest().unwrap();
    let signature = authorization
        .signature(retained_instance_tests::KEY)
        .unwrap();
    db.execute("INSERT INTO public.app_core_agentrunauthorization VALUES('run-purge',$1,$2::text::jsonb,$3)", &[&digest,&serde_json::to_string(&authorization).unwrap(),&signature]).unwrap();
    let owner = "owner-run-purge";
    db.execute("UPDATE public.app_core_sessionenvironment SET environment_profile=$1::text::jsonb,environment_profile_digest=$2,controller_run_id='run-purge',controller_epoch=1,controller_lease_owner=$3,controller_purpose='execute',controller_released=false", &[&serde_json::to_string(&authorization.environment_profile).unwrap(),&authorization.environment_profile.digest().unwrap(),&owner]).unwrap();
    let lifecycle_id = "agent_run.lifecycle:run-purge";
    let mut lifecycle = job(lifecycle_id, &format!("{lifecycle_id}:{digest}"));
    lifecycle.job_kind = "agent_run.lifecycle".into();
    lifecycle.status = RuntimeJobStatus::Running;
    lifecycle.lease_owner = Some(owner.into());
    lifecycle.lease_expires_at_ms = Some(4_000_000_000_000);
    lifecycle.session_id = Some("sess_1".into());
    lifecycle.payload_ref = Some("record:agent_run:run-purge".into());
    store
        .schedule_worker_job(ScheduleRuntimeJobRequest { job: lifecycle }, Some("ws_1"))
        .unwrap();
    let mut lease_digest = Sha256::new();
    lease_digest.update(b"workspace:environment-controller-lease:v1\0");
    lease_digest.update(owner.as_bytes());
    let run = retained_instance_tests::Run {
        original: authorization.clone(),
        grant: retained_instance_tests::sign(EnvironmentBinding {
            schema: "workspace.environment_authorization.v1".into(),
            environment_id: "environment".into(),
            session_id: "sess_1".into(),
            agent_run_id: "run-purge".into(),
            agent_run_authorization_digest: digest.clone(),
            execution_mode: "retained".into(),
            controller: Some(ControllerBinding {
                controller_id: "run-purge".into(),
                controller_epoch: 1,
                controller_lease_digest: format!("sha256:{:x}", lease_digest.finalize()),
                controller_purpose: "execute".into(),
            }),
        }),
        fence: RuntimeJobLeaseFence {
            job_id: lifecycle_id.into(),
            job_kind: "agent_run.lifecycle".into(),
            lease_owner: owner.into(),
        },
    };
    let physical = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut allocation =
        retained_instance_tests::prepare(&store, &run, "purge-allocation", &physical).unwrap();
    let container = "a".repeat(64);
    db.execute(
        "UPDATE runtime.resident_sandboxes SET container_id=$1 WHERE execution_id=$2",
        &[&container, &allocation.allocation_execution_id],
    )
    .unwrap();
    physical.lock().unwrap()[0].container_id = container.clone();
    allocation.container_id = Some(container.clone());
    db.execute("INSERT INTO public.app_core_agentrunenvironmentrelease VALUES('run-purge','environment',$1,1)", &[&digest]).unwrap();
    db.execute("INSERT INTO runtime.agent_run_cleanup_receipts(receipt_id,agent_run_id,environment_id,authorization_digest,controller_epoch,allocation_kind,instance_id,cleanup_complete,file_protection,recorded_at_ms,physical_removed,removed_container_id) VALUES('cleanup-purge','run-purge','environment',$1,1,'instance',$2,true,'deferred',1,false,NULL)", &[&digest,&allocation.instance_id]).unwrap();
    db.batch_execute("UPDATE public.app_core_agentrun SET status='completed' WHERE id='run-purge'; UPDATE public.app_core_sessionenvironment SET controller_released=true").unwrap();
    let authorization_before: String = db.query_one("SELECT to_jsonb(a)::text FROM public.app_core_agentrunauthorization a WHERE agent_run_id='run-purge'", &[]).unwrap().get(0);
    let cleanup_before: String = db.query_one("SELECT to_jsonb(c)::text FROM runtime.agent_run_cleanup_receipts c WHERE agent_run_id='run-purge'", &[]).unwrap().get(0);
    body_row(
        &mut db,
        "event-user",
        "user_message",
        serde_json::json!({"messageId":"user","text":"private body","attachments":[]}),
    );
    body_row(
        &mut db,
        "event-terminal",
        "agent_run_completed",
        serde_json::json!({"doneReason":"end_turn"}),
    );
    db.execute("UPDATE runtime.runtime_jobs SET status='succeeded',lease_owner=NULL,lease_expires_at_ms=NULL WHERE job_id=$1", &[&lifecycle_id]).unwrap();
    body_row(
        &mut db,
        "event-capture",
        "tool_result",
        serde_json::json!({
            "callId":"call","toolName":"read","resultState":"successWithOutput","modelContent":"private body",
            "fullOutputPath":null,"outputStartByte":null,"outputByteLength":12,"outputComplete":true,
            "summary":"private preview","operations":[],"modelInputImages":[],"latencyMs":1,
        }),
    );
    let capture_digest = format!("sha256:{:x}", Sha256::digest(b"private body"));
    db.execute("INSERT INTO public.app_core_transcriptoutputcapture VALUES('event-capture','execution-purge',$1,12,65536,NULL)",&[&capture_digest]).unwrap();
    db.batch_execute("INSERT INTO public.app_core_transcriptoutputchunk VALUES('event-capture','private body'::bytea)").unwrap();
    let mut pending_job_id = String::new();
    for child in ["child-one", "child-two", "child-three"] {
        let packet_id = format!("packet-{child}");
        let result_id = format!("result-{child}");
        object(&store, &packet_id, &packet(child).to_string());
        object(&store, &result_id, "private child result");
        let mut job = build_subagent_run_job(SubagentRunJobRequest {
            session_id: "sess_1".into(),
            parent_turn_id: "parent-turn".into(),
            tool_call_id: child.into(),
            subagent_id: child.into(),
            work_packet_ref: packet_id,
            checkpoint_id: None,
            run_at_ms: 1,
            created_at_ms: 1,
            max_retries: 0,
        });
        job.status = RuntimeJobStatus::Succeeded;
        job.output_refs = vec![result_id];
        if child == "child-three" {
            pending_job_id = job.job_id.clone();
            job.status = RuntimeJobStatus::Queued;
        }
        store
            .schedule_runtime_job(ScheduleRuntimeJobRequest { job })
            .unwrap();
        store
            .save_agent_runtime_snapshot(child, "private child snapshot", 1)
            .unwrap();
        store
            .append_event(RuntimeEvent {
                event_id: format!("event-{child}"),
                session_id: child.into(),
                task_id: None,
                event_type: "test.child".into(),
                at_ms: 1,
                visibility: EventVisibility::Internal,
                payload_json: "{\"text\":\"private child event\"}".into(),
            })
            .unwrap();
    }
    store
        .save_agent_runtime_snapshot("sess_1", "private parent snapshot", 1)
        .unwrap();
    store
        .append_event(RuntimeEvent {
            event_id: "parent-event".into(),
            session_id: "sess_1".into(),
            task_id: None,
            event_type: "test.parent".into(),
            at_ms: 1,
            visibility: EventVisibility::Internal,
            payload_json: "{\"text\":\"private parent event\"}".into(),
        })
        .unwrap();
    store
        .save_agent_runtime_snapshot("unrelated", "retained", 1)
        .unwrap();
    let mut unrelated = job("unrelated-job", "unrelated-job");
    unrelated.session_id = Some("unrelated".into());
    unrelated.output_refs = vec!["result-child-two".into()];
    store
        .schedule_runtime_job(ScheduleRuntimeJobRequest { job: unrelated })
        .unwrap();
    store
        .link_external_context_object(ExternalContextObjectLink {
            session_id: "sess_1".into(),
            turn_id: None,
            tool_call_id: None,
            object_id: "result-child-two".into(),
            source_provider_id: "centaeris.core".into(),
            source_tool_name: "agent".into(),
            linked_at_ms: 1,
        })
        .unwrap();
    let request = SessionPurgeRequest {
        schema: "runtime.session.purge.v1".into(),
        session_id: "sess_1".into(),
        workspace_id: "ws_1".into(),
        owner_id: 1,
    };
    assert!(
        store
            .preflight_interactive_session_purge(&request)
            .unwrap_err()
            .contains("scope_rejected"),
        "physical retirement requires a committed platform deletion intent"
    );
    db.execute("UPDATE public.app_core_session SET \"purgeRequestedAt\"=clock_timestamp() WHERE id='sess_1'",&[]).unwrap();
    assert!(store
        .purge_interactive_session(&request)
        .unwrap_err()
        .contains("liabilities_pending"));
    assert_eq!(
        db.query_one(
            "SELECT status FROM public.app_core_session WHERE id='sess_1'",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        "active"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM public.app_core_transcriptoutputchunk",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    assert!(store
        .load_agent_runtime_snapshot("child-one")
        .unwrap()
        .is_some());
    db.execute(
        "UPDATE runtime.runtime_jobs SET status='succeeded' WHERE job_id=$1",
        &[&pending_job_id],
    )
    .unwrap();
    for (workspace, owner) in [("ws-other", 1), ("ws_1", 2)] {
        let wrong = SessionPurgeRequest {
            schema: request.schema.clone(),
            session_id: request.session_id.clone(),
            workspace_id: workspace.into(),
            owner_id: owner,
        };
        assert!(store
            .purge_interactive_session(&wrong)
            .unwrap_err()
            .contains("scope_rejected"));
    }
    let retirements = store.preflight_interactive_session_purge(&request).unwrap();
    assert_eq!(retirements.len(), 1);
    assert_eq!(
        retirements[0].operation_id,
        format!("session-delete:{}", allocation.instance_id)
    );
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=2")
        .unwrap();
    assert!(matches!(
        store.retire_retained_instance(&retirements[0], |_| panic!(
            "a stale controller cannot remove the instance"
        )),
        Err(crate::postgres_store::residency::retirement::RetirementError::ControllerEpochConflict)
    ));
    db.batch_execute("UPDATE public.app_core_sessionenvironment SET controller_epoch=1")
        .unwrap();
    assert_eq!(
        store
            .retire_retained_instance(&retirements[0], |_| Err("unknown Docker outcome".into()))
            .unwrap()
            .status,
        "pending"
    );
    let resumed = store.preflight_interactive_session_purge(&request).unwrap();
    assert_eq!(
        serde_json::to_string(&resumed[0]).unwrap(),
        serde_json::to_string(&retirements[0]).unwrap()
    );
    assert_eq!(
        store
            .retire_retained_instance(&resumed[0], |_| {
                physical.lock().unwrap().clear();
                Ok(true)
            })
            .unwrap()
            .status,
        "destroyed"
    );
    assert!(
        store
            .purge_interactive_session(&request)
            .unwrap_err()
            .contains("liabilities_pending"),
        "native Host cleanup remains an independent obligation"
    );
    assert_eq!(
        store
            .process_host_retirements("host", |target| {
                assert_eq!(target.container_id, container);
                Ok(())
            })
            .unwrap(),
        1
    );
    body_row(
        &mut db,
        "event-upload-pending",
        "tool_result",
        serde_json::json!({
            "callId":"pending-call","toolName":"read","resultState":"successWithOutput",
            "modelContent":"late","fullOutputPath":null,"outputStartByte":null,
            "outputByteLength":4,"outputComplete":true,"summary":"late preview",
            "operations":[],"modelInputImages":[],"latencyMs":1,
        }),
    );
    let mut uploader = Client::connect(&_url, NoTls).unwrap();
    let mut upload = uploader.transaction().unwrap();
    // This real FK insertion holds KEY SHARE on the committed result while an
    // asynchronous publisher is still uploading. The production purge must not
    // wait for that publisher before it can commit the Session tombstone.
    upload.execute("INSERT INTO public.app_core_transcriptoutputcapture VALUES('event-upload-pending','execution-late',$1,4,65536,NULL)",&[&capture_digest]).unwrap();
    let erased = store.purge_interactive_session(&request).unwrap();
    assert!(upload.query_opt("SELECT id FROM public.app_core_session WHERE id='sess_1' AND status='active' AND \"purgedAt\" IS NULL FOR UPDATE",&[]).unwrap().is_none(),"the blocked publisher cannot pass its final owner check after purge");
    upload.rollback().unwrap();
    assert_eq!(
        erased,
        vec!["child-one", "child-three", "child-two", "sess_1"]
    );
    for session in &erased {
        assert!(store
            .load_agent_runtime_snapshot(session)
            .unwrap()
            .is_none());
        assert_eq!(
            db.query_one(
                "SELECT count(*) FROM runtime.runtime_events WHERE session_id=$1",
                &[session]
            )
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        assert_eq!(
            db.query_one(
                "SELECT count(*) FROM runtime.runtime_jobs WHERE session_id=$1",
                &[session]
            )
            .unwrap()
            .get::<_, i64>(0),
            0
        );
    }
    assert!(store
        .load_agent_runtime_snapshot("unrelated")
        .unwrap()
        .is_some());
    for id in [
        "packet-child-one",
        "packet-child-two",
        "packet-child-three",
        "result-child-one",
        "result-child-three",
    ] {
        assert!(store.load_external_context_object(id).unwrap().is_none());
    }
    assert!(
        store
            .load_external_context_object("result-child-two")
            .unwrap()
            .is_some(),
        "another Session still owns this result reference"
    );
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM public.app_core_sessionevent WHERE \"eventId\"='event-user'",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    let capture = db.query_one("SELECT c.\"purgedAt\" IS NOT NULL,e.payload::text,c.sha256,c.\"byteLength\" FROM public.app_core_transcriptoutputcapture c JOIN public.app_core_sessionevent e ON e.\"eventId\"=c.event_id",&[]).unwrap();
    assert!(capture.get::<_, bool>(0));
    assert_eq!(capture.get::<_, String>(2), capture_digest);
    assert_eq!(capture.get::<_, i64>(3), 12);
    let wire: serde_json::Value = serde_json::from_str(&capture.get::<_, String>(1)).unwrap();
    assert_eq!(wire["payload"]["modelContent"], "");
    centaeris_core::session::parse_wire_record(&wire).unwrap();
    assert_eq!(
        db.query_one(
            "SELECT count(*) FROM public.app_core_transcriptoutputchunk",
            &[]
        )
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(
        db.query_one(
            "SELECT prompt||agent_instructions FROM public.app_core_agentrun WHERE id='run-purge'",
            &[]
        )
        .unwrap()
        .get::<_, String>(0),
        ""
    );
    assert_eq!(db.query_one("SELECT to_jsonb(a)::text FROM public.app_core_agentrunauthorization a WHERE agent_run_id='run-purge'",&[]).unwrap().get::<_,String>(0),authorization_before);
    assert_eq!(db.query_one("SELECT to_jsonb(c)::text FROM runtime.agent_run_cleanup_receipts c WHERE agent_run_id='run-purge'",&[]).unwrap().get::<_,String>(0),cleanup_before);
    assert!(db.query_one("SELECT status='deleted' AND \"purgedAt\" IS NOT NULL FROM public.app_core_session WHERE id='sess_1'",&[]).unwrap().get::<_,bool>(0));
    assert_eq!(
        store.purge_interactive_session(&request).unwrap(),
        vec!["sess_1"]
    );
}
