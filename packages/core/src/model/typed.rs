//! Finite native typed-model questions, wire construction, and answer validation.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::JsonHttpRequest;

/// A native finite question. Unsupported question types and fields are rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum TypedQuestion {
    /// A selection from the named finite criteria.
    Choice {
        instructions: Value,
        criteria: BTreeMap<String, String>,
    },
    /// A probability of the positive criterion, between zero and one.
    Noul {
        instructions: Value,
        criteria: BTreeMap<String, String>,
    },
}

/// A complete native typed-model HTTP payload, separate from conversation turns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypedModelRequest {
    pub model: String,
    pub state: Value,
    pub questions: BTreeMap<String, TypedQuestion>,
}

/// A finite answer whose value is checked against its corresponding question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum TypedAnswer {
    Choice {
        choice: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        probabilities: Option<BTreeMap<String, f64>>,
    },
    Noul {
        noul: f64,
    },
}

/// The validated native response, retaining the provider's raw usage object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypedModelResponse {
    pub model: String,
    pub answers: BTreeMap<String, TypedAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
}

/// An unsupported finite schema, invalid answer, or invalid native wire value.
#[derive(Debug, thiserror::Error)]
#[error("invalid typed-model contract: {0}")]
pub struct TypedModelError(String);

fn invalid(message: impl Into<String>) -> TypedModelError {
    TypedModelError(message.into())
}

impl TypedModelRequest {
    /// Checks finite criteria before any external request can be dispatched.
    ///
    /// # Errors
    /// Rejects empty identities, unsupported criteria, and invalid instructions.
    pub fn validate(&self) -> Result<(), TypedModelError> {
        if self.model.trim().is_empty() || self.questions.is_empty() {
            return Err(invalid("model and at least one question are required"));
        }
        for (id, question) in &self.questions {
            let (instructions, criteria) = match question {
                TypedQuestion::Choice {
                    instructions,
                    criteria,
                }
                | TypedQuestion::Noul {
                    instructions,
                    criteria,
                } => (instructions, criteria),
            };
            if id.trim().is_empty()
                || criteria.is_empty()
                || criteria
                    .iter()
                    .any(|(key, value)| key.trim().is_empty() || value.trim().is_empty())
                || !(instructions.is_object()
                    || instructions.as_str().is_some_and(|s| !s.trim().is_empty()))
            {
                return Err(invalid(format!(
                    "question {id} requires instructions and finite named criteria"
                )));
            }
            if matches!(question, TypedQuestion::Noul { .. })
                && (criteria.len() != 2
                    || !criteria.contains_key("true")
                    || !criteria.contains_key("false"))
            {
                return Err(invalid(format!(
                    "noul question {id} requires exactly true and false criteria"
                )));
            }
        }
        Ok(())
    }

