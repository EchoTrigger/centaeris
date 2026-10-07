use postgres::{GenericClient, Row};

use centaeris_core::session::external_context::{
    ExternalContextObject, ExternalContextObjectIndexEntry, ExternalContextObjectLink,
    ExternalContextStorePort, ListExternalContextObjectsRequest,
};

use super::runtime::to_i64;
use super::PostgresRuntimeStore;

impl ExternalContextStorePort for PostgresRuntimeStore {
    fn upsert_external_context_object(&self, object: ExternalContextObject) -> Result<(), String> {
        self.with_client(|client| upsert_object(client, &object))
    }

    fn load_external_context_object(
        &self,
        object_id: &str,
    ) -> Result<Option<ExternalContextObject>, String> {
        if object_id.trim().is_empty() {
            return Err("external context object_id is required".to_string());
        }
        self.with_client(|client| client.query_opt("SELECT schema_version,object_id,object_kind,source_provider_id,source_tool_name,title,content,metadata_json,updated_at_ms FROM external_context_objects WHERE object_id=$1", &[&object_id]).map_err(|error| format!("load Postgres external context object failed: {error}"))?.map(|row| row_to_object(&row)).transpose())
    }

    fn link_external_context_object(&self, link: ExternalContextObjectLink) -> Result<(), String> {
        self.with_client(|client| link_object(client, &link))
    }

    fn load_external_context_object_link(
        &self,
        session_id: &str,
        object_id: &str,
        turn_id: &str,
        tool_call_id: &str,
    ) -> Result<Option<ExternalContextObjectLink>, String> {
        if [session_id, object_id, turn_id, tool_call_id]
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return Err("external context link identity is required".to_string());
        }
        self.with_client(|client| {
            client
                .query_opt(
                    "SELECT session_id,turn_id,tool_call_id,object_id,source_provider_id,source_tool_name,linked_at_ms FROM external_context_links WHERE session_id=$1 AND object_id=$2 AND turn_id=$3 AND tool_call_id=$4",
                    &[&session_id, &object_id, &turn_id, &tool_call_id],
                )
                .map(|row| {
                    row.map(|row| ExternalContextObjectLink {
                        session_id: row.get(0),
                        turn_id: Some(row.get(1)),
                        tool_call_id: Some(row.get(2)),
                        object_id: row.get(3),
                        source_provider_id: row.get(4),
                        source_tool_name: row.get(5),
                        linked_at_ms: row.get(6),
                    })
                })
                .map_err(|error| format!("load Postgres external context object link failed: {error}"))
        })
    }

    fn list_external_context_objects(
        &self,
        req: ListExternalContextObjectsRequest,
    ) -> Result<Vec<ExternalContextObjectIndexEntry>, String> {
        let limit = to_i64(req.limit.clamp(1, 128))?;
        let offset = to_i64(req.offset)?;
        self.with_client(|client| {
            let result = if let Some(session_id) = req.session_id.as_deref() {
                client.query("SELECT obj.object_id,obj.object_kind,obj.source_provider_id,obj.source_tool_name,obj.title,obj.updated_at_ms,COUNT(link.object_id)::bigint,MAX(link.linked_at_ms) FROM external_context_links link JOIN external_context_objects obj ON obj.object_id=link.object_id WHERE link.session_id=$1 GROUP BY obj.object_id,obj.object_kind,obj.source_provider_id,obj.source_tool_name,obj.title,obj.updated_at_ms ORDER BY MAX(link.linked_at_ms) DESC,obj.object_id ASC LIMIT $2 OFFSET $3", &[&session_id,&limit,&offset])
            } else {
                client.query("SELECT obj.object_id,obj.object_kind,obj.source_provider_id,obj.source_tool_name,obj.title,obj.updated_at_ms,COUNT(link.object_id)::bigint,MAX(link.linked_at_ms) FROM external_context_objects obj LEFT JOIN external_context_links link ON link.object_id=obj.object_id GROUP BY obj.object_id,obj.object_kind,obj.source_provider_id,obj.source_tool_name,obj.title,obj.updated_at_ms ORDER BY obj.updated_at_ms DESC,obj.object_id ASC LIMIT $1 OFFSET $2", &[&limit,&offset])
            }.map_err(|error| format!("list Postgres external context objects failed: {error}"))?;
            result.iter().map(row_to_index).collect()
        })
    }
}

