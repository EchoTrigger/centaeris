use super::*;
use rusqlite::{Connection, params};

fn run_owner(id: &str) -> EnvironmentTaskOwner {
    EnvironmentTaskOwner::AgentRun { run_id: id.into() }
}
fn maintenance_owner(id: &str) -> EnvironmentTaskOwner {
    EnvironmentTaskOwner::MaintenanceOperation {
        operation_id: id.into(),
    }
}
fn fence(owner: EnvironmentTaskOwner, epoch: u64) -> EnvironmentTaskFence {
    EnvironmentTaskFence {
        environment_instance_id: "i".into(),
        controller_owner: owner,
        controller_epoch: epoch,
        controller_purpose: EnvironmentTaskControllerPurpose::Execute,
    }
}
fn record(fence: &EnvironmentTaskFence) -> Record {
    Record {
        digest: "d".repeat(64),
        creator_owner: fence.controller_owner.clone(),
        identity_version: TaskIdentityVersion::OwnerV3,
        creator_epoch: fence.controller_epoch,
        snapshot: EnvironmentTaskSnapshot {
            task_id: fence.task_id(&fence.operation_id("start")).unwrap(),
            operation_id: fence.operation_id("start"),
            scope: owner_scope(&fence.controller_owner),
            state: EnvironmentTaskState::Running,
            exit_code: None,
            cleanup_complete: false,
            output_complete: false,
        },
    }
}
fn legacy() -> (serde_json::Value, String) {
    let owner = fence(run_owner("same"), 1);
    let id = migration::task_identity_v2(&owner, "1:start").unwrap();
    let mut value = serde_json::json!({"schema":"environment.task.v2","instance":"i","fence":{"environmentInstanceId":"i","controllerId":"same","controllerEpoch":1,"controllerPurpose":"execute"},"takeoverPending":false,"closedRun":null,"tasks":{},"receipts":{}});
    value["tasks"][&id] = serde_json::json!({"digest":"d".repeat(64),"creatorControllerId":"same","creatorEpoch":1,"snapshot":{"taskId":id,"operationId":"1:start","scope":{"kind":"run","runId":"same"},"state":"running","exitCode":null,"cleanupComplete":false,"outputComplete":false}});
    (value, id)
}

