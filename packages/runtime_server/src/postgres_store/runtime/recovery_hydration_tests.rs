use super::*;
use serde_json::{json, Value};

struct Fixture {
    manifests: HashMap<(String, String), ModelObservationManifest>,
    contents: HashMap<(String, String), ModelObservationContent>,
    records: Vec<(Value, Value)>,
}

fn fixture(rounds: usize) -> Fixture {
    let session_id = "owned-recovery-fixture";
    let mut fixture = Fixture {
        manifests: HashMap::new(),
        contents: HashMap::new(),
        records: Vec::new(),
    };
    let mut parent_digest = None;
    let mut references = Vec::new();
    let mut observations = Vec::new();
    for round in 1..=rounds {
        observations.push(json!({"kind":"message","message":{"messageId":format!("message-{round}"),"role":"user","content":format!("{round}:{}","x".repeat(1024))}}));
        let mut wire = json!({"type":"model_request_started","sessionId":session_id,"payload":{"requestId":format!("request-{round}"),"observations":observations}});
        let expected = wire.clone();
        let (new_contents, manifest, next_references) =
            compact_model_request_wire(&mut wire, round as i64, parent_digest, &references)
                .unwrap();
        parent_digest = Some(manifest.digest.clone());
        fixture
            .manifests
            .insert((session_id.into(), manifest.digest.clone()), manifest);
        for content in new_contents {
            fixture
                .contents
                .insert((session_id.into(), content.digest.clone()), content);
        }
        references = next_references;
        fixture.records.push((wire, expected));
    }
    fixture
}

#[test]
fn recovery_hydration_releases_previous_expanded_manifest_working_sets() {
    let fixture = fixture(100);
    let mut hydrator = SessionWireHydrator::default();
    let mut peak_owned_reference_strings = 0;
    for (index, (stored, expected)) in fixture.records.iter().enumerate() {
        let mut wire = stored.clone();
        hydrator
            .hydrate_loaded_record(&mut wire, &fixture.manifests, &fixture.contents)
            .unwrap();
        assert_eq!(&wire, expected);
        drop(wire); // The recovery reducer has consumed this record.
        let current = index + 1;
        assert!(
            hydrator.retained_reference_count() <= current,
            "completed records must not retain expanded references: current={current} retained={}",
            hydrator.retained_reference_count()
        );
        let owned = hydrator.retained_reference_string_bytes();
        // All digests are canonical SHA256 and the observation kind is unchanged.
        let largest_single_request_strings = current * ("message".len() + "sha256:".len() + 64);
        assert!(
            owned <= largest_single_request_strings,
            "retained reference bytes {owned} exceed one request {largest_single_request_strings}"
        );
        peak_owned_reference_strings = peak_owned_reference_strings.max(owned);
    }
    println!("recovery roots=100 final_refs={} peak_reference_string_bytes={peak_owned_reference_strings}; hydrated bodies consumed and dropped per record",hydrator.retained_reference_count());
}

#[test]
fn recovery_hydration_branch_and_revisit_preserve_exact_record_bytes() {
    let fixture = fixture(12);
    let mut hydrator = SessionWireHydrator::default();
    for index in [8, 2, 11, 11, 0, 1, 10] {
        let (stored, expected) = &fixture.records[index];
        let mut wire = stored.clone();
        hydrator
            .hydrate_loaded_record(&mut wire, &fixture.manifests, &fixture.contents)
            .unwrap();
        assert_eq!(&wire, expected);
        assert_eq!(hydrator.retained_reference_count(), index + 1);
    }
}

#[test]
fn recovery_hydration_rejects_missing_ancestors_cycles_and_foreign_session() {
    let fixture = fixture(3);
    let stored = fixture.records.last().unwrap().0.clone();
    let root = model_request_manifest_key(&stored).unwrap().unwrap();
    let parent = fixture.manifests[&root].parent_digest.clone().unwrap();
    let mut missing = fixture.manifests.clone();
    missing.remove(&(root.0.clone(), parent.clone()));
    let error = SessionWireHydrator::default()
        .hydrate_loaded_record(&mut stored.clone(), &missing, &fixture.contents)
        .unwrap_err();
    assert!(error.contains("manifest is missing"), "{error}");
    let mut cycle = fixture.manifests.clone();
    cycle
        .get_mut(&(root.0.clone(), parent))
        .unwrap()
        .parent_digest = Some(root.1.clone());
    let error = SessionWireHydrator::default()
        .hydrate_loaded_record(&mut stored.clone(), &cycle, &fixture.contents)
        .unwrap_err();
    assert!(error.contains("parent cycle"), "{error}");
    let mut foreign = stored;
    foreign["sessionId"] = json!("other-session");
    let error = SessionWireHydrator::default()
        .hydrate_loaded_record(&mut foreign, &fixture.manifests, &fixture.contents)
        .unwrap_err();
    assert!(error.contains("manifest is missing"), "{error}");
}

#[test]
fn recovery_hydration_rejects_missing_changed_or_wrong_kind_content() {
    let fixture = fixture(3);
    let stored = fixture.records.last().unwrap().0.clone();
    let key = fixture.contents.keys().next().unwrap().clone();
    for mutation in ["missing", "changed", "kind"] {
        let mut contents = fixture.contents.clone();
        match mutation {
            "missing" => {
                contents.remove(&key);
            }
            "changed" => contents.get_mut(&key).unwrap().content_json.push(' '),
            "kind" => contents.get_mut(&key).unwrap().kind = "unsupported".into(),
            _ => unreachable!(),
        }
        let mut wire = stored.clone();
        let error = SessionWireHydrator::default()
            .hydrate_loaded_record(&mut wire, &fixture.manifests, &contents)
            .unwrap_err();
        assert_eq!(
            wire, stored,
            "a failed hydration must not publish partial observations"
        );
        assert!(error.contains("content"), "{mutation}: {error}");
    }
}

#[test]
fn recovery_manifest_decode_keeps_strict_fields_kind_and_digest_validation() {
    let fixture = fixture(1);
    let manifest = fixture.manifests.values().next().unwrap();
    for case in ["unknown_field", "unknown_kind", "digest"] {
        let mut value: Value = serde_json::from_str(&manifest.manifest_json).unwrap();
        match case {
            "unknown_field" => value["schema"] = json!("unsupported"),
            "unknown_kind" => value["changes"][0]["kind"] = json!("unsupported"),
            "digest" => value["changes"][0]["contentDigest"] = json!("not-a-digest"),
            _ => unreachable!(),
        }
        let raw = serde_json::to_string(&value).unwrap();
        let digest = sha256_json(MODEL_OBSERVATION_MANIFEST_DIGEST_DOMAIN, &raw);
        assert!(
            decode_model_observation_manifest(digest, None, raw.clone(), raw.len() as i64, 0)
                .is_err(),
            "{case}"
        );
    }
}
