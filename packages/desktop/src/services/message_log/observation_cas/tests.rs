use super::*;
use serde_json::json;

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "centaeris-manifest-gate-{}-{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).expect("create isolated manifest test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn observation_reference(value: &Value) -> ObservationReference {
    let content = observation_content(value).expect("canonical observation");
    ObservationReference {
        kind: content.kind,
        digest: content.digest,
    }
}

#[test]
#[ignore = "performance/release gate"]
fn observation_manifest_growth_is_linear_with_early_changes_through_4095_observations() {
    let directory = TestDirectory::new();
    let log_path = directory.0.join("session-growth.jsonl");
    let content_dir = content_directory(&log_path, "session-growth").unwrap();
    ensure_content_directory(&content_dir).unwrap();
    let mut log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&log_path)
        .unwrap();
    writeln!(log, "{{}}").unwrap();
    let mut observations = vec![
        json!({"kind": "system_prompt", "content": "stable system"}),
        json!({"kind": "message", "message": {"messageId": "runtime-context", "role": "user", "content": "runtime context 0"}}),
        json!({"kind": "message", "message": {"messageId": "stable-context", "role": "user", "content": "stable prior context"}}),
    ];
    let mut references = vec![
        observation_reference(&observations[0]),
        ObservationReference {
            kind: "message".to_string(),
            digest: format!("sha256:{}", "0".repeat(64)),
        },
        observation_reference(&observations[2]),
    ];
    let mut parent_digest = None;
    let mut parent = Vec::new();
    let mut legacy_full_refs = 0usize;
    let mut submitted_cas_content_bytes = 0usize;
    let mut curve = Vec::new();
    for round in 1..=2046usize {
        observations[1] = json!({"kind": "message", "message": {
            "messageId": "runtime-context", "role": "user", "content": format!("runtime context {round}")
        }});
        references[1] = observation_reference(&observations[1]);
        for role in ["user", "assistant"] {
            let observation = json!({"kind": "message", "message": {
                "messageId": format!("{round}-{role}"), "role": role,
                "content": format!("{round}:{role}:{}", "m".repeat(64)),
            }});
            references.push(observation_reference(&observation));
            observations.push(observation);
        }
        let (new_contents, manifest, prepared_references) =
            prepare_observation_manifest(&observations, parent_digest, &parent).unwrap();
        assert_eq!(prepared_references, references);
        assert_eq!(new_contents.len(), if round == 1 { 5 } else { 3 });
        for content in new_contents {
            submitted_cas_content_bytes += content.content_json.len();
            install_content(&content_dir, &content).unwrap();
        }
        assert_eq!(manifest.changes.len(), if round == 1 { 5 } else { 3 });
        install_manifest(&content_dir, &manifest).unwrap();
        let wire = json!({"type": "model_request_started", "sessionId": "session-growth",
            "payload": {"requestId": format!("request-{round}"),
                "observations": {"manifestDigest": manifest.digest}}});
        let root_bytes = serde_json::to_vec(&wire).unwrap();
        assert!(
            root_bytes.len() < 240,
            "event payload must remain one O(1) root"
        );
        log.write_all(&root_bytes).unwrap();
        log.write_all(b"\n").unwrap();
        legacy_full_refs += references.len();
        parent_digest = Some(manifest.digest);
        parent = references.clone();
        if [20, 81, 512, 2046].contains(&round) {
            log.sync_data().unwrap();
            let mut hydrated = vec![wire];
            hydrate_wires(&log_path, "session-growth", &mut hydrated).unwrap();
            assert_eq!(hydrated[0]["payload"]["observations"], json!(observations));
            let mut nodes = 0usize;
            let mut changed_refs = 0usize;
            let mut contents = 0usize;
            let mut cas_bytes = 0u64;
            let mut manifest_bytes = 0u64;
            for entry in fs::read_dir(&content_dir).unwrap() {
                let path = entry.unwrap().path();
                let bytes = fs::metadata(&path).unwrap().len();
                cas_bytes += bytes;
                if path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(MANIFEST_FILE_PREFIX)
                {
                    nodes += 1;
                    manifest_bytes += bytes;
                    let value: Value =
                        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
                    changed_refs += value["changes"].as_array().unwrap().len();
                } else {
                    contents += 1;
                }
            }
            assert_eq!(nodes, round);
            assert_eq!(changed_refs, round * 3 + 2);
            assert_eq!(contents, round * 3 + 2);
            assert_eq!(
                submitted_cas_content_bytes as u64,
                cas_bytes - manifest_bytes
            );
            let jsonl_bytes = fs::metadata(&log_path).unwrap().len();
            curve.push(
                json!({"rounds": round, "observationCount": references.len(),
                "legacyFullRefs": legacy_full_refs, "manifestRefs": changed_refs,
                "manifestNodes": nodes, "manifestBytes": manifest_bytes, "uniqueContents": contents,
                "jsonlBytes": jsonl_bytes, "casBytes": cas_bytes,
                "submittedCasContentBytes": submitted_cas_content_bytes,
                "physicalBytes": cas_bytes + jsonl_bytes}),
            );
        }
    }
    let small = curve[1]["physicalBytes"].as_u64().unwrap();
    let large = curve[3]["physicalBytes"].as_u64().unwrap();
    assert!(
        large * 81 * 100 < small * 2046 * 115,
        "bytes per round must stay within 15%, including growing integer widths"
    );
    assert_eq!(references.len(), 4095);
    println!(
        "RUNTIME_01_ARTIFACT {}",
        json!({
            "gate": "local_manifest_linear_growth", "measurement": "actual_jsonl_and_cas_file_bytes",
            "workload": "early_runtime_context_replaced_and_two_tail_observations_appended_per_round",
            "curve": curve,
        })
    );
}

