//! Opt-in acceptance against a disposable, API-migrated PostgreSQL database.
//!
//! CENTAERIS_TEST_POSTGRES_URL must point to that isolated database. Set
//! CENTAERIS_NATIVE_FIXTURE_API to the loopback Django fixture, and supply its
//! internal token and the synthetic provider CA through CENTAERIS_NATIVE_FIXTURE_TOKEN
//! and CENTAERIS_NATIVE_FIXTURE_CA. Use the same test signing key in both processes.
//! The fixture seeds a Run at POST /__native_fixture/seed after Runtime schema
//! initialization, rotates its credential at POST /__native_fixture/rotate, and
//! reports provider and legacy gateway call counts at GET /__native_fixture/counts.
//! The TLS provider returns fixture-ok and explicit 8/2/12 usage.

use super::*;
use crate::api_model_client::{ApiModelClient, ApiModelClientConfig};
use centaeris_core::model::prepared_prompt::{
    ModelMessageRoleV1, ModelMessageV1, PreparedPromptV1,
};
use centaeris_core::model::ModelSessionConfigStore;
use centaeris_core::tool::ModelToolChoice;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Seed {
    start: AgentRunStart,
    lease_owner: String,
    provider_id: String,
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Counts {
    provider_requests: u32,
    legacy_requests: u32,
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires exclusively owned Django, PostgreSQL and TLS provider fixtures"]
async fn hosted_responses_fixture() {
    let env = |name| std::env::var(name).unwrap_or_else(|_| panic!("{name} required"));
    let db_url = env("CENTAERIS_TEST_POSTGRES_URL");
    let api_url = env("CENTAERIS_NATIVE_FIXTURE_API");
    let token = env("CENTAERIS_NATIVE_FIXTURE_TOKEN");
    let key = env("AGENT_RUN_AUTHORIZATION_SIGNING_KEY");
    let certificate = std::fs::read(env("CENTAERIS_NATIVE_FIXTURE_CA")).expect("read fixture CA");
    let http = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(&certificate).expect("fixture CA"))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("construct fixture client");
    let store = Arc::new(PostgresRuntimeStore::new(&db_url).expect("bootstrap Runtime schema"));
    let seed = http
        .post(format!("{api_url}/__native_fixture/seed"))
        .send()
        .await
        .expect("seed fixture")
        .error_for_status()
        .expect("fixture seed accepted")
        .json::<Seed>()
        .await
        .expect("fixture seed result");
    let start = seed
        .start
        .validate(key.as_bytes())
        .expect("verify signed fixture start");
    let gateway = NativeModelGateway {
        provider_id: seed.provider_id,
        http: http.clone(),
    };
    let authorize = || async {
        gateway
            .authorize(&api_url, &token, &start, &seed.lease_owner, store.clone())
            .await
            .expect("Django issues native grant")
            .expect("fixture provider selected for native route")
    };
    let make_client = |native| {
        ApiModelClient::new(ApiModelClientConfig {
            api_internal_url: api_url.clone(),
            internal_api_token: token.clone(),
            agent_run_id: start.agent_run_id.clone(),
            model_config_ref: start.authorization.model_config_ref.clone(),
            authorization_ref: start.authorization.id.clone(),
            authorization_digest: start.authorization_digest.clone(),
            thinking_mode: start.authorization.thinking_mode.clone(),
            model_max_output_tokens: start.model_max_output_tokens,
        })
        .with_native(Some(native))
    };
    let client = make_client(authorize().await);
    let session_id = &start.authorization.session_id;
    let request = ModelClientRequest {
        session_id: session_id.clone(),
        turn_id: start.turn_id.clone(),
        loop_index: 0,
        provider_prompt_cache_key: None,
        provider_prompt_cache_retention: None,
        system_prompt_manifest_json: None,
        compression_stats_json: None,
        context_token_estimate: 8,
        prepared_prompt: PreparedPromptV1::new(
            None,
            vec![ModelMessageV1 {
                message_id: "fixture-message".into(),
                role: ModelMessageRoleV1::User,
                content: "Reply with fixture-ok.".into(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                reasoning_content: None,
            }],
            Vec::new(),
            ModelToolChoice::None,
            64,
        )
        .expect("fixture prompt"),
        session_config: client
            .get_session_config(session_id)
            .expect("read native session model configuration")
            .expect("native model configuration"),
    };
    let mut events = Vec::new();
    let result = client
        .generate_stream(&request, &mut |event| events.push(event))
        .await
        .expect("native provider returns a complete response");
    assert_eq!(result.generate_result.content, "fixture-ok");
    assert_eq!(result.generate_result.input_tokens, Some(8));
    assert_eq!(result.generate_result.total_tokens, Some(12));
    assert_eq!(result.provider_attempts, 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelClientStreamEvent::Done { .. }))
            .count(),
        1,
    );
    let assert_no_dispatch = |client: ApiModelClient, request: ModelClientRequest| async move {
        let error = client
            .generate_stream(&request, &mut |_| {})
            .await
            .expect_err("native provider dispatch must be denied");
        assert_eq!(
            error.provider_code.as_deref(),
            Some("native_model_authority_denied")
        );
        assert!(!error.retryable);
    };

    http.post(format!("{api_url}/__native_fixture/rotate"))
        .send()
        .await
        .expect("request fixture credential rotation")
        .error_for_status()
        .expect("fixture credential rotated");
    assert_no_dispatch(client, request.clone()).await;
    let client = make_client(authorize().await);

    store
        .with_execution_control_client(|db| {
            db.execute(
                "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=0 WHERE job_id=$1",
                &[&format!("agent_run.lifecycle:{}", start.agent_run_id)],
            )
            .map_err(|error| error.to_string())
        })
        .expect("expire fixture execute lease");
    assert_no_dispatch(client.clone(), request.clone()).await;
    store
        .with_execution_control_client(|db| {
            db.execute(
                "UPDATE runtime.runtime_jobs SET lease_expires_at_ms=(EXTRACT(EPOCH FROM clock_timestamp())*1000)::bigint+60000 WHERE job_id=$1",
                &[&format!("agent_run.lifecycle:{}", start.agent_run_id)],
            ).map_err(|error| error.to_string())?;
            db.execute(
                "DELETE FROM public.app_core_workspacemembership WHERE id=(SELECT membership_ref FROM public.app_core_agentrun WHERE id=$1)",
                &[&start.agent_run_id],
            ).map_err(|error| error.to_string())
        })
        .expect("restore fixture lease and revoke membership");
    assert_no_dispatch(client, request).await;

    let counts = http
        .get(format!("{api_url}/__native_fixture/counts"))
        .send()
        .await
        .expect("read fixture traffic counts")
        .json::<Counts>()
        .await
        .expect("fixture traffic counts");
    assert_eq!(
        counts,
        Counts {
            provider_requests: 1,
            legacy_requests: 0
        }
    );
    let usage = store
        .with_client(|db| {
            db.query_one(
                r#"SELECT status,"promptTokens","completionTokens","totalTokens"
                   FROM public.app_core_modelrunlog WHERE "agentRunId"=$1"#,
                &[&start.agent_run_id],
            )
            .map(|row| {
                (
                    row.get::<_, String>(0),
                    row.get::<_, i32>(1),
                    row.get::<_, i32>(2),
                    row.get::<_, i32>(3),
                )
            })
            .map_err(|error| error.to_string())
        })
        .expect("read durable fixture usage");
    assert_eq!(usage, ("success".to_owned(), 8, 2, 12));
}
