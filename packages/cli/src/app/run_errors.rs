use std::{collections::HashMap, sync::OnceLock};

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    messages: HashMap<String, Message>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    #[serde(rename = "translationKey")]
    _translation_key: String,
    english: String,
}

pub(super) fn detail(message: &str, reason_type: Option<&str>) -> &'static str {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    let catalog = CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!("../../../../assets/run-error-messages.json"))
            .expect("checked-in run error catalog must be valid")
    });
    reason_type
        .and_then(|reason| catalog.messages.get(reason))
        .or_else(|| catalog.messages.get(message))
        .unwrap_or_else(|| &catalog.messages["model_adapter_failed"])
        .english
        .as_str()
}
