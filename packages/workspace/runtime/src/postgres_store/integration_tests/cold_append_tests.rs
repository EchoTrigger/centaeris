use super::*;
use serde_json::json;

pub(super) const SESSION: &str = "cold-append-session";
pub(super) const RUN: &str = "cold-append-run";
pub(super) const WORKSPACE: &str = "cold-append-workspace";

pub(super) struct Fixture {
    pub(super) store: PostgresRuntimeStore,
    pub(super) runtime: tokio::runtime::Runtime,
    pub(super) next_sequence: u64,
    pub(super) largest_request_bytes: usize,
}
impl Fixture {
    pub(super) fn fresh_log(&self) -> super::super::PostgresSessionLog {
        self.store
            .session_log(WORKSPACE.into(), SESSION.into(), "cold append".into())
    }
}

fn composition() -> centaeris_core::extension::composition::ResolvedAgentCompositionV1 {
    use centaeris_core::extension::composition::*;
    let digest = format!("sha256:{}", "a".repeat(64));
    resolve_agent_composition(
        AgentCompositionInputsV1 {
            prompt_digest: digest.clone(),
            model_binding: ResolvedModelBindingV1 {
                provider_id: "test-provider".into(),
                model_name: "test-model".into(),
                wire_protocol: "test-wire".into(),
                config_digest: digest.clone(),
            },
            skill_catalog_digest: digest.clone(),
            plugin_activation_digest: digest.clone(),
            hook_composition_digest: digest.clone(),
            execution_profile_digest: digest,
            policy_version: "test-v1".into(),
        },
        std::iter::empty(),
    )
    .unwrap()
}

fn assistant(sequence: u64) -> SequencedSessionRecord {
    let turn = format!("cold-turn-{}", sequence - 1);
    let mut record = session_record(
        RUN,
        SESSION,
        sequence,
        SessionRecordType::AssistantMessage,
        json!({"messageId":format!("message:{turn}:assistant"),"modelMarkdown":"accepted","artifactRefs":[],"status":"done"}),
        sequence as i64,
    );
    record.event.turn_id = Some(turn);
    record
}

pub(super) fn control_record(sequence: u64) -> SequencedSessionRecord {
    let mut record = session_record(
        RUN,
        SESSION,
        sequence,
        SessionRecordType::PhaseEvent,
        json!({"stage":"model_process_summary","message":"accepted"}),
        sequence as i64,
    );
    record.event.turn_id = Some(format!("cold-control-{sequence}"));
    record
}

