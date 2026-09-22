    #[test]
    fn export_released_upgrade_fixture() {
        use std::io::Write;
        let output = std::env::var("CENTAERIS_UPGRADE_FIXTURE_OUT").expect("fixture output");
        let mut events = valid_log();
        events.insert(3, base_event("model_request_started", "evt-model", model_request_payload(vec![])));
        events.insert(4, base_event("reasoning_block", "evt-reasoning", json!({
            "blockId": "reasoning:model-request-1", "requestId": "model-request-1",
            "text": "Upgrade fixture reasoning.", "status": "done"
        })));
        let records = events.iter().enumerate().map(|(index, event)| SequencedSessionRecord {
            sequence: index as u64 + 1, event: parse_event(event).expect("released event")
        }).collect::<Vec<_>>();
        validate_sequenced_session_records(&records).expect("released session");
        let mut file = std::fs::File::create(std::path::Path::new(&output).join("session.jsonl")).unwrap();
        serde_json::to_writer(&mut file, &SessionManifestV1::new("session-1", 1, "upgrade fixture").unwrap()).unwrap();
        writeln!(file).unwrap();
        for record in records {
            serde_json::to_writer(&mut file, &wire_record_value(&record).unwrap()).unwrap();
            writeln!(file).unwrap();
        }
    }
