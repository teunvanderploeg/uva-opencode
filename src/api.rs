use crate::{
    error::{Error, ErrorKind},
    models,
    session::Session,
    transport::{self, ChatRequest, ToolMode},
    upstream::{self, Upstream},
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Request, State, rejection::BytesRejection},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use futures::stream;
use serde_json::{Value, json};
use std::{collections::VecDeque, convert::Infallible, sync::Arc};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct ApiState<U> {
    pub upstream: U,
    pub session: Option<Session>,
    pub token: String,
    pub mode: ToolMode,
    pub stop: CancellationToken,
    stats: Mutex<VecDeque<Value>>,
}
impl<U> ApiState<U> {
    pub fn new(
        upstream: U,
        session: Option<Session>,
        token: String,
        mode: ToolMode,
        stop: CancellationToken,
    ) -> Self {
        Self {
            upstream,
            session,
            token,
            mode,
            stop,
            stats: Mutex::new(VecDeque::new()),
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (code, kind) = match self.kind {
            ErrorKind::Transport => (StatusCode::UNPROCESSABLE_ENTITY, "uva_transport_error"),
            ErrorKind::Bridge => (StatusCode::BAD_GATEWAY, "uva_bridge_error"),
            ErrorKind::BadRequest => (StatusCode::BAD_REQUEST, "invalid_request_error"),
            ErrorKind::Busy => (StatusCode::TOO_MANY_REQUESTS, "uva_bridge_error"),
            ErrorKind::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "uva_bridge_error"),
            ErrorKind::Timeout => (StatusCode::GATEWAY_TIMEOUT, "uva_bridge_error"),
        };
        (
            code,
            Json(json!({"error":{"message":self.message,"type":kind}})),
        )
            .into_response()
    }
}

pub fn authorized(headers: &HeaderMap, token: &str) -> bool {
    if headers.contains_key(header::ORIGIN) || token.is_empty() {
        return false;
    }
    if !matches!(
        headers.get(header::HOST).and_then(|v| v.to_str().ok()),
        Some("127.0.0.1:8765" | "localhost:8765")
    ) {
        return false;
    }
    let Some(supplied) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let expected = format!("Bearer {token}");
    bool::from(supplied.as_bytes().ct_eq(expected.as_bytes()))
}

async fn authenticate<U: Upstream + 'static>(
    State(state): State<Arc<ApiState<U>>>,
    request: Request,
    next: Next,
) -> Response {
    if !authorized(request.headers(), &state.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Local bridge authentication required."})),
        )
            .into_response();
    }
    next.run(request).await
}

pub fn router<U: Upstream + 'static>(state: Arc<ApiState<U>>) -> Router {
    Router::new()
        .route("/health", get(health::<U>))
        .route("/stats", get(stats::<U>))
        .route("/v1/models", get(|| async { Json(models::listing()) }))
        .route("/v1/chat/completions", post(chat::<U>))
        .route("/probe", post(probe::<U>))
        .route("/shutdown", post(shutdown::<U>))
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"Unknown route."})),
            )
        })
        .layer(DefaultBodyLimit::max(2_000_000))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authenticate::<U>,
        ))
        .with_state(state)
}

async fn health<U>(State(state): State<Arc<ApiState<U>>>) -> Json<Value> {
    let mut result = json!({"service":"uva-opencode","protocol":3,"implementation":"rust","version":env!("CARGO_PKG_VERSION"),"tool_mode":state.mode,"backend":"browser","ready":true});
    if let Some(session) = &state.session {
        let status = serde_json::to_value(session.status()).unwrap();
        result
            .as_object_mut()
            .unwrap()
            .extend(status.as_object().unwrap().clone());
    }
    Json(result)
}
async fn stats<U>(State(state): State<Arc<ApiState<U>>>) -> Json<Value> {
    Json(json!(*state.stats.lock().await))
}

fn body<T: serde::de::DeserializeOwned>(
    bytes: std::result::Result<Bytes, BytesRejection>,
) -> std::result::Result<T, (StatusCode, Json<Value>)> {
    let bytes = bytes.map_err(|e| (e.status(), Json(json!({"error":{"message":"Request body too large or unreadable.","type":"invalid_request_error"}}))))?;
    if bytes.is_empty() {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"error":"Request body too large or empty."})),
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        (StatusCode::BAD_REQUEST, Json(json!({"error":{"message":"Invalid JSON request or missing required fields.","type":"invalid_request_error"}})))
    })
}

async fn chat<U: Upstream>(
    State(state): State<Arc<ApiState<U>>>,
    bytes: std::result::Result<Bytes, BytesRejection>,
) -> Response {
    let request: ChatRequest = match body(bytes) {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    match transport::completion(&request, state.mode, &state.upstream).await {
        Err(error) => error.into_response(),
        Ok(result) => {
            let mut stats = state.stats.lock().await;
            if stats.len() == 1000 {
                stats.pop_front();
            }
            stats.push_back(result.stats);
            drop(stats);
            if !request.stream {
                return Json(result.response).into_response();
            }
            let events = transport::stream_chunks(&result.response)
                .into_iter()
                .map(|s| Ok::<_, Infallible>(Event::default().data(s)));
            Sse::new(stream::iter(events)).into_response()
        }
    }
}

async fn probe<U: Upstream>(
    State(state): State<Arc<ApiState<U>>>,
    bytes: std::result::Result<Bytes, BytesRejection>,
) -> Response {
    #[derive(serde::Deserialize)]
    struct Probe {
        payload: Value,
        #[serde(default = "generic")]
        endpoint: String,
    }
    fn generic() -> String {
        "generic".into()
    }
    let request: Probe = match body(bytes) {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    match state.upstream.request(request.payload, &request.endpoint).await {
        Err(error) => error.into_response(),
        Ok(events) => match upstream::text_of(&events) {
            Err(error) => error.into_response(),
            Ok(text) => Json(json!({"text":text,"events":events.iter().filter(|e| matches!(e.kind.as_str(), "tool-input-available" | "finish" | "error")).collect::<Vec<_>>(),"event_types":upstream::event_types(&events)})).into_response(),
        },
    }
}

async fn shutdown<U>(
    State(state): State<Arc<ApiState<U>>>,
    bytes: std::result::Result<Bytes, BytesRejection>,
) -> Response {
    let _: Value = match body(bytes) {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    state.stop.cancel();
    Json(json!({"stopping":true})).into_response()
}
