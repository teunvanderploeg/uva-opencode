use crate::{
    error::{Error, Result},
    models::DEFAULT_MODEL,
    upstream::{self, Event, Upstream},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::HashSet,
    sync::LazyLock,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ToolMode {
    Native,
    #[default]
    Prompt,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Function {
    pub name: String,
    #[serde(flatten)]
    pub definition: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: Function,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatRequest {
    #[serde(default = "default_model")]
    pub model: String,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default = "auto_choice")]
    pub tool_choice: Value,
    #[serde(default)]
    pub uva_tool_mode: Option<ToolMode>,
    #[serde(default)]
    pub stream: bool,
}
fn default_model() -> String {
    DEFAULT_MODEL.into()
}
fn auto_choice() -> Value {
    json!("auto")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    // Value makes the content key mandatory; Option<String> alone allows a missing key.
    pub content: Value,
    pub tool_calls: Vec<RequestedCall>,
}
#[derive(Debug, Deserialize)]
pub struct RequestedCall {
    pub name: String,
    pub arguments: Map<String, Value>,
}

pub fn transport_object(content: &str) -> Result<Envelope> {
    static FENCES: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)```(?:json)?[ \t]*\r?\n(.*?)\r?\n```").unwrap());
    let raw = content.trim();
    let mut candidates = vec![raw];
    let fenced: Vec<_> = FENCES.captures_iter(raw).collect();
    if fenced.len() == 1
        && !raw[..fenced[0].get(0).unwrap().start()].contains(['<', '>', '`'])
        && !raw[fenced[0].get(0).unwrap().end()..].contains(['<', '>', '`'])
    {
        candidates.push(fenced[0].get(1).unwrap().as_str().trim());
    }
    if !raw.starts_with('<')
        && !raw.starts_with("```")
        && let Some(pos) = raw.find('{').filter(|pos| *pos > 0)
        && !raw[..pos].contains(['<', '>', '`'])
    {
        candidates.push(&raw[pos..]);
    }
    // Stop at the first JSON value, matching the Python parser's behavior.
    let value = candidates
        .into_iter()
        .find_map(|candidate| serde_json::from_str::<Value>(candidate).ok())
        .ok_or_else(|| {
            Error::transport("Model did not return the required JSON transport object.")
        })?;
    let obj: Envelope = serde_json::from_value(value).map_err(|_| {
        Error::transport("Model returned an invalid JSON transport object or tool arguments.")
    })?;
    if !obj.content.is_null() && !obj.content.is_string() {
        return Err(Error::transport("Model returned non-text content."));
    }
    if obj.tool_calls.is_empty() && obj.content.as_str().is_none_or(str::is_empty) {
        return Err(Error::transport(
            "Model returned an empty transport object.",
        ));
    }
    Ok(obj)
}

fn content_text(message: &Value) -> Result<String> {
    match &message["content"] {
        Value::Null => Ok(String::new()),
        Value::String(text) => Ok(text.clone()),
        Value::Array(parts) => parts
            .iter()
            .map(|part| {
                if part["type"] != "text" {
                    return Err(Error::bad_request(
                        "This adapter accepts text messages only.",
                    ));
                }
                part["text"]
                    .as_str()
                    .map(String::from)
                    .ok_or_else(|| Error::bad_request("Text parts must contain a text string."))
            })
            .collect::<Result<Vec<_>>>()
            .map(|parts| parts.join("\n")),
        _ => Err(Error::bad_request(
            "Message content must be text, text parts, or null.",
        )),
    }
}

fn ui_message(role: &str, content: &str) -> Value {
    json!({"id":Uuid::new_v4().simple().to_string(),"role":role,"parts":[{"type":"text","text":content}],
        "createdAt":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)})
}

// Keep message boundaries and tool IDs, but avoid JSON encoded inside JSON.
// Invalid or non-object argument strings are retained verbatim.
fn normalized_messages(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            let mut message = message.clone();
            if message["content"].is_array() {
                message["content"] = json!(content_text(&message).expect("validated text content"));
            }
            if let Some(calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) {
                for call in calls {
                    if let Some(raw) = call["function"]["arguments"].as_str()
                        && let Ok(value @ Value::Object(_)) = serde_json::from_str::<Value>(raw)
                    {
                        call["function"]["arguments"] = value;
                    }
                }
            }
            message
        })
        .collect()
}

