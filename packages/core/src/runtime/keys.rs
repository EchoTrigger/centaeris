pub mod external_context {
    pub const SUBAGENT_WORK_PACKET_PREFIX: &str = "external_context:subagent_work_packet:";
    pub const SUBAGENT_RESULT_PREFIX: &str = "external_context:subagent_result:";

    pub fn subagent_work_packet_ref(id: &str) -> String {
        format!("{SUBAGENT_WORK_PACKET_PREFIX}{id}")
    }

    pub fn subagent_result_ref(id: &str) -> String {
        format!("{SUBAGENT_RESULT_PREFIX}{id}")
    }
}

pub mod metadata {
    pub const SYSTEM_PROMPT_MANIFEST: &str = "system_prompt_manifest_json";
    pub const SUBAGENT_RESULT_PROJECTION: &str = "subagent_result_projection_v1_json";
    pub const MESSAGE_SEMANTIC_KIND: &str = "message_semantic_kind";
    pub const MODEL_INPUT_IMAGES: &str = "model_input_images_v1_json";
    pub const MODEL_INPUT_IMAGE_SOURCES: &str = "model_input_image_sources_v1_json";
    pub const ACTIVE_OBJECTIVE: &str = "active_objective_v1_json";
    pub const CONTEXT_WINDOW_MATERIALIZATION: &str = "context_window_materialization_v1_json";
    pub const PROMPT_COMPACTION_FAILURE: &str = "prompt_compaction_failure_v1_json";
    pub const PROMPT_COMPACTION_FAILURE_COUNT: &str = "prompt_compaction_failure_count";
    pub const PROMPT_COMPACTION_CIRCUIT: &str = "prompt_compaction_circuit_v1_json";
}

pub mod runtime_job {
    pub const SUBAGENT_RUN: &str = "subagent.run";
    pub const SUBAGENT_RUN_PREFIX: &str = "subagent.run:";

    pub fn subagent_run_job_id(stable_hash: &str) -> String {
        format!("{SUBAGENT_RUN_PREFIX}{stable_hash}")
    }

    pub fn subagent_run_idempotency_key(stable_key: &str) -> String {
        format!("{SUBAGENT_RUN_PREFIX}{stable_key}")
    }
}

#[cfg(test)]
mod tests {
    use super::{external_context, metadata, runtime_job};

    #[test]
    fn registry_keeps_stable_protocol_keys() {
        assert_eq!(
            metadata::CONTEXT_WINDOW_MATERIALIZATION,
            "context_window_materialization_v1_json"
        );
        assert_eq!(runtime_job::SUBAGENT_RUN, "subagent.run");
        assert_eq!(
            external_context::subagent_work_packet_ref("abc"),
            "external_context:subagent_work_packet:abc"
        );
        assert_eq!(
            external_context::subagent_result_ref("abc"),
            "external_context:subagent_result:abc"
        );
    }
}
