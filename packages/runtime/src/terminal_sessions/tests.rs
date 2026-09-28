use super::*;
#[test]
fn log_is_bounded_and_reports_lost_output() {
    let mut log = Log::default();
    log.append(&vec![42; OUTPUT_CAP + 4096]);
    assert!(log.bytes <= OUTPUT_CAP);
    assert!(log.first() > 0);
}
#[test]
fn zero_dimensions_and_unknown_fields_are_rejected() {
    assert!(size(0, 24).is_err());
    assert!(size(80, 0).is_err());
    assert!(serde_json::from_value::<Target>(
        serde_json::json!({"terminalId":"t","serviceInstanceId":"s","extra":1})
    )
    .is_err());
}

#[test]
fn terminal_snapshot_matches_ui_wire_sample() {
    let snapshot = Snapshot {
        terminal_id: "terminal-1".into(),
        workspace_root: "/workspace".into(),
        session_id: None,
        shell: "pwsh".into(),
        state: "running",
        exit_code: None,
        output_complete: false,
        error: None,
    };
    let sample: serde_json::Value =
        serde_json::from_str(include_str!("../../generated/terminal-samples.json")).unwrap();
    assert_eq!(serde_json::to_value(snapshot).unwrap(), sample);
}
