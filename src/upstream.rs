use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeSet, future::Future};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    #[serde(default, alias = "totalUsage", skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    #[serde(
        default,
        rename = "toolCallId",
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_call_id: Option<String>,
    #[serde(default, rename = "toolName", skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(
        default,
        rename = "finishReason",
        skip_serializing_if = "Option::is_none"
    )]
    pub finish_reason: Option<String>,
}

pub trait Upstream: Send + Sync {
    fn request(
        &self,
        payload: Value,
        endpoint: &str,
    ) -> impl Future<Output = Result<Vec<Event>>> + Send;
}
impl<U: Upstream> Upstream for std::sync::Arc<U> {
    async fn request(&self, payload: Value, endpoint: &str) -> Result<Vec<Event>> {
        self.as_ref().request(payload, endpoint).await
    }
}

/// Parse SSE frames, including CRLF and multiline data. Ignore metadata and [DONE].
pub fn parse_events(stream: &str) -> Result<Vec<Event>> {
    if stream.len() > 8_000_000 {
        return Err(Error::bridge("UvA response was too large."));
    }
    let mut events = Vec::new();
    let mut data = Vec::new();
    let flush = |data: &mut Vec<&str>, events: &mut Vec<Event>| -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let raw = data.join("\n");
        data.clear();
        if raw.trim() == "[DONE]" {
            return Ok(());
        }
        let event: Event = serde_json::from_str(&raw)
            .map_err(|_| Error::bridge("UvA returned a malformed stream event."))?;
        events.push(event);
        if events.len() > 100_000 {
            return Err(Error::bridge("UvA returned too many stream events."));
        }
        Ok(())
    };
    for line in stream.lines() {
        if line.is_empty() {
            flush(&mut data, &mut events)?;
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    flush(&mut data, &mut events)?;
    if events.is_empty() {
        return Err(Error::bridge("UvA returned no stream events."));
    }
    Ok(events)
}

pub fn text_of(events: &[Event]) -> Result<String> {
    if events.iter().any(|e| e.kind == "error") {
        return Err(Error::bridge("UvA returned an error event."));
    }
    Ok(events
        .iter()
        .filter(|e| e.kind == "text-delta")
        .filter_map(|e| e.delta.as_deref())
        .collect())
}

pub fn event_types(events: &[Event]) -> BTreeSet<&str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stream_frames_and_split_tokens() {
        let events = parse_events(": ping\r\nevent: message\r\ndata: {\"type\":\"text-delta\",\r\ndata: \"delta\":\"UVA_\"}\r\n\r\ndata: {\"type\":\"text-delta\",\"delta\":\"RUST_OK\"}\n\ndata: [DONE]\n\n").unwrap();
        assert_eq!(text_of(&events).unwrap(), "UVA_RUST_OK");
    }
    #[test]
    fn malformed_empty_and_error_streams_fail() {
        assert!(parse_events("data: [DONE]\n\n").is_err());
        assert!(parse_events("data: {\n\n").is_err());
        assert!(
            text_of(
                &parse_events("data: {\"type\":\"error\",\"errorText\":\"private data\"}\n\n")
                    .unwrap()
            )
            .is_err()
        );
    }
}