pub(super) fn upsert_object<C: GenericClient>(
    client: &mut C,
    object: &ExternalContextObject,
) -> Result<(), String> {
    let metadata = serde_json::to_string(&object.metadata)
        .map_err(|error| format!("serialize external context metadata failed: {error}"))?;
    client.execute("INSERT INTO external_context_objects(object_id,schema_version,object_kind,source_provider_id,source_tool_name,title,content,metadata_json,updated_at_ms,inserted_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9) ON CONFLICT(object_id) DO UPDATE SET schema_version=excluded.schema_version,object_kind=excluded.object_kind,source_provider_id=excluded.source_provider_id,source_tool_name=excluded.source_tool_name,title=excluded.title,content=excluded.content,metadata_json=excluded.metadata_json,updated_at_ms=excluded.updated_at_ms", &[&object.object_id,&object.schema_version,&object.object_kind,&object.source_provider_id,&object.source_tool_name,&object.title,&object.content,&metadata,&object.updated_at_ms]).map(|_| ()).map_err(|error| format!("upsert Postgres external context object failed: {error}"))
}

pub(super) fn link_object<C: GenericClient>(
    client: &mut C,
    link: &ExternalContextObjectLink,
) -> Result<(), String> {
    if link.session_id.trim().is_empty() || link.object_id.trim().is_empty() {
        return Err("external context link session/object is required".to_string());
    }
    if client
        .query_opt(
            "SELECT 1 FROM external_context_objects WHERE object_id=$1",
            &[&link.object_id],
        )
        .map_err(|error| format!("check Postgres external context target failed: {error}"))?
        .is_none()
    {
        return Err(format!(
            "external context object not found for link: {}",
            link.object_id
        ));
    }
    client.execute("INSERT INTO external_context_links(session_id,object_id,turn_id,tool_call_id,source_provider_id,source_tool_name,linked_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(session_id,object_id,turn_id,tool_call_id) DO UPDATE SET source_provider_id=excluded.source_provider_id,source_tool_name=excluded.source_tool_name,linked_at_ms=excluded.linked_at_ms", &[&link.session_id,&link.object_id,&link.turn_id.clone().unwrap_or_default(),&link.tool_call_id.clone().unwrap_or_default(),&link.source_provider_id,&link.source_tool_name,&link.linked_at_ms]).map(|_| ()).map_err(|error| format!("link Postgres external context object failed: {error}"))
}

fn row_to_object(row: &Row) -> Result<ExternalContextObject, String> {
    Ok(ExternalContextObject {
        schema_version: row.get(0),
        object_id: row.get(1),
        object_kind: row.get(2),
        source_provider_id: row.get(3),
        source_tool_name: row.get(4),
        title: row.get(5),
        content: row.get(6),
        metadata: serde_json::from_str(row.get::<_, String>(7).as_str())
            .map_err(|error| format!("decode external metadata failed: {error}"))?,
        updated_at_ms: row.get(8),
    })
}
fn row_to_index(row: &Row) -> Result<ExternalContextObjectIndexEntry, String> {
    let count: i64 = row.get(6);
    Ok(ExternalContextObjectIndexEntry {
        object_id: row.get(0),
        object_kind: row.get(1),
        source_provider_id: row.get(2),
        source_tool_name: row.get(3),
        title: row.get(4),
        updated_at_ms: row.get(5),
        link_count: usize::try_from(count)
            .map_err(|_| "external link count overflow".to_string())?,
        last_linked_at_ms: row.get(7),
    })
}