fn conversation(messages: &[Value]) -> String {
    format!(
        "Client conversation:\n{}",
        json!(normalized_messages(messages))
    )
}

// UvA does not always expose usage. A conservative text estimate lets clients
// compact context instead of treating every request as zero tokens. It is not
// a billing count and varies by model, language, and source code.
fn estimate_tokens(text: &str) -> u64 {
    text.chars().count().div_ceil(3) as u64
}

fn request_tokens(payload: &Value) -> u64 {
    let mut tokens = payload["systemPrompt"]
        .as_str()
        .map(estimate_tokens)
        .unwrap_or(0);
    if let Some(messages) = payload["messages"].as_array() {
        for message in messages {
            tokens += 4;
            if let Some(parts) = message["parts"].as_array() {
                for part in parts {
                    tokens += part["text"].as_str().map(estimate_tokens).unwrap_or(0);
                }
            }
        }
    }
    if let Some(tools) = payload.get("tools") {
        tokens += estimate_tokens(&tools.to_string());
    }
    tokens
}

// Diagnostic estimates for the original client request only. Do not log text.
// Separate serialization/rounding means these need not sum to provider usage.
fn input_breakdown(body: &ChatRequest, mode: ToolMode) -> Value {
    let mut instructions = 0;
    let mut conversation = 0;
    let mut results = 0;
    for message in normalized_messages(&body.messages) {
        let tokens = estimate_tokens(&message.to_string());
        match message["role"].as_str() {
            Some("system" | "developer") => instructions += tokens,
            Some("tool") => results += tokens,
            _ => conversation += tokens,
        }
    }
    let (adapter, tools) = if mode == ToolMode::Prompt {
        let definitions: Vec<_> = body.tools.iter().map(|tool| &tool.function).collect();
        (
            estimate_tokens(include_str!("transport-prompt.txt").trim_end()),
            estimate_tokens(&json!(definitions).to_string()),
        )
    } else {
        (
            0,
            if body.tools.is_empty() {
                0
            } else {
                estimate_tokens(&json!(body.tools).to_string())
            },
        )
    };
    json!({"adapter_instructions":adapter,"tool_definitions":tools,
        "client_instructions":instructions,"conversation":conversation,"tool_results":results})
}

fn usage(events: &[Event], input_estimate: u64, raw: &str) -> (Value, &'static str) {
    let actual = events
        .iter()
        .rev()
        .filter_map(|event| event.usage.as_ref())
        .find_map(|usage| {
            let input = usage["inputTokens"]
                .as_u64()
                .or_else(|| usage["prompt_tokens"].as_u64())?;
            let output = usage["outputTokens"]
                .as_u64()
                .or_else(|| usage["completion_tokens"].as_u64())?;
            (input > 0).then_some((input, output))
        });
    let (input, output) = actual.unwrap_or_else(|| (input_estimate, estimate_tokens(raw)));
    (
        json!({"prompt_tokens":input,"completion_tokens":output,"total_tokens":input + output}),
        if actual.is_some() {
            "upstream"
        } else {
            "estimated"
        },
    )
}

fn instructions(body: &ChatRequest) -> String {
    let definitions: Vec<_> = body
        .tools
        .iter()
        .filter(|tool| tool.kind == "function")
        .map(|tool| &tool.function)
        .collect();
    format!(
        "{} Available client tools: {}\nTool choice: {}",
        include_str!("transport-prompt.txt").trim_end(),
        json!(definitions),
        body.tool_choice
    )
}

pub const REPAIR: &str = "Your last response did not match the client transport format. Re-emit the intended next action as one valid JSON object with exactly content and tool_calls. For a file operation, use a listed client tool with name and an arguments object. For a final answer use {\"tool_calls\":[],\"content\":\"answer\"}. Escape quotes and newlines. No Markdown fences, HTML, commentary, or server tools outside that JSON object. No tools from your previous response have executed.";