pub(super) fn fixture(requests: usize) -> Fixture {
    let url = test_url();
    reset_store(&url);
    let mut setup = Client::connect(&url, NoTls).unwrap();
    setup.batch_execute(r#"
        DROP TABLE IF EXISTS public.app_core_sessionevent CASCADE;
        DROP TABLE IF EXISTS public.app_core_session CASCADE;
        CREATE TABLE public.app_core_session(id text PRIMARY KEY,workspace_id text NOT NULL);
        CREATE TABLE public.app_core_sessionevent(
            "eventId" text PRIMARY KEY,workspace_id text NOT NULL,session_id text NOT NULL,
            agent_run_id text NOT NULL,sequence integer NOT NULL,agent_run_sequence integer,
            session_level boolean NOT NULL DEFAULT false,projects_to_agent_run_stream boolean NOT NULL,
            payload jsonb NOT NULL,"createdAtMs" bigint NOT NULL,"insertedAt" timestamptz NOT NULL DEFAULT clock_timestamp(),
            UNIQUE(session_id,sequence),UNIQUE(agent_run_id,agent_run_sequence));
        INSERT INTO public.app_core_session VALUES('cold-append-session','cold-append-workspace');
    "#).unwrap();
    drop(setup);
    let store = PostgresRuntimeStore::new(&url).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let warm = store.session_log(WORKSPACE.into(), SESSION.into(), "cold append".into());
    runtime.block_on(warm.append_session_records(RUN,&[
        session_record(RUN,SESSION,1,SessionRecordType::AgentRunStarted,json!({"userObjective":"cold append"}),1),
        session_record(RUN,SESSION,2,SessionRecordType::UserMessage,json!({"messageId":"message:turn_pg_fenced_terminal:user","text":"cold append","attachments":[]}),1),
    ])).unwrap();
    let composition = composition();
    let mut next = 3;
    let mut largest_request_bytes = 0;
    for index in 0..requests {
        // The final context is small. Cold validation still must visit the
        // earlier large, overlapping contexts, rather than only the last root.
        let prefix = if index + 1 == requests {
            "latest small".into()
        } else {
            "shared context ".repeat(2048)
        };
        let observations = json!([
            {"kind":"system_prompt","content":prefix},
            {"kind":"message","message":{"messageId":format!("context-{index}"),"role":"user","content":format!("request-{index}")}},
        ]);
        largest_request_bytes = largest_request_bytes.max(observations.to_string().len());
        let mut request = session_record(
            RUN,
            SESSION,
            next,
            SessionRecordType::ModelRequestStarted,
            json!({
                "requestId":format!("request-{index}"),"purpose":"main","loopIndex":index,
                "toolChoice":{"type":"none"},"maxOutputTokens":1024,"promptCacheKey":null,"promptCacheRetention":null,
                "preparedPromptSchema":"prepared_prompt.v1","contextTokenEstimate":0,
                "contextTokenBreakdown":{"systemPromptTokens":0,"systemToolTokens":0,"mcpToolTokens":0,"skillsTokens":0,"messageTokens":0,"mcpTools":[]},
                "agentComposition":composition,"observations":observations,
            }),
            next as i64,
        );
        request.event.turn_id = Some(format!("cold-turn-{next}"));
        let mut phase = control_record(next + 1);
        phase.event.turn_id = request.event.turn_id.clone();
        runtime
            .block_on(warm.append_session_records(RUN, &[request, phase]))
            .unwrap();
        next += 2;
    }
    drop(warm);
    Fixture {
        store,
        runtime,
        next_sequence: next,
        largest_request_bytes,
    }
}

pub(super) fn assert_bounded_cold_scan(fixture: &Fixture, log: &super::super::PostgresSessionLog) {
    let observed = log.cold_append_observation();
    println!(
        "cold append loads={} pages={} encodedPageMax={} expandedRecordsMax={} expandedBytesMax={}",
        observed.loads,
        observed.pages,
        observed.max_encoded_page_bytes,
        observed.max_expanded_model_records,
        observed.max_expanded_model_bytes
    );
    assert_eq!(
        observed.loads, 1,
        "a fresh logger must actually rebuild its append state"
    );
    assert!(
        observed.max_expanded_model_records <= 1,
        "historical model requests are simultaneously expanded: {}",
        observed.max_expanded_model_records
    );
    assert!(
        observed.max_expanded_model_bytes <= fixture.largest_request_bytes,
        "retained expanded bytes exceed the largest single historical request"
    );
    assert!(
        observed.pages >= 3,
        "the fixture must exercise at least three actual encoded pages"
    );
    assert_eq!(observed.records as u64, fixture.next_sequence - 1);
    assert_eq!(
        observed.expanded_model_records as u64,
        (fixture.next_sequence - 3) / 2,
        "every earlier model request must still be hydrated and validated"
    );
    assert!(log.has_append_cache(RUN));
}

#[test]
#[ignore = "requires destructive dedicated Postgres test database"]
fn postgres_cold_append_bounds_overlapping_model_observations() {
    let _guard = TEST_LOCK.lock().unwrap();
    let fixture = fixture(100);
    let cold = fixture.fresh_log();
    assert!(
        !cold.has_append_cache(RUN),
        "fresh must not share the warm cache"
    );
    let event = control_record(fixture.next_sequence);
    let receipt = fixture
        .runtime
        .block_on(cold.append_session_records(RUN, std::slice::from_ref(&event)))
        .unwrap();
    assert_eq!(receipt.records[0].event, event.event);
    assert_bounded_cold_scan(&fixture, &cold);
    let retry = fixture
        .runtime
        .block_on(cold.append_session_records(RUN, std::slice::from_ref(&event)))
        .unwrap();
    assert_eq!(
        retry, receipt,
        "lost acknowledgment retry must return the same committed batch"
    );
    assert_eq!(cold.cold_append_observation().loads, 1);
}

#[test]
fn cold_append_control_records_follow_existing_core_schema() {
    for record in [assistant(4), control_record(5)] {
        let wire = centaeris_core::session::wire_record_value(&record).unwrap();
        centaeris_core::session::parse_wire_record(&wire).unwrap();
    }
}

#[test]
#[ignore = "requires separately scheduled destructive Postgres history pressure test"]
fn postgres_cold_append_pressure_at_one_hundred_five_hundred_and_one_thousand_requests() {
    let _guard = TEST_LOCK.lock().unwrap();
    for requests in [100, 500, 1000] {
        let fixture = fixture(requests);
        let start = super::cold_append_fault_tests::start();
        let mut sequence = crate::load_existing_session_sequence(&fixture.store, &start).unwrap();
        assert!(!sequence.is_empty());
        let fence = super::cold_append_fault_tests::lease(&fixture);
        let cold = fixture.fresh_log();
        assert!(!cold.has_append_cache(RUN));
        let outcome = crate::commit_failed_agent_run(
            &fixture.runtime,
            &cold,
            &start,
            &mut sequence,
            &mut None,
            "turn_input_prepared_message_missing",
            "core_run_failed",
            &crate::AssistantTextProjection::default(),
            &fence,
        )
        .unwrap();
        assert_eq!(outcome.terminal_state, Some("failed"));
        let observed = cold.cold_append_observation();
        println!("cold failure append historicalRequests={requests} pages={} expandedRecordsMax={} expandedBytesMax={}",
            observed.pages, observed.max_expanded_model_records, observed.max_expanded_model_bytes);
        assert_bounded_cold_scan(&fixture, &cold);
    }
}

#[test]
fn cold_append_control_records_restore_distinct_phase_turns() {
    let mut state = centaeris_core::session::AgentRunSessionState::new(SESSION, RUN).unwrap();
    for record in [
        session_record(
            RUN,
            SESSION,
            1,
            SessionRecordType::AgentRunStarted,
            json!({"userObjective":"cold append"}),
            1,
        ),
        session_record(
            RUN,
            SESSION,
            2,
            SessionRecordType::UserMessage,
            json!({"messageId":"message:turn_pg_fenced_terminal:user","text":"cold append","attachments":[]}),
            1,
        ),
        control_record(3),
        control_record(4),
    ] {
        state.restore(record).unwrap();
    }
}
