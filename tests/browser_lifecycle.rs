use axum::{
    Router,
    http::{HeaderMap, header},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::time::Duration;
use tempfile::tempdir;
use uva_opencode::{browser::Driver, paths::Paths};

/// Explicitly run with `cargo test --test browser_lifecycle -- --ignored`.
/// Uses only a local fixture server and a disposable profile, never UvA credentials.
#[tokio::test]
#[ignore = "requires bundled Chromium and a desktop for the visible-to-headless check"]
async fn visible_login_persists_across_headless_restarts_and_identity_is_retained() {
    let app = Router::new()
        .route("/login", get(|| async { ([(header::SET_COOKIE, "fixture=signed-in; HttpOnly; SameSite=Strict; Max-Age=3600; Path=/")], "signed in") }))
        .route("/chat", get(|| async { "fixture chat" }))
        .route("/session", get(|headers: HeaderMap| async move {
            axum::Json(json!({"authenticated":headers.get(header::COOKIE).is_some_and(|v| v.to_str().unwrap_or("").contains("fixture=signed-in"))}))
        }))
        .route("/api/v1/chat/generic", post(|headers: HeaderMap, axum::Json(body): axum::Json<Value>| async move {
            assert!(headers.get(header::COOKIE).unwrap().to_str().unwrap().contains("fixture=signed-in"));
            assert_eq!(body["model"], "fixture-model");
            "data: {\"type\":\"text-delta\",\"delta\":\"UVA_RUST_OK\"}\r\n\r\ndata: [DONE]\r\n\r\n"
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let paths = Paths::discover().unwrap();
    let executable = paths.browser_executable().unwrap();
    let profile = tempdir().unwrap();
    let driver = Driver::launch(&executable, profile.path(), false, None)
        .await
        .unwrap();
    assert!(
        !profile.path().join("DevToolsActivePort").exists(),
        "the browser must not expose a debugging port"
    );
    driver.navigate(&format!("{origin}/login")).await.unwrap();
    let identity = driver.user_agent().await.unwrap();
    driver.close().await;
    for _ in 0..2 {
        let driver = Driver::launch(&executable, profile.path(), true, Some(&identity))
            .await
            .unwrap();
        driver.navigate(&format!("{origin}/chat")).await.unwrap();
        assert_eq!(driver.user_agent().await.unwrap(), identity);
        assert_eq!(
            driver
                .evaluate("async () => (await fetch('/session')).json()")
                .await
                .unwrap(),
            json!({"authenticated":true})
        );
        let result = driver
            .fetch(&json!({"model":"fixture-model"}), "generic")
            .await
            .unwrap();
        assert_eq!(result["status"], 200);
        assert!(result["stream"].as_str().unwrap().contains("UVA_RUST_OK"));
        tokio::time::timeout(Duration::from_secs(12), driver.close())
            .await
            .unwrap();
    }
    server.abort();
}