async fn prompt_response(
    upstream: &impl Upstream,
    mut payload: Value,
) -> Result<(Envelope, Vec<Event>, bool, Value, &'static str)> {
    let input_estimate = request_tokens(&payload);
    let events = upstream.request(payload.clone(), "generic").await?;
    let raw = upstream::text_of(&events)?;
    if let Ok(obj) = transport_object(&raw) {
        let (usage, source) = usage(&events, input_estimate, &raw);
        return Ok((obj, events, false, usage, source));
    }
    if events
        .iter()
        .any(|e| e.kind == "finish" && e.finish_reason.as_deref() == Some("length"))
    {
        return Err(Error::transport(
            "UvA truncated the model response. Try a smaller task or another model.",
        ));
    }
    if raw.chars().count() > 100_000 {
        return Err(Error::transport(
            "Model response was too large to repair. Try a smaller task.",
        ));
    }
    let messages = payload["messages"]
        .as_array_mut()
        .expect("constructed messages are an array");
    messages.push(ui_message(
        "assistant",
        if raw.is_empty() {
            "[No text response was returned.]"
        } else {
            &raw
        },
    ));
    messages.push(ui_message("user", REPAIR));
    let input_estimate = request_tokens(&payload);
    let events = upstream.request(payload, "generic").await?;
    let raw = upstream::text_of(&events)?;
    let obj = transport_object(&raw)
        .map_err(|_| Error::transport("Model formatting failed after one repair attempt. Try another UvA model or a smaller task. No invalid tool call was executed."))?;
    let (usage, source) = usage(&events, input_estimate, &raw);
    Ok((obj, events, true, usage, source))
}

pub struct Completion {
    pub response: Value,
    pub stats: Value,
}

