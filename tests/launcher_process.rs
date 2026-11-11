#[cfg(unix)]
use std::time::Duration;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};
use tempfile::TempDir;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_uva-opencode"))
}
struct Fixture {
    state: TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = Command::new(binary())
            .arg("--stop")
            .env("UVA_STATE_DIR", self.state.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Needs exclusive ownership of port 8765. The browser path is deliberately invalid.
#[test]
#[ignore = "requires port 8765 to be free"]
fn concurrent_launchers_start_one_service_and_stop_releases_it() {
    let port = std::net::TcpListener::bind("127.0.0.1:8765")
        .expect("stop the existing helper before this test");
    drop(port);
    let fixture = Fixture {
        state: tempfile::tempdir().unwrap(),
    };
    let missing = fixture.state.path().join("missing-browser");
    let mut clients = Vec::new();
    for _ in 0..6 {
        clients.push(
            Command::new(binary())
                .args(["--check", "--wait", "3"])
                .env("UVA_STATE_DIR", fixture.state.path())
                .env("UVA_BROWSER_PATH", &missing)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in clients {
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("UVA_BROWSER_PATH"));
    }
    let log = fs::read_to_string(fixture.state.path().join("bridge.log")).unwrap();
    assert_eq!(
        log.matches("UvA bridge on").count(),
        1,
        "duplicate background services started"
    );
    let result = Command::new(binary())
        .arg("--stop")
        .env("UVA_STATE_DIR", fixture.state.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:8765").unwrap();
    let file = fs::OpenOptions::new()
        .append(true)
        .open(fixture.state.path().join("service.lock"))
        .unwrap();
    assert!(file.try_lock().is_ok());
    drop(listener);
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires port 8765 to be free"]
async fn ready_service_preserves_directory_environment_and_opencode_exit_code() {
    use axum::{Json, Router, routing::get};
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture {
        state: tempfile::tempdir().unwrap(),
    };
    let project = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    fs::write(
        fixture.state.path().join("local-token"),
        "fixture-api-key-0123456789abcdef00",
    )
    .unwrap();
    let executable = tools.path().join("opencode");
    fs::write(&executable,"#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$XDG_CONFIG_HOME\" \"${OPENCODE_SENTINEL-unset}\" \"${opencode_lower_sentinel-unset}\" \"$1\" > \"$CAPTURE\"\nexit 37\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8765")
        .await
        .expect("stop the existing helper before this test");
    let app=Router::new().route("/health",get(||async{Json(json!({"service":"uva-opencode","implementation":"rust","version":env!("CARGO_PKG_VERSION"),"protocol":3,"ready":true}))}));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let capture = fixture.state.path().join("capture");
    let output = tokio::process::Command::new(binary())
        .arg(project.path())
        .current_dir(fixture.state.path())
        .env("UVA_STATE_DIR", ".")
        .env("PATH", tools.path())
        .env("CAPTURE", &capture)
        .env("OPENCODE_SENTINEL", "remove-me")
        .env("opencode_lower_sentinel", "remove-me-too")
        .output()
        .await
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(37),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let captured = fs::read_to_string(capture).unwrap();
    let lines: Vec<_> = captured.lines().collect();
    assert_eq!(
        lines[0],
        project.path().canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(
        lines[1],
        fixture
            .state
            .path()
            .canonicalize()
            .unwrap()
            .join("opencode-profile/config")
            .to_str()
            .unwrap()
    );
    assert_eq!(lines[2], "unset");
    assert_eq!(lines[3], "unset");
    assert_eq!(lines[4], "--standalone");
    assert!(
        !fixture.state.path().join("bridge.log").exists(),
        "an existing ready bridge must be reused"
    );
    server.abort();
    let _ = server.await;
    tokio::time::sleep(Duration::from_millis(20)).await;
}