#[test]
fn maintenance_input_and_cleanup_use_stored_scope_before_accepting_a_receipt() {
    let root = tests::root();
    let mut journal = Journal::open(&root, "i").unwrap();
    let run = fence(run_owner("same"), 1);
    let task = record(&run);
    let id = task.snapshot.task_id.clone();
    journal.takeover(run.clone()).unwrap();
    journal.tasks.insert(id.clone(), task);
    journal.persist(&root).unwrap();
    let maintenance = fence(maintenance_owner("same"), 2);
    journal.takeover(maintenance.clone()).unwrap();
    assert_eq!(
        journal.authorize_task_input(&maintenance, &id).unwrap_err(),
        "task_owner_scope_denied"
    );
    assert!(
        maintenance
            .authorize_termination(&journal.tasks[&id].snapshot.scope)
            .is_err()
    );
    let task = record(&maintenance);
    let maintenance_id = task.snapshot.task_id.clone();
    journal.tasks.insert(maintenance_id.clone(), task);
    journal.persist(&root).unwrap();
    journal
        .authorize_task_input(&maintenance, &maintenance_id)
        .unwrap();
    let cleanup = EnvironmentTaskFence {
        controller_epoch: 3,
        controller_purpose: EnvironmentTaskControllerPurpose::Cleanup,
        ..maintenance
    };
    journal.takeover(cleanup.clone()).unwrap();
    assert_eq!(
        journal
            .authorize_task_input(&cleanup, &maintenance_id)
            .unwrap_err(),
        "task_cleanup_execute_denied"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn maintenance_scope_and_owner_are_immutable_in_the_index() {
    let root = tests::root();
    let mut journal = Journal::open(&root, "i").unwrap();
    let owner = fence(maintenance_owner("prepare"), 1);
    journal.takeover(owner.clone()).unwrap();
    let task = record(&owner);
    let id = task.snapshot.task_id.clone();
    journal.tasks.insert(id.clone(), task);
    journal.persist(&root).unwrap();
    let before = fs::read(root.join("history.sqlite")).unwrap();
    journal.tasks.get_mut(&id).unwrap().snapshot.scope = EnvironmentTaskScope::Environment;
    assert_eq!(
        journal.persist(&root).unwrap_err(),
        "task_index_identity_changed"
    );
    assert_eq!(fs::read(root.join("history.sqlite")).unwrap(), before);
    assert_eq!(
        store::task(&root, &id).unwrap().unwrap().snapshot.scope,
        owner_scope(&owner.controller_owner)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn maintenance_closure_and_input_receipts_do_not_alias_a_same_named_run() {
    let root = tests::root();
    let mut journal = Journal::open(&root, "i").unwrap();
    let maintenance = fence(maintenance_owner("same"), 1);
    journal.takeover(maintenance.clone()).unwrap();
    journal.closed_owner = Some(maintenance.controller_owner.clone());
    journal.receipts.insert(
        "endOperation:same".into(),
        Receipt {
            digest: "d".repeat(64),
            result: Some(EnvironmentTaskResult::OperationEnded {
                task_ids: vec![],
                cleanup_complete: true,
            }),
        },
    );
    journal.persist(&root).unwrap();
    let run = fence(run_owner("same"), 2);
    journal.takeover(run.clone()).unwrap();
    journal.persist(&root).unwrap();
    journal.check_admission().unwrap();
    assert!(store::closed(&root, &maintenance.controller_owner).unwrap());
    assert!(!store::closed(&root, &run.controller_owner).unwrap());
    assert_ne!(
        input_identity(&maintenance, "task", "1:input").unwrap(),
        input_identity(&run, "task", "2:input").unwrap()
    );
    assert!(matches!(
        store::receipt(&root, "endOperation:same")
            .unwrap()
            .unwrap()
            .result,
        Some(EnvironmentTaskResult::OperationEnded { .. })
    ));
    assert!(store::receipt(&root, "endRun:same").unwrap().is_none());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn v2_journal_forward_import_preserves_task_identity_and_requires_owner_recovery() {
    let root = tests::root();
    let (legacy, id) = legacy();
    let bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(root.join("journal.json"), &bytes).unwrap();
    let mut journal = Journal::open(&root, "i")
        .expect("v2 persisted history requires an explicit forward import");
    assert_eq!(fs::read(root.join("journal.legacy.json")).unwrap(), bytes);
    let task = &journal.tasks[&id];
    assert_eq!(task.snapshot.task_id, id);
    assert_eq!(task.digest, "d".repeat(64));
    assert_eq!(task.snapshot.state, EnvironmentTaskState::Unknown);
    assert!(!task.snapshot.cleanup_complete);
    assert!(!task.snapshot.output_complete);
    let owner = journal.fence.clone().unwrap();
    assert_eq!(
        journal.check_admission().unwrap_err(),
        "task_owner_upgrade_required"
    );
    assert_eq!(
        journal.authorize_task_input(&owner, &id).unwrap_err(),
        "task_owner_upgrade_required"
    );
    owner.authorize_termination(&task.snapshot.scope).unwrap();
    journal
        .takeover(fence(maintenance_owner("same"), 2))
        .unwrap();
    journal.check_admission().unwrap();
    journal.persist(&root).unwrap();
    Journal::open(&root, "i").unwrap();
    assert_eq!(fs::read(root.join("journal.legacy.json")).unwrap(), bytes);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_v2_history_is_rejected_without_rewriting_its_source() {
    for invalid in ["owner", "identity", "scope"] {
        let root = tests::root();
        let (mut legacy, id) = legacy();
        match invalid {
            "owner" => {
                legacy["fence"]["controllerOwner"] =
                    serde_json::json!({"kind":"maintenanceOperation","operationId":"same"})
            }
            "identity" => {
                legacy["tasks"][&id]["snapshot"]["taskId"] = serde_json::json!("foreign-task")
            }
            "scope" => {
                legacy["tasks"][&id]["snapshot"]["scope"] =
                    serde_json::json!({"kind":"operation","operationId":"same"})
            }
            _ => unreachable!(),
        }
        let bytes = serde_json::to_vec(&legacy).unwrap();
        fs::write(root.join("journal.json"), &bytes).unwrap();
        assert!(Journal::open(&root, "i").is_err());
        assert_eq!(fs::read(root.join("journal.json")).unwrap(), bytes);
        assert!(!root.join("journal.legacy.json").exists());
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn v2_index_forward_import_keeps_original_bytes_and_receipt_history() {
    let root = tests::root();
    let (mut legacy, id) = legacy();
    let legacy_backup = serde_json::to_vec(&legacy).unwrap();
    let snapshot = legacy["tasks"][&id]["snapshot"].clone();
    legacy["tasks"] = serde_json::json!({});
    let db = Connection::open(root.join("history.sqlite")).unwrap();
    db.execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL) WITHOUT ROWID; CREATE TABLE tasks(id TEXT PRIMARY KEY,record TEXT NOT NULL,live INTEGER NOT NULL,hot INTEGER NOT NULL) WITHOUT ROWID; CREATE TABLE operations(key BLOB PRIMARY KEY,receipt TEXT NOT NULL,task_id BLOB) WITHOUT ROWID; CREATE TABLE closed_runs(id TEXT PRIMARY KEY) WITHOUT ROWID; PRAGMA user_version=1;").unwrap();
    db.execute(
        "INSERT INTO metadata VALUES('journal',?1)",
        [legacy.to_string()],
    )
    .unwrap();
    let stored = serde_json::json!([
        "d".repeat(64),
        "same",
        1,
        "1:start",
        false,
        "running",
        null,
        0
    ]);
    db.execute(
        "INSERT INTO tasks VALUES(?1,?2,1,1)",
        params![id, stored.to_string()],
    )
    .unwrap();
    let receipt_key = format!("input:{id}:legacy");
    let receipt = serde_json::json!(["a".repeat(64), {"Mutated":[id,"running",null,0]}]);
    db.execute(
        "INSERT INTO operations VALUES(?1,?2,?3)",
        params![
            Sha256::digest(receipt_key.as_bytes()).as_slice(),
            receipt.to_string(),
            Sha256::digest(id.as_bytes()).as_slice()
        ],
    )
    .unwrap();
    db.execute("INSERT INTO closed_runs VALUES('past')", [])
        .unwrap();
    drop(db);
    fs::write(root.join("journal.legacy.json"), &legacy_backup).unwrap();
    fs::write(root.join("journal.json"), LEGACY_INDEX_MARKER).unwrap();
    let original = fs::read(root.join("history.sqlite")).unwrap();
    let journal =
        Journal::open(&root, "i").expect("the v1 index must migrate its v2 records forward");
    assert_eq!(fs::read(root.join("history.v1.sqlite")).unwrap(), original);
    assert_eq!(
        fs::read(root.join("journal.legacy.json")).unwrap(),
        legacy_backup
    );
    assert_eq!(
        journal.check_admission().unwrap_err(),
        "task_owner_upgrade_required"
    );
    let receipt = store::receipt(&root, &receipt_key).unwrap().unwrap();
    let Some(EnvironmentTaskResult::Mutated { task }) = receipt.result else {
        panic!("missing historical receipt")
    };
    assert_eq!(serde_json::to_value(task).unwrap(), snapshot);
    assert!(store::closed(&root, &run_owner("past")).unwrap());
    Journal::open(&root, "i").unwrap();
    assert_eq!(fs::read(root.join("history.v1.sqlite")).unwrap(), original);
    fs::remove_dir_all(root).unwrap();
}