pub async fn completion(
    body: &ChatRequest,
    default_mode: ToolMode,
    upstream: &impl Upstream,
) -> Result<Completion> {
    if body.messages.is_empty() {
        return Err(Error::bad_request("messages must not be empty."));
    }
    let mode = body.uva_tool_mode.unwrap_or(default_mode);
    for message in &body.messages {
        if !matches!(
            message["role"].as_str(),
            Some("system" | "developer" | "user" | "assistant" | "tool")
        ) {
            return Err(Error::bad_request("Message has an invalid role."));
        }
        content_text(message)?;
    }
    if body
        .tools
        .iter()
        .any(|tool| tool.kind != "function" || tool.function.name.is_empty())
    {
        return Err(Error::bad_request(
            "Only named function tools are supported.",
        ));
    }
    if mode == ToolMode::Native
        && body.messages.iter().any(|m| {
            m["role"] == "tool" || m["tool_calls"].as_array().is_some_and(|v| !v.is_empty())
        })
    {
        return Err(Error::bridge(
            "Native tool history is not supported. Use prompt mode.",
        ));
    }
    let messages: Vec<_> = if mode == ToolMode::Prompt {
        vec![ui_message("user", &conversation(&body.messages))]
    } else {
        body.messages
            .iter()
            .map(|m| Ok(ui_message(m["role"].as_str().unwrap(), &content_text(m)?)))
            .collect::<Result<_>>()?
    };
    let mut payload = json!({"model":body.model,"messages":messages});
    if mode == ToolMode::Prompt {
        payload["systemPrompt"] = json!(instructions(body));
    } else if !body.tools.is_empty() {
        payload["tools"] = json!(body.tools);
        payload["tool_choice"] = body.tool_choice.clone();
    }
    let breakdown = input_breakdown(body, mode);
    let initial_input_estimate = request_tokens(&payload);
    let allowed: HashSet<_> = body
        .tools
        .iter()
        .map(|tool| tool.function.name.as_str())
        .collect();
    let (content, calls, events, repaired, usage, usage_source) = if mode == ToolMode::Prompt {
        let (obj, events, repaired, usage, source) = prompt_response(upstream, payload).await?;
        let mut calls = Vec::new();
        for call in obj.tool_calls {
            if !allowed.contains(call.name.as_str()) {
                return Err(Error::transport(
                    "Model requested an unknown tool or invalid arguments.",
                ));
            }
            calls.push(
                json!({"id":format!("call_{}",Uuid::new_v4().simple()),"type":"function",
                "function":{"name":call.name,"arguments":json!(call.arguments).to_string()}}),
            );
        }
        (obj.content, calls, events, repaired, usage, source)
    } else {
        let input_estimate = request_tokens(&payload);
        let events = upstream.request(payload, "generic").await?;
        let content = upstream::text_of(&events)?;
        let mut calls = Vec::new();
        for event in events.iter().filter(|e| e.kind == "tool-input-available") {
            let name = event
                .tool_name
                .as_deref()
                .ok_or_else(|| Error::bridge("UvA returned an invalid tool event."))?;
            if !allowed.contains(name) {
                return Err(Error::bridge(
                    "UvA requested a server tool rather than a client tool.",
                ));
            }
            let id = event
                .tool_call_id
                .as_deref()
                .ok_or_else(|| Error::bridge("UvA returned a tool event without an ID."))?;
            let input = event.input.clone().unwrap_or(json!({}));
            if !input.is_object() {
                return Err(Error::transport("UvA returned invalid tool arguments."));
            }
            calls.push(json!({"id":id,"type":"function","function":{"name":name,"arguments":input.to_string()}}));
        }
        let (usage, source) = usage(&events, input_estimate, &content);
        (json!(content), calls, events, false, usage, source)
    };
    if body.tool_choice == "none" && !calls.is_empty() {
        return Err(Error::transport(
            "Model requested a tool when tool_choice was none.",
        ));
    }
    if body.tool_choice.is_object() && body.tool_choice["type"] == "function" {
        let wanted = &body.tool_choice["function"]["name"];
        if calls.is_empty() || calls.iter().any(|c| &c["function"]["name"] != wanted) {
            return Err(Error::transport(
                "Model did not request the selected function.",
            ));
        }
    }
    if !body.tools.is_empty() && body.tool_choice == "required" && calls.is_empty() {
        return Err(Error::transport(
            "Required client tool call was not returned. Native tools may be unsupported.",
        ));
    }
    let mut message = json!({"role":"assistant","content":if content.as_str().is_some_and(|s| !s.is_empty()) { content } else { Value::Null }});
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    let stats = json!({"mode":mode,"model":body.model,"messages":body.messages.len(),"tools_offered":body.tools.len(),
        "tools_returned":calls.iter().map(|c| &c["function"]["name"]).collect::<Vec<_>>(),"transport_repaired":repaired,"usage":usage,"usage_source":usage_source,"input_breakdown_estimated":breakdown,"initial_input_estimated_tokens":initial_input_estimate,"upstream_requests":if repaired { 2 } else { 1 },"event_types":upstream::event_types(&events)});
    let response = json!({"id":format!("chatcmpl-{}",Uuid::new_v4().simple()),"object":"chat.completion",
        "created":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),"model":body.model,
        "usage":usage,"choices":[{"index":0,"message":message,"finish_reason":if calls.is_empty() { "stop" } else { "tool_calls" }}]});
    Ok(Completion { response, stats })
}

pub fn stream_chunks(result: &Value) -> Vec<String> {
    let choice = &result["choices"][0];
    let message = &choice["message"];
    let mut delta = json!({"role":"assistant"});
    if message["content"].as_str().is_some_and(|s| !s.is_empty()) {
        delta["content"] = message["content"].clone();
    }
    if let Some(calls) = message["tool_calls"].as_array() {
        delta["tool_calls"] = calls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut call = c.clone();
                call["index"] = json!(i);
                call
            })
            .collect();
    }
    let mut chunks = Vec::new();
    for (delta, finish) in [
        (delta, Value::Null),
        (json!({}), choice["finish_reason"].clone()),
    ] {
        let mut chunk = result.clone();
        chunk["object"] = json!("chat.completion.chunk");
        if finish.is_null() {
            chunk.as_object_mut().unwrap().remove("usage");
        }
        chunk["choices"] = json!([{"index":0,"delta":delta,"finish_reason":finish}]);
        chunks.push(chunk.to_string());
    }
    chunks.push("[DONE]".into());
    chunks
}
