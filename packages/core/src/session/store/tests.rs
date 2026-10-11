use super::*;
use crate::runtime::contracts::{CheckpointKindV1, EventVisibility};

fn request() -> SaveWaitCheckpointRequest {
    SaveWaitCheckpointRequest {
        checkpoint: CheckpointRecord {
            checkpoint_id: "checkpoint".into(),
            kind: CheckpointKindV1::Wait,
            session_id: "session".into(),
            turn_id: "turn".into(),
            status: "waiting".into(),
            done_reason: None,
            updated_at_ms: 1,
            payload_json: "{}".into(),
        },
        event: RuntimeEvent {
            event_id: "event".into(),
            session_id: "session".into(),
            task_id: Some("turn".into()),
            event_type: "wait".into(),
            at_ms: 1,
            visibility: EventVisibility::Internal,
            payload_json: "{}".into(),
        },
    }
}

#[test]
fn checkpoint_requests_accept_bound_events_without_new_field_restrictions() {
    let mut save = request();
    save.checkpoint.checkpoint_id.clear();
    save.event.event_id.clear();
    save.checkpoint.payload_json = "opaque checkpoint".into();
    save.event.payload_json = "opaque event".into();
    assert_eq!(save.validate(), Ok(()));
    assert_eq!(
        ConsumeWaitCheckpointRequest {
            checkpoint: save.checkpoint,
            events: vec![save.event]
        }
        .validate(),
        Ok(())
    );
}

#[test]
fn save_checkpoint_preserves_exact_binding_error() {
    for task in [None, Some("other".into())] {
        let mut save = request();
        save.event.task_id = task.clone();
        assert_eq!(save.validate().unwrap_err(),format!("save_wait_checkpoint scope mismatch: checkpointChat=session eventChat=session checkpointTurn=turn eventTask={task:?}"));
    }
    let mut save = request();
    save.event.session_id = "other".into();
    assert_eq!(save.validate().unwrap_err(),"save_wait_checkpoint scope mismatch: checkpointChat=session eventChat=other checkpointTurn=turn eventTask=Some(\"turn\")");
}

#[test]
fn consume_checkpoint_preserves_nonempty_binding_and_duplicate_rules() {
    let save = request();
    let mut consume = ConsumeWaitCheckpointRequest {
        checkpoint: save.checkpoint,
        events: vec![],
    };
    assert_eq!(
        consume.validate().unwrap_err(),
        "consume_wait_checkpoint requires events"
    );
    consume.events = vec![save.event.clone(), save.event];
    assert_eq!(
        consume.validate().unwrap_err(),
        "consume_wait_checkpoint duplicate event id"
    );
    consume.events[1].event_id = "second".into();
    assert_eq!(consume.validate(), Ok(()));
    consume.events[1].task_id = None;
    assert_eq!(
        consume.validate().unwrap_err(),
        "consume_wait_checkpoint event scope mismatch"
    );
    consume.events[1].task_id = Some("other".into());
    assert_eq!(
        consume.validate().unwrap_err(),
        "consume_wait_checkpoint event scope mismatch"
    );
    consume.events[1].task_id = Some("turn".into());
    consume.events[1].session_id = "other".into();
    assert_eq!(
        consume.validate().unwrap_err(),
        "consume_wait_checkpoint event scope mismatch"
    );
}

#[test]
fn consume_checkpoint_binding_error_precedes_duplicate_error_for_same_event() {
    let save = request();
    let mut wrong = save.event.clone();
    wrong.session_id = "other".into();
    assert_eq!(
        ConsumeWaitCheckpointRequest {
            checkpoint: save.checkpoint,
            events: vec![save.event, wrong]
        }
        .validate()
        .unwrap_err(),
        "consume_wait_checkpoint event scope mismatch"
    );
}
