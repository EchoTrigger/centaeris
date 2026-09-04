use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let value = serde_json::to_value(value)
        .map_err(|error| format!("encode canonical JSON value failed: {error}"))?;
    serde_json::to_vec(&sort_value(value))
        .map_err(|error| format!("encode canonical JSON failed: {error}"))
}

pub fn sha256<T: Serialize>(domain: &str, value: &T) -> Result<String, String> {
    let bytes = to_vec(value)?;
    let mut digest = Sha256::new();
    digest.update(domain.as_bytes());
    digest.update([0]);
    digest.update(bytes);
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn sort_value(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(sort_value).collect()),
        Value::Object(items) => {
            let mut entries = items.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, sort_value(value)))
                    .collect(),
            )
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_object_keys_without_rewriting_strings() {
        let left = json!({"b": " value\r\n", "a": {"d": 2, "c": 1}});
        let right = json!({"a": {"c": 1, "d": 2}, "b": " value\r\n"});
        assert_eq!(to_vec(&left).expect("left"), to_vec(&right).expect("right"));
        assert_eq!(
            String::from_utf8(to_vec(&left).expect("json")).expect("utf8"),
            r#"{"a":{"c":1,"d":2},"b":" value\r\n"}"#
        );
    }
}
