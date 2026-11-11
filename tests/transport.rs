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
