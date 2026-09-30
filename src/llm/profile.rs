//! What a provider was seen to do. OpenAI-compatible endpoints differ in
//! behaviour, not only in syntax: one ignores `parallel_tool_calls: false`,
//! another writes calls as text, a router serves each request from a
//! different upstream. A [`ProviderProfile`] records those facts from neutral
//! probes before work starts. Callers choose their policy from it, and let
//! later responses correct it (a provider may route the next request
//! elsewhere), rather than keeping a table of named providers.
//!
//! This module depends only on the client and the normalizer, so the layer
//! can stand alone.

use super::normalize::{normalize, CallSource};
use super::{CompletionError, OpenAiCompatClient};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Provider behaviour observed by probes. Every field is optional: absent
/// means not probed or inconclusive, never "fine".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderProfile {
    /// A request for one call (`tool_choice: "required"`,
    /// `parallel_tool_calls: false`) that invited two came back with more
    /// than one. `Some(false)` is weaker evidence: this probe got one call,
    /// which the model may simply have chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiple_calls_seen: Option<bool>,
    /// How the probe's call arrived: `native` tool calls, or the name of the
    /// text dialect the normalizer read it in (`hermes`, `json`, `gemma`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_dialect: Option<String>,
    /// The upstream providers a routing endpoint named, in the order first
    /// seen. More than one means requests are served by different backends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub served_by: Vec<String>,
    /// Wall-clock milliseconds of the passing non-streaming and streaming probes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonstream_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_ms: Option<u64>,
}

/// The name a call's source goes by in a profile.
pub fn call_dialect(source: CallSource) -> &'static str {
    match source {
        CallSource::Native => "native",
        CallSource::Text { dialect } => dialect,
    }
}

const FIRST: &str = "first_check";
const SECOND: &str = "second_check";

/// Ask for two calls in one response while the request allows only one, and
/// count the calls that come back: more than one means the provider does not
/// enforce single calls. Uses whatever output limit and request allowance the
/// client carries.
pub async fn probe_multiple_calls(
    client: &OpenAiCompatClient,
    model: &str,
) -> Result<bool, CompletionError> {
    const PROMPT: &str = "This is a neutral connection test. Call first_check once and second_check once, in that order.";
    let tool = |name: &str| {
        json!({"type":"function","function":{
            "name": name,
            "description": "A connection check.",
            "parameters": {"type":"object","additionalProperties":false,"properties":{}}
        }})
    };
    let completion = client
        .chat_completion_tools_checked(
            model,
            PROMPT,
            json!([tool(FIRST), tool(SECOND)]),
            false,
            |_| {},
        )
        .await?;
    let calls = normalize(&completion)
        .calls
        .into_iter()
        .filter(|call| call.name == FIRST || call.name == SECOND)
        .count();
    if calls == 0 {
        return Err(CompletionError::InvalidResponse(
            "Multiple-call probe returned no check call".into(),
        ));
    }
    Ok(calls > 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Answer(Arc<Mutex<(serde_json::Value, Vec<serde_json::Value>)>>);

    async fn complete(
        State(answer): State<Answer>,
        Json(body): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        let mut state = answer.0.lock().unwrap();
        state.1.push(body);
        Json(state.0.clone())
    }

    async fn serve(answer: serde_json::Value) -> (OpenAiCompatClient, Answer) {
        let answer = Answer(Arc::new(Mutex::new((answer, vec![]))));
        let app = Router::new()
            .route("/v1/chat/completions", post(complete))
            .with_state(answer.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            OpenAiCompatClient::new(format!("http://{address}/v1"), "test"),
            answer,
        )
    }

    fn calls(names: &[&str]) -> serde_json::Value {
        let calls: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(index, name)| json!({"id": format!("c{index}"), "type":"function","function":{"name": name, "arguments":"{}"}}))
            .collect();
        json!({"provider":"Fixture","choices":[{"message":{"role":"assistant","content":"","tool_calls":calls},"finish_reason":"tool_calls"}]})
    }

    #[tokio::test]
    async fn two_calls_to_a_single_call_request_are_seen() {
        let (client, answer) = serve(calls(&[FIRST, SECOND])).await;
        assert!(probe_multiple_calls(&client, "model").await.unwrap());
        let requests = &answer.0.lock().unwrap().1;
        assert_eq!(requests[0]["tool_choice"], "required");
        assert_eq!(requests[0]["parallel_tool_calls"], false);
        assert_eq!(requests[0]["tools"].as_array().unwrap().len(), 2);
        assert_eq!(client.served_by(), ["Fixture"]);
    }

    #[tokio::test]
    async fn one_call_is_not_seen_and_no_call_is_inconclusive() {
        let (client, _) = serve(calls(&[FIRST])).await;
        assert!(!probe_multiple_calls(&client, "model").await.unwrap());
        let (client, _) = serve(calls(&["other"])).await;
        assert!(probe_multiple_calls(&client, "model").await.is_err());
    }

    #[test]
    fn a_profile_omits_what_was_not_probed() {
        assert_eq!(
            serde_json::to_value(ProviderProfile::default()).unwrap(),
            json!({})
        );
        assert_eq!(call_dialect(CallSource::Native), "native");
        assert_eq!(
            call_dialect(CallSource::Text { dialect: "hermes" }),
            "hermes"
        );
    }
}