#[test]
fn observation_manifest_rejects_corruption_missing_parent_and_cross_session_roots() {
    let directory = TestDirectory::new();
    let log_path = directory.0.join("session-a.jsonl");
    fs::write(&log_path, "{}\n").unwrap();
    let mut wires = vec![
        json!({"type": "model_request_started", "sessionId": "session-a",
        "payload": {"observations": [{"kind": "system_prompt", "content": "secret"}]}}),
    ];
    compact_and_install_wires(&log_path, "session-a", &mut wires).unwrap();
    let content_dir = content_directory(&log_path, "session-a").unwrap();
    let root = observation_manifest_digest(&wires[0]).unwrap();
    let manifest_path = manifest_file_path(&content_dir, &root).unwrap();
    let valid_manifest = fs::read_to_string(&manifest_path).unwrap();
    fs::write(&manifest_path, "{}").unwrap();
    assert!(hydrate_wires(&log_path, "session-a", &mut wires.clone())
        .unwrap_err()
        .contains("digest mismatch"));
    fs::write(&manifest_path, &valid_manifest).unwrap();
    let other_log = directory.0.join("session-b.jsonl");
    let mut other = wires.clone();
    other[0]["sessionId"] = json!("session-b");
    assert!(hydrate_wires(&other_log, "session-b", &mut other).is_err());
    assert!(hydrate_wires(&log_path, "session-a", &mut other)
        .unwrap_err()
        .contains("sessionId mismatch"));
    let parent = resolve_manifest_references(&content_dir, &root, &mut BTreeMap::new()).unwrap();
    let child = build_manifest(Some(root.clone()), &parent, &[]).unwrap();
    install_manifest(&content_dir, &child).unwrap();
    wires[0]["payload"]["observations"] = json!({"manifestDigest": child.digest});
    hydrate_wires(&log_path, "session-a", &mut wires.clone()).unwrap();
    fs::remove_file(&manifest_path).unwrap();
    assert!(hydrate_wires(&log_path, "session-a", &mut wires)
        .unwrap_err()
        .contains("read model observation manifest"));
}

