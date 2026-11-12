use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uva_opencode::{
    api::{self, ApiState},
    error::Result,
    transport::ToolMode,
    upstream::{self, Event, Upstream},
};

struct Fake;
impl Upstream for Fake {
    async fn request(&self, payload: Value, _: &str) -> Result<Vec<Event>> {
        let malformed = payload["model"] == "malformed";
        upstream::parse_events(&format!(
            "data: {}\n\n",
            json!({"type":"text-delta","delta":if malformed { "invalid" } else { r#"{"content":"hello","tool_calls":[]}"# }})
        ))
    }
}
fn app() -> axum::Router {
    api::router(Arc::new(ApiState::new(
        Fake,
        None,
        "api-key".into(),
        ToolMode::Prompt,
        CancellationToken::new(),
    )))
}
fn request(method: &str, path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "127.0.0.1:8765")
        .header("Authorization", "Bearer api-key")
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}
async fn value(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 3_000_000).await.unwrap()).unwrap()
}

#[tokio::test]
async fn token_origin_and_host_checks_precede_body_processing() {
    for (name, value) in [
        ("Authorization", "Bearer relay-key"),
        ("Origin", "https://aichat.uva.nl"),
        ("Origin", "https://evil.test"),
        ("Host", "evil.test:8765"),
    ] {
        for path in ["/health", "/shutdown", "/v1/chat/completions"] {
            let mut req = request("POST", path, json!({}));
            req.headers_mut().insert(name, value.parse().unwrap());
            assert_eq!(
                app().oneshot(req).await.unwrap().status(),
                StatusCode::UNAUTHORIZED
            );
        }
    }
}
#[tokio::test]
async fn model_listing_and_health_preserve_the_contract() {
    let health = value(
        app()
            .oneshot(request("GET", "/health", json!({})))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(health["service"], "uva-opencode");
    assert_eq!(health["protocol"], 3);
    assert_eq!(health["implementation"], "rust");
    let models = value(
        app()
            .oneshot(request("GET", "/v1/models", json!({})))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(models["data"].as_array().unwrap().len(), 17);
    assert!(
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["id"] != "claude-haiku-4.5" && m["id"] != "claude-sonnet-5")
    );
}
#[tokio::test]
async fn unrepaired_transport_failures_are_http_422() {
    let response = app()
        .oneshot(request(
            "POST",
            "/v1/chat/completions",
            json!({"model":"malformed","messages":[{"role":"user","content":"test"}]}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        value(response).await["error"]["type"],
        "uva_transport_error"
    );
}
#[tokio::test]
async fn json_and_sse_have_matching_content_and_finish_reasons() {
    let response = value(
        app()
            .oneshot(request(
                "POST",
                "/v1/chat/completions",
                json!({"messages":[{"role":"user","content":"test"}]}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(response["choices"][0]["message"]["content"], "hello");
    let response = app()
        .oneshot(request(
            "POST",
            "/v1/chat/completions",
            json!({"messages":[{"role":"user","content":"test"}],"stream":true}),
        ))
        .await
        .unwrap();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    let frames: Vec<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .collect();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[2], "[DONE]");
    let first: Value = serde_json::from_str(frames[0]).unwrap();
    assert_eq!(first["choices"][0]["delta"]["content"], "hello");
    let last: Value = serde_json::from_str(frames[1]).unwrap();
    assert_eq!(last["choices"][0]["finish_reason"], "stop");
    assert!(first.get("usage").is_none());
    assert!(last["usage"]["prompt_tokens"].as_u64().unwrap() > 0);
    assert_eq!(
        last["usage"]["total_tokens"].as_u64().unwrap(),
        last["usage"]["prompt_tokens"].as_u64().unwrap()
            + last["usage"]["completion_tokens"].as_u64().unwrap()
    );
}
#[tokio::test]
async fn oversized_and_malformed_requests_are_rejected() {
    let mut req = request("POST", "/v1/chat/completions", json!({}));
    *req.body_mut() = Body::from("x".repeat(2_000_001));
    assert_eq!(
        app().oneshot(req).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        app()
            .oneshot(request(
                "POST",
                "/v1/chat/completions",
                json!({"messages":"invalid"})
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn shutdown_requires_authentication_and_cancels_the_service() {
    let stop = CancellationToken::new();
    let app = api::router(Arc::new(ApiState::new(
        Fake,
        None,
        "api-key".into(),
        ToolMode::Prompt,
        stop.clone(),
    )));
    assert_eq!(
        app.oneshot(request("POST", "/shutdown", json!({})))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert!(stop.is_cancelled());
}