    /// Constructs one native Jev request without a chat protocol or tool loop.
    ///
    /// # Errors
    /// Returns an error if the finite question contract or URL is invalid.
    pub fn http_request(
        &self,
        url: &str,
        key: &str,
        request_id: &str,
    ) -> Result<JsonHttpRequest, TypedModelError> {
        self.validate()?;
        let parsed = url::Url::parse(url).map_err(|_| invalid("invalid endpoint URL"))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(invalid(
                "endpoint must be an HTTP URL without embedded credentials",
            ));
        }
        let mut headers = HashMap::from([
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("X-Request-Id".to_owned(), request_id.to_owned()),
        ]);
        if !key.is_empty() {
            headers.insert("Authorization".to_owned(), format!("Bearer {key}"));
        }
        Ok(JsonHttpRequest {
            method: "POST".to_owned(),
            url: url.to_owned(),
            headers,
            timeout_ms: 180_000,
            sse_idle_timeout_ms: 180_000,
            max_retries: 0,
            retry_backoff_ms: 0,
            body_json: serde_json::to_string(self)
                .map_err(|e| invalid(e.to_string()))?
                .into(),
        })
    }

    /// Decodes native answers and checks every identifier, type, and finite value.
    ///
    /// # Errors
    /// Rejects extra/missing answers, unsupported fields, and out-of-domain values.
    pub fn decode_response(&self, body: &str) -> Result<TypedModelResponse, TypedModelError> {
        self.validate()?;
        let response: TypedModelResponse =
            serde_json::from_str(body).map_err(|e| invalid(e.to_string()))?;
        if response.model != self.model || response.answers.len() != self.questions.len() {
            return Err(invalid(
                "response model or answer set does not match request",
            ));
        }
        for (id, question) in &self.questions {
            match (question, response.answers.get(id)) {
                (
                    TypedQuestion::Choice { criteria, .. },
                    Some(TypedAnswer::Choice {
                        choice,
                        confidence,
                        probabilities,
                    }),
                ) => {
                    if !criteria.contains_key(choice)
                        || confidence.is_some_and(|p| !probability(p))
                        || probabilities.as_ref().is_some_and(|values| {
                            values.len() != criteria.len()
                                || values.iter().any(|(key, value)| {
                                    !criteria.contains_key(key) || !probability(*value)
                                })
                        })
                    {
                        return Err(invalid(format!("invalid choice answer for {id}")));
                    }
                }
                (TypedQuestion::Noul { .. }, Some(TypedAnswer::Noul { noul }))
                    if probability(*noul) => {}
                _ => return Err(invalid(format!("missing or incompatible answer for {id}"))),
            }
        }
        Ok(response)
    }
}

fn probability(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> TypedModelRequest {
        serde_json::from_value(json!({"model":"jev", "state":{"evidence":"read-only"}, "questions":{
            "relation":{"type":"choice","instructions":"Compare with task", "criteria":{"withinTask":"within","outsideTask":"outside"}},
            "support":{"type":"noul","instructions":{"question":"Supports?"},"criteria":{"true":"yes","false":"no"}}
        }})).unwrap()
    }

    #[test]
    fn native_typed_contract_validates_wire_and_answers() {
        let request = request();
        let wire = request
            .http_request("http://127.0.0.1/evaluate", "secret", "attempt-1")
            .unwrap();
        assert_eq!(wire.max_retries, 0);
        assert_eq!(
            serde_json::from_str::<Value>(&wire.body_json).unwrap(),
            serde_json::to_value(&request).unwrap()
        );
        let body = json!({"model":"jev","answers":{"relation":{"type":"choice","choice":"withinTask","confidence":0.9,"probabilities":{"withinTask":0.9,"outsideTask":0.1}},"support":{"type":"noul","noul":0.8}},"usage":{"input_tokens":7}});
        assert_eq!(
            request.decode_response(&body.to_string()).unwrap().usage,
            Some(json!({"input_tokens":7}))
        );
        for bad in [
            json!({"model":"jev","answers":{"relation":{"type":"choice","choice":"invented"},"support":{"type":"noul","noul":0.8}}}),
            json!({"model":"jev","answers":{"relation":{"type":"choice","choice":"withinTask"},"support":{"type":"noul","noul":1.1}}}),
            json!({"model":"jev","answers":{"relation":{"type":"choice","choice":"withinTask","text":"unbounded"},"support":{"type":"noul","noul":0.8}}}),
            json!({"model":"jev","answers":{"relation":{"type":"choice","choice":"withinTask"}}}),
        ] {
            assert!(request.decode_response(&bad.to_string()).is_err(), "{bad}");
        }
    }

    #[test]
    fn typed_contract_rejects_unknown_schemas_and_fields() {
        for question in [
            json!({"type":"score","instructions":"score","criteria":{"a":"A"}}),
            json!({"type":"choice","instructions":"choose","criteria":{"a":"A"},"old_name":true}),
        ] {
            assert!(serde_json::from_value::<TypedQuestion>(question).is_err());
        }
        let mut request = request();
        request.questions.insert(
            "invalid".into(),
            TypedQuestion::Noul {
                instructions: json!("score"),
                criteria: BTreeMap::from([("maybe".into(), "Maybe".into())]),
            },
        );
        assert!(request.validate().is_err());
    }
}
