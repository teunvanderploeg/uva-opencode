use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Mutex};
use uva_opencode::{
    error::{ErrorKind, Result},
    transport::{self, ChatRequest, ToolMode},
    upstream::{self, Event, Upstream},
};

#[derive(Default)]
pub struct Fake {
    responses: Mutex<VecDeque<Vec<Event>>>,
    pub requests: Mutex<Vec<Value>>,
}
impl Fake {
    pub fn new(texts: &[&str]) -> Self {
        Self {
            responses: Mutex::new(
                texts
                    .iter()
                    .map(|text| {
                        upstream::parse_events(&format!(
                            "data: {}\n\n",
                            json!({"type":"text-delta","delta":text})
                        ))
                        .unwrap()
                    })
                    .collect(),
            ),
            ..Self::default()
        }
    }
}
impl Upstream for Fake {
    async fn request(&self, payload: Value, _: &str) -> Result<Vec<Event>> {
        self.requests.lock().unwrap().push(payload);
        Ok(self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected upstream request"))
    }
}
pub fn request() -> ChatRequest {
    serde_json::from_value(json!({"messages":[{"role":"user","content":"test"}],"tools":[{"type":"function","function":{"name":"read","parameters":{}}}]})).unwrap()
}
async fn respond(value: Value, body: ChatRequest) -> Result<transport::Completion> {
    transport::completion(&body, ToolMode::Prompt, &Fake::new(&[&value.to_string()])).await
}

#[tokio::test]
async fn unknown_tools_are_rejected_without_repair() {
    let error = respond(
        json!({"content":null,"tool_calls":[{"name":"shell","arguments":{}}]}),
        request(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ErrorKind::Transport);
    assert!(error.message.contains("unknown tool"));
}
#[tokio::test]
async fn malformed_and_string_arguments_get_exactly_one_repair() {
    for raw in [
        "plain text",
        r#"{"content":null,"tool_calls":[{"name":"read","arguments":"not an object"}]}"#,
    ] {
        let fake = Fake::new(&[raw, raw]);
        let error = transport::completion(&request(), ToolMode::Prompt, &fake)
            .await
            .err()
            .unwrap();
        assert!(error.message.contains("one repair attempt"));
        assert_eq!(fake.requests.lock().unwrap().len(), 2);
    }
}
#[tokio::test]
async fn tool_choice_none_required_and_selected_are_enforced() {
    let mut body = request();
    body.tool_choice = json!("none");
    assert!(
        respond(
            json!({"content":null,"tool_calls":[{"name":"read","arguments":{}}]}),
            body
        )
        .await
        .err()
        .unwrap()
        .message
        .contains("tool_choice was none")
    );
    for choice in [
        json!("required"),
        json!({"type":"function","function":{"name":"read"}}),
    ] {
        let mut body = request();
        body.tool_choice = choice;
        assert_eq!(
            respond(json!({"content":"done","tool_calls":[]}), body)
                .await
                .err()
                .unwrap()
                .kind,
            ErrorKind::Transport
        );
    }
}
#[tokio::test]
async fn json_fence_with_explanation_is_accepted_without_repair() {
    let raw = "I will inspect the file first.\n```json\n{\"content\":null,\"tool_calls\":[{\"name\":\"read\",\"arguments\":{\"path\":\"index.html\"}}]}\n```";
    let fake = Fake::new(&[raw]);
    let result = transport::completion(&request(), ToolMode::Prompt, &fake)
        .await
        .unwrap();
    assert_eq!(
        result.response["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "read"
    );
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn html_is_repaired_before_exposing_a_tool_call() {
    let fake = Fake::new(&[
        "<!doctype html><html>plain HTML</html>",
        r#"{"content":null,"tool_calls":[{"name":"read","arguments":{"path":"index.html"}}]}"#,
    ]);
    let result = transport::completion(&request(), ToolMode::Prompt, &fake)
        .await
        .unwrap();
    assert_eq!(result.stats["transport_repaired"], true);
    let requests = fake.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]["systemPrompt"]
            .as_str()
            .unwrap()
            .contains("JSON envelope is mandatory")
    );
    assert!(
        requests[0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != "system")
    );
    assert!(
        requests[1]["messages"][2]["parts"][0]["text"]
            .as_str()
            .unwrap()
            .contains("No tools from your previous response have executed")
    );
}
#[test]
fn html_strings_and_unicode_preserve_their_contents() {
    let html =
        "<html>\n<style>body { color: red; }</style>\n<div class=\"hero\">UvA 🦀</div></html>";
    let raw = json!({"content":null,"tool_calls":[{"name":"write","arguments":{"content":html}}]});
    let envelope = transport::transport_object(&raw.to_string()).unwrap();
    assert_eq!(envelope.tool_calls[0].arguments["content"], html);
}
#[test]
fn embedded_html_objects_missing_keys_and_empty_objects_are_rejected() {
    for raw in [
        r#"<html><script>{"content":null,"tool_calls":[{"name":"read","arguments":{}}]}</script></html>"#,
        "<html><script>```json\n{\"content\":null,\"tool_calls\":[{\"name\":\"read\",\"arguments\":{}}]}\n```</script></html>",
        "{}",
        r#"{"tool_calls":[]}"#,
        r#"{"content":"ok","tool_calls":[],"extra":1}"#,
        r#"{"content":null,"tool_calls":[]}"#,
    ] {
        assert!(transport::transport_object(raw).is_err(), "accepted {raw}");
    }
}
#[tokio::test]
async fn truncated_and_large_outputs_are_never_repaired() {
    let fake = Fake::new(&["{\"content\":"]);
    fake.responses
        .lock()
        .unwrap()
        .front_mut()
        .unwrap()
        .push(serde_json::from_value(json!({"type":"finish","finishReason":"length"})).unwrap());
    assert!(
        transport::completion(&request(), ToolMode::Prompt, &fake)
            .await
            .err()
            .unwrap()
            .message
            .contains("truncated")
    );
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
    let raw = "x".repeat(100001);
    let fake = Fake::new(&[&raw]);
    assert!(
        transport::completion(&request(), ToolMode::Prompt, &fake)
            .await
            .err()
            .unwrap()
            .message
            .contains("too large")
    );
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn native_tool_history_is_rejected_and_server_tools_cannot_escape() {
    let mut body = request();
    body.uva_tool_mode = Some(ToolMode::Native);
    body.messages = vec![json!({"role":"tool","tool_call_id":"x","content":"private value"})];
    assert!(
        transport::completion(&body, ToolMode::Prompt, &Fake::default())
            .await
            .err()
            .unwrap()
            .message
            .contains("Native tool history")
    );
    body.messages = vec![json!({"role":"user","content":"test"})];
    let fake = Fake::default();
    fake.responses.lock().unwrap().push_back(vec![
        serde_json::from_value(
            json!({"type":"tool-input-available","toolName":"server_search","toolCallId":"x"}),
        )
        .unwrap(),
    ]);
    assert!(
        transport::completion(&body, ToolMode::Prompt, &fake)
            .await
            .err()
            .unwrap()
            .message
            .contains("server tool")
    );
}
#[tokio::test]
async fn images_are_rejected_in_prompt_mode_before_any_network_request() {
    let mut body = request();
    body.messages =
        vec![json!({"role":"user","content":[{"type":"image_url","image_url":{"url":"private"}}]})];
    assert_eq!(
        transport::completion(&body, ToolMode::Prompt, &Fake::default())
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::BadRequest
    );
}
#[tokio::test]
async fn upstream_error_events_are_never_repaired() {
    let fake = Fake::default();
    fake.responses.lock().unwrap().push_back(
        upstream::parse_events("data: {\"type\":\"error\",\"message\":\"private\"}\n\n").unwrap(),
    );
    assert_eq!(
        transport::completion(&request(), ToolMode::Prompt, &fake)
            .await
            .err()
            .unwrap()
            .message,
        "UvA returned an error event."
    );
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn compact_history_preserves_content_ids_and_invalid_arguments() {
    let code = "const greeting = \"UvA 🦀\";\n".repeat(100);
    let arguments = json!({"path":"main.js","content":code});
    let mut body = request();
    body.messages = vec![
        json!({"role":"system","content":"Keep the instructions."}),
        json!({"role":"user","content":[{"type":"text","text":"First"},{"type":"text","text":"Second"}]}),
        json!({"role":"assistant","content":null,"tool_calls":[
            {"id":"call_original","type":"function","function":{"name":"write","arguments":arguments.to_string()}},
            {"id":"call_invalid","type":"function","function":{"name":"read","arguments":"{broken"}}
        ]}),
        json!({"role":"tool","tool_call_id":"call_original","content":"Written"}),
    ];
    let original = json!(body.messages).to_string();
    let fake = Fake::new(&[r#"{"content":"done","tool_calls":[]}"#]);
    transport::completion(&body, ToolMode::Prompt, &fake)
        .await
        .unwrap();
    let requests = fake.requests.lock().unwrap();
    let text = requests[0]["messages"][0]["parts"][0]["text"]
        .as_str()
        .unwrap();
    let compact = text.strip_prefix("Client conversation:\n").unwrap();
    assert!(compact.len() < original.len());
    let messages: Value = serde_json::from_str(compact).unwrap();
    assert_eq!(messages[0], body.messages[0]);
    assert_eq!(messages[1]["content"], "First\nSecond");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["arguments"],
        arguments
    );
    assert_eq!(messages[2]["tool_calls"][0]["id"], "call_original");
    assert_eq!(
        messages[2]["tool_calls"][1]["function"]["arguments"],
        "{broken"
    );
    assert_eq!(messages[3], body.messages[3]);
}

#[tokio::test]
async fn usage_falls_back_to_an_estimate_and_prefers_provider_counts() {
    let fake = Fake::new(&[r#"{"content":"hello","tool_calls":[]}"#]);
    let result = transport::completion(&request(), ToolMode::Prompt, &fake)
        .await
        .unwrap();
    assert_eq!(result.stats["usage_source"], "estimated");
    let usage = &result.response["usage"];
    assert!(usage["prompt_tokens"].as_u64().unwrap() > 0);
    assert!(usage["completion_tokens"].as_u64().unwrap() > 0);
    assert_eq!(
        usage["total_tokens"].as_u64().unwrap(),
        usage["prompt_tokens"].as_u64().unwrap() + usage["completion_tokens"].as_u64().unwrap()
    );
    for key in ["usage", "totalUsage"] {
        let fake = Fake::new(&[r#"{"content":"hello","tool_calls":[]}"#]);
        fake.responses.lock().unwrap().front_mut().unwrap().push(
            serde_json::from_value(
                json!({"type":"finish",key:{"inputTokens":1234,"outputTokens":56}}),
            )
            .unwrap(),
        );
        let result = transport::completion(&request(), ToolMode::Prompt, &fake)
            .await
            .unwrap();
        assert_eq!(result.stats["usage_source"], "upstream");
        assert_eq!(
            result.response["usage"],
            json!({"prompt_tokens":1234,"completion_tokens":56,"total_tokens":1290})
        );
    }
}

#[tokio::test]
async fn repair_usage_accounts_for_the_expanded_context() {
    let valid = r#"{"content":"hello","tool_calls":[]}"#;
    let fake = Fake::new(&[valid]);
    let initial = transport::completion(&request(), ToolMode::Prompt, &fake)
        .await
        .unwrap();
    let fake = Fake::new(&["invalid response", valid]);
    let repaired = transport::completion(&request(), ToolMode::Prompt, &fake)
        .await
        .unwrap();
    assert!(
        repaired.response["usage"]["prompt_tokens"]
            .as_u64()
            .unwrap()
            > initial.response["usage"]["prompt_tokens"].as_u64().unwrap()
    );
    assert_eq!(repaired.stats["transport_repaired"], true);
}

#[tokio::test]
async fn diagnostic_breakdown_separates_context_without_recording_text() {
    let mut body = request();
    body.messages = vec![
        json!({"role":"system","content":"PRIVATE_SYSTEM"}),
        json!({"role":"developer","content":"PRIVATE_POLICY"}),
        json!({"role":"user","content":"PRIVATE_TASK"}),
        json!({"role":"assistant","content":null,"tool_calls":[{"id":"original","type":"function","function":{"name":"read","arguments":"{\"path\":\"private.txt\"}"}}]}),
        json!({"role":"tool","tool_call_id":"original","content":"PRIVATE_RESULT"}),
    ];
    let valid = r#"{"content":"done","tool_calls":[]}"#;
    let first = transport::completion(&body, ToolMode::Prompt, &Fake::new(&[valid]))
        .await
        .unwrap();
    let breakdown = first.stats["input_breakdown_estimated"]
        .as_object()
        .unwrap();
    assert_eq!(breakdown.len(), 5);
    assert!(breakdown.values().all(|v| v.as_u64().unwrap() > 0));
    assert!(!first.stats.to_string().contains("PRIVATE_"));
    assert!(!first.stats.to_string().contains("private.txt"));
    assert_eq!(first.stats["upstream_requests"], 1);
    assert_eq!(
        first.stats["initial_input_estimated_tokens"],
        first.response["usage"]["prompt_tokens"]
    );

    // Only the tool-results bucket should grow when a result grows.
    body.messages[4]["content"] = json!("🦀".repeat(3000));
    let larger = transport::completion(&body, ToolMode::Prompt, &Fake::new(&[valid]))
        .await
        .unwrap();
    for key in [
        "adapter_instructions",
        "tool_definitions",
        "client_instructions",
        "conversation",
    ] {
        assert_eq!(
            first.stats["input_breakdown_estimated"][key],
            larger.stats["input_breakdown_estimated"][key]
        );
    }
    assert!(
        larger.stats["input_breakdown_estimated"]["tool_results"]
            .as_u64()
            .unwrap()
            > breakdown["tool_results"].as_u64().unwrap()
    );
    let repaired = transport::completion(&body, ToolMode::Prompt, &Fake::new(&["invalid", valid]))
        .await
        .unwrap();
    assert_eq!(repaired.stats["upstream_requests"], 2);
    assert_eq!(
        repaired.stats["input_breakdown_estimated"],
        larger.stats["input_breakdown_estimated"]
    );
    assert_eq!(
        repaired.stats["initial_input_estimated_tokens"],
        larger.stats["initial_input_estimated_tokens"]
    );
}

#[tokio::test]
async fn multiple_read_requests_preserve_each_call_and_argument() {
    let fake = Fake::new(&[
        r#"{"content":null,"tool_calls":[{"name":"read","arguments":{"path":"a.rs","offset":10,"limit":20}},{"name":"read","arguments":{"path":"b.rs","offset":30,"limit":40}}]}"#,
    ]);
    let result = transport::completion(&request(), ToolMode::Prompt, &fake)
        .await
        .unwrap();
    let calls = result.response["choices"][0]["message"]["tool_calls"]
        .as_array()
        .unwrap();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0]["id"], calls[1]["id"]);
    for (call, expected) in calls.iter().zip([
        json!({"path":"a.rs","offset":10,"limit":20}),
        json!({"path":"b.rs","offset":30,"limit":40}),
    ]) {
        assert_eq!(
            serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap()).unwrap(),
            expected
        );
    }
    assert_eq!(fake.requests.lock().unwrap().len(), 1);
}