#[test]
fn observation_manifest_index_delta_rejects_holes_duplicates_and_unknown_fields() {
    let reference = json!({"index": 0, "kind": "system_prompt", "contentDigest": format!("sha256:{}", "a".repeat(64))});
    let valid =
        json!({"parentDigest": null, "observationCount": 1, "changes": [reference.clone()]});
    for invalid in [
        json!({"parentDigest": null, "observationCount": 2, "changes": [reference.clone(), reference.clone()]}),
        json!({"parentDigest": null, "observationCount": 1, "changes": [{"index": 1, "kind": "system_prompt", "contentDigest": format!("sha256:{}", "a".repeat(64))}]}),
        json!({"parentDigest": null, "observationCount": 1, "changes": [reference], "extra": true}),
    ] {
        let raw = invalid.to_string();
        assert!(decode_manifest(&digest_manifest(&raw), raw).is_err());
    }
    let raw = valid.to_string();
    let root = decode_manifest(&digest_manifest(&raw), raw).unwrap();
    let references = apply_manifest(&root, Vec::new()).unwrap();
    let child = build_manifest(Some(root.digest.clone()), &references, &references).unwrap();
    assert!(child.changes.is_empty());
    let mut hole = child.clone();
    hole.observation_count = 2;
    assert!(apply_manifest(&hole, references.clone())
        .unwrap_err()
        .contains("count mismatch"));
    let mut redundant = child;
    redundant.changes = root.changes;
    assert!(apply_manifest(&redundant, references)
        .unwrap_err()
        .contains("redundant"));
}

#[test]
fn observation_cas_skips_non_model_batches_without_reading_log() {
    let directory = TestDirectory::new();
    let missing_log = directory.0.join("session-skip.jsonl");
    compact_and_install_wires(
        &missing_log,
        "session-skip",
        &mut [json!({"type": "tool_result"})],
    )
    .unwrap();
    assert!(!missing_log.exists());
}

#[test]
fn observation_cas_cleans_only_uncommitted_orphans_and_rejects_missing_content() {
    let directory = TestDirectory::new();
    let log_path = directory.0.join("session-orphan.jsonl");
    fs::write(&log_path, "{}\n").unwrap();
    let mut wires = vec![
        json!({"type": "model_request_started", "sessionId": "session-orphan",
        "payload": {"observations": [{"kind": "system_prompt", "content": "committed"}]}}),
    ];
    compact_and_install_wires(&log_path, "session-orphan", &mut wires).unwrap();
    fs::write(&log_path, format!("{{}}\n{}\n", wires[0])).unwrap();
    let content_dir = content_directory(&log_path, "session-orphan").unwrap();
    let orphan =
        observation_content(&json!({"kind": "system_prompt", "content": "uncommitted"})).unwrap();
    install_content(&content_dir, &orphan).unwrap();
    fs::write(content_dir.join(".interrupted.tmp"), "partial").unwrap();
    cleanup_session_content_directory(&log_path, "session-orphan", &content_dir).unwrap();
    assert!(!content_file_path(&content_dir, &orphan.digest)
        .unwrap()
        .exists());
    assert!(!content_dir.join(".interrupted.tmp").exists());
    hydrate_wires(&log_path, "session-orphan", &mut wires.clone()).unwrap();
    let committed =
        observation_content(&json!({"kind": "system_prompt", "content": "committed"})).unwrap();
    fs::remove_file(content_file_path(&content_dir, &committed.digest).unwrap()).unwrap();
    assert!(hydrate_wires(&log_path, "session-orphan", &mut wires).is_err());
    assert!(cleanup_session_content_directory(&log_path, "session-orphan", &content_dir).is_err());
}

#[test]
fn observation_cas_gc_does_not_follow_directory_links_outside_sessions() {
    let directory = TestDirectory::new();
    let sessions = directory.0.join("sessions");
    let outside = directory.0.join("outside");
    fs::create_dir(&sessions).unwrap();
    fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("sentinel");
    fs::write(&sentinel, "must remain").unwrap();
    for link in [
        sessions.join("2026"),
        sessions
            .join("2026")
            .join("08")
            .join("31")
            .join("session-linked.observations"),
    ] {
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(&outside, &link).is_err() {
            let result = std::process::Command::new("cmd.exe")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&outside)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "create isolated directory junction fixture: {} {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
        assert!(cleanup_orphan_content_directories(&sessions)
            .unwrap_err()
            .contains("symbolic links"));
        assert!(ensure_content_directory(&link).is_err());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "must remain");
        #[cfg(windows)]
        fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        fs::remove_file(&link).unwrap();
    }
}
