use std::{collections::HashMap, sync::OnceLock};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Catalog {
    failure_label: String,
    messages: HashMap<String, Message>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Message {
    translation_key: String,
    english: String,
}

pub(super) fn detail(reason: &str) -> String {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    let catalog = CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../../../../assets/run-error-messages.json"))
            .expect("checked-in run error catalog must be valid")
    });
    let message = catalog.messages.get(reason);
    let coded = ["model_", "provider_", "prepared_prompt_", "completion_"]
        .iter()
        .any(|prefix| {
            reason.strip_prefix(prefix).is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
        });
    if coded {
        let label = message
            .filter(|message| message.translation_key != "modelErrors.failed")
            .map_or(catalog.failure_label.as_str(), |message| {
                message.english.as_str()
            });
        format!("{label} · {reason}")
    } else {
        message.map_or_else(|| reason.into(), |message| message.english.clone())
    }
}
