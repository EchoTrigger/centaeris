//! Host-private indexed records. Historical rows are never loaded by a worker
//! unless their indexed state says that work is still pending.
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};
use std::path::Path;

pub(crate) fn open(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if version > 1 {
        return Err("unsupported local work database schema".into());
    }
    conn.execute_batch("PRAGMA synchronous=FULL")
        .map_err(|e| e.to_string())?;
    if version == 0 {
        conn.execute_batch("BEGIN IMMEDIATE;
        CREATE TABLE IF NOT EXISTS records(namespace TEXT NOT NULL,id TEXT NOT NULL,owner TEXT NOT NULL,state TEXT NOT NULL,due INTEGER,body TEXT NOT NULL,PRIMARY KEY(namespace,id));
        CREATE INDEX IF NOT EXISTS work_pending ON records(namespace,state,due,id);
        CREATE INDEX IF NOT EXISTS work_owner ON records(namespace,owner,id);
        CREATE INDEX IF NOT EXISTS work_history ON records(namespace,owner,due DESC,id DESC);
        PRAGMA user_version=1; COMMIT;").map_err(|e|e.to_string())?;
    }
    Ok(conn)
}
pub(crate) fn get<T: DeserializeOwned>(
    conn: &Connection,
    namespace: &str,
    id: &str,
) -> Result<Option<T>, String> {
    let json: Option<String> = conn
        .query_row(
            "SELECT body FROM records WHERE namespace=?1 AND id=?2",
            params![namespace, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    json.map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
        .transpose()
}
pub(crate) fn put<T: Serialize>(
    conn: &Connection,
    namespace: &str,
    id: &str,
    owner: &str,
    state: &str,
    due: Option<i64>,
    value: &T,
) -> Result<(), String> {
    let json = serde_json::to_string(value).map_err(|e| e.to_string())?;
    conn.execute("INSERT INTO records VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(namespace,id) DO UPDATE SET owner=excluded.owner,state=excluded.state,due=excluded.due,body=excluded.body",params![namespace,id,owner,state,due,json]).map_err(|e|e.to_string())?;
    Ok(())
}
pub(crate) fn query<T: DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<T>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(args, |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.map(|r| serde_json::from_str(&r.map_err(|e| e.to_string())?).map_err(|e| e.to_string()))
        .collect()
}

/// Receipts are looked up by key, never loaded into an unbounded in-memory map.
/// Service identity is part of the namespace, so a restarted service cannot
/// pretend to own a previous process. Intent is saved before spawn.
#[derive(Default)]
pub(crate) struct Receipts {
    connection: Option<Connection>,
}
impl Receipts {
    fn connection(&mut self) -> Result<&Connection, String> {
        if self.connection.is_none() {
            #[cfg(test)]
            let path = std::path::PathBuf::from(":memory:");
            #[cfg(not(test))]
            let path = crate::user_data_layout::desktop_data_root_dir()
                .join("runtime/work-receipts.sqlite3");
            self.connection = Some(open(&path)?);
        }
        Ok(self.connection.as_ref().unwrap())
    }
    pub fn get(
        &mut self,
        namespace: &str,
        key: &str,
        digest: &str,
    ) -> Result<Option<Result<String, String>>, String> {
        let row: Option<(String, Result<String, String>)> =
            get(self.connection()?, namespace, key)?;
        match row {
            Some((old, result)) if old == digest => Ok(Some(result)),
            Some(_) => Err("operation request conflict".into()),
            None => Ok(None),
        }
    }
    pub fn put(
        &mut self,
        namespace: &str,
        key: &str,
        digest: &str,
        result: &Result<String, String>,
    ) -> Result<(), String> {
        put(
            self.connection()?,
            namespace,
            key,
            "",
            "receipt",
            None,
            &(digest, result),
        )
    }
    pub fn result(
        &mut self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<Result<String, String>>, String> {
        let row: Option<(String, Result<String, String>)> =
            get(self.connection()?, namespace, key)?;
        Ok(row.map(|(_, r)| r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn historical_receipts_do_not_exhaust_admission_and_retries_keep_identity() {
        let mut receipts = Receipts::default();
        for i in 0..5000 {
            receipts
                .put(
                    "service",
                    &i.to_string(),
                    "digest",
                    &Ok(format!("process-{i}")),
                )
                .unwrap();
        }
        assert_eq!(
            receipts.get("service", "0", "digest").unwrap(),
            Some(Ok("process-0".into()))
        );
        assert!(receipts.get("service", "0", "changed").is_err());
        assert_eq!(receipts.get("other-service", "0", "digest").unwrap(), None);
        // An uncertain persisted intent is never treated as a fresh start.
        receipts
            .put("service", "next", "digest", &Err("unknown".into()))
            .unwrap();
        assert_eq!(
            receipts.get("service", "next", "digest").unwrap(),
            Some(Err("unknown".into()))
        );
    }

    #[test]
    fn receipt_reopen_reads_only_the_requested_key_and_rejects_newer_schema() {
        let root = std::env::temp_dir()
            .join(crate::process_sessions::Manager::default().service_instance_id());
        let path = root.join("receipts.sqlite3");
        let conn = open(&path).unwrap();
        put(
            &conn,
            "service",
            "first",
            "",
            "receipt",
            None,
            &("digest", Ok::<_, String>("process")),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO records VALUES('service','unrelated','','receipt',NULL,'invalid json')",
            [],
        )
        .unwrap();
        drop(conn);
        let mut receipts = Receipts {
            connection: Some(open(&path).unwrap()),
        };
        assert_eq!(
            receipts.get("service", "first", "digest").unwrap(),
            Some(Ok("process".into()))
        );
        receipts
            .connection
            .as_ref()
            .unwrap()
            .execute_batch("PRAGMA user_version=2")
            .unwrap();
        drop(receipts);
        assert!(open(&path).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
