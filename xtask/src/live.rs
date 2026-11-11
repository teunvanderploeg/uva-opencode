use anyhow::{Context, Result};
use clap::ValueEnum;
use serde_json::{Value, json};
use std::{fs, path::PathBuf, time::Duration};

#[derive(Clone, Copy, ValueEnum)]
pub enum Scenario {
    Copy,
    Website,
}

fn state(path: Option<PathBuf>) -> PathBuf {
    path.or_else(|| std::env::var_os("UVA_STATE_DIR").map(PathBuf::from))
        .unwrap_or_else(|| crate::root().join(".runtime/rust"))
}
fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(300))
        .build()?)
}

pub async fn run(
    model: &str,
    scenario: Scenario,
    state_path: Option<PathBuf>,
    opencode: Option<PathBuf>,
) -> Result<()> {
    let state = state(state_path);
    let token = fs::read_to_string(state.join("local-token"))
        .context("Start the Rust bridge with cargo run -- --check first.")?;
    let health: Value = client()?
        .get("http://127.0.0.1:8765/health")
        .bearer_auth(token.trim())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    anyhow::ensure!(
        health["implementation"] == "rust" && health["ready"] == true,
        "A ready Rust bridge is required for this check."
    );
    let scratch = tempfile::tempdir()?;
    let project = scratch.path().canonicalize()?;
    let nonce = format!("UVA_OPENCODE_{}", uuid::Uuid::new_v4().simple());
    fs::write(project.join("challenge.txt"), format!("{nonce}\n"))?;
    let mut config: Value = serde_json::from_str(include_str!("../../opencode.example.json"))?;
    anyhow::ensure!(
        !config["providers"]["uva"]["models"][model].is_null(),
        "Choose a model from opencode.example.json."
    );
    config["snapshots"] = json!(false);
    config["warming"] = json!(false);
    let (agent, prompt, output, suffix) = match scenario {
        Scenario::Copy => {
            config["default_agent"] = json!("uva-probe");
            config["agents"] = json!({"uva-probe":{"description":"Read and write the disposable connection-test files only.","mode":"primary","steps":6,
                "system":"Use client tools to read challenge.txt and write the exact value to answer.txt. Do not invent file contents. Do not use shell, network, or subagents. Give a short final answer.",
                "permissions":[{"action":"*","resource":"*","effect":"deny"},{"action":"read","resource":"challenge.txt","effect":"allow"},{"action":"read","resource":"answer.txt","effect":"allow"},{"action":"edit","resource":"answer.txt","effect":"allow"}]}});
            (
                "uva-probe",
                "Read challenge.txt with a tool. Write its exact contents to answer.txt with a tool. Then say TEST_COMPLETE.",
                "answer.txt",
                "",
            )
        }
        Scenario::Website => {
            config["default_agent"] = json!("build");
            config["agents"] = json!({"build":{"steps":10,"permissions":[{"action":"*","resource":"*","effect":"deny"},{"action":"read","resource":"*","effect":"allow"},{"action":"glob","resource":"*","effect":"allow"},{"action":"edit","resource":"*.html","effect":"allow"},{"action":"edit","resource":"*.css","effect":"allow"},{"action":"edit","resource":"*.js","effect":"allow"}]}});
            (
                "build",
                "Create a self-contained UvA OS3 master website in index.html using the client file tools. Use a text wordmark. Read challenge.txt and include its verification value in an HTML comment. Do not paste HTML in chat. Do not use shell, network, or subagents.",
                "index.html",
                "-website",
            )
        }
    };
    fs::write(project.join("opencode.json"), serde_json::to_vec(&config)?)?;
    let executable = opencode.unwrap_or_else(|| {
        PathBuf::from(if cfg!(windows) {
            "opencode.exe"
        } else {
            "opencode"
        })
    });
    let mut command = tokio::process::Command::new(executable);
    command
        .args([
            "run",
            "--standalone",
            "--format",
            "json",
            "--model",
            &format!("uva/{model}"),
            "--agent",
            agent,
            prompt,
        ])
        .current_dir(&project)
        .env_clear();
    for (key, value) in std::env::vars_os().filter(|(key, _)| {
        !["OPENCODE_", "OPENAI_", "ANTHROPIC_", "GOOGLE_", "GEMINI_"]
            .iter()
            .any(|prefix| {
                key.to_string_lossy()
                    .to_ascii_uppercase()
                    .starts_with(prefix)
            })
    }) {
        command.env(key, value);
    }
    for (name, leaf) in [
        ("XDG_DATA_HOME", "data"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
    ] {
        command.env(name, project.join(".isolated").join(leaf));
    }
    command
        .env("UVA_BRIDGE_KEY", token.trim())
        .env("PWD", &project)
        .kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(300), command.output())
        .await
        .context("OpenCode live test timed out.")??;
    let events = String::from_utf8_lossy(&result.stdout);
    let completed: Vec<_> = events
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|e| e["type"] == "tool_use" && e["part"]["state"]["status"] == "completed")
        .filter_map(|e| e["part"]["tool"].as_str().map(String::from))
        .collect();
    let actual = fs::read_to_string(project.join(output)).unwrap_or_default();
    let matches = match scenario {
        Scenario::Copy => actual.trim() == nonce,
        Scenario::Website => actual.contains(&nonce) && actual.to_lowercase().contains("<html"),
    };
    let success = result.status.success()
        && matches
        && completed.iter().any(|t| t == "read")
        && completed
            .iter()
            .any(|t| matches!(t.as_str(), "write" | "edit" | "patch"));
    let report = json!({"checked_at":chrono::Utc::now().to_rfc3339(),"implementation":"rust","opencode_exit_code":result.status.code(),"file_contents_match":success,"mode":"prompt","native_tools":false,"isolated_project":true,"completed_tools":completed,"model":model,"scenario":if matches!(scenario,Scenario::Copy) {"copy"} else {"website"},"output_bytes":actual.len()});
    fs::write(
        state.join(format!("opencode{suffix}-result.json")),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    anyhow::ensure!(
        success,
        "OpenCode did not complete the verified file operation. Exit status: {}.",
        result.status
    );
    Ok(())
}

pub async fn models(path: Option<PathBuf>) -> Result<()> {
    let state = state(path);
    let token = fs::read_to_string(state.join("local-token"))?;
    let health: Value = client()?
        .get("http://127.0.0.1:8765/health")
        .bearer_auth(token.trim())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    anyhow::ensure!(
        health["implementation"] == "rust" && health["ready"] == true,
        "A ready Rust bridge is required for this check."
    );
    let config: Value = serde_json::from_str(include_str!("../../opencode.example.json"))?;
    let mut results = Vec::new();
    for (model, spec) in config["providers"]["uva"]["models"].as_object().unwrap() {
        if spec["disabled"] == true {
            continue;
        }
        let response=client()?.post("http://127.0.0.1:8765/v1/chat/completions").bearer_auth(token.trim())
            .json(&json!({"model":model,"messages":[{"role":"user","content":"Reply with exactly UVA_MODEL_OK."}]})).send().await?;
        let status = response.status();
        let body: Value = response.json().await?;
        let passed = status.is_success()
            && body["choices"][0]["message"]["content"]
                .as_str()
                .is_some_and(|s| s.trim().trim_end_matches('.') == "UVA_MODEL_OK");
        let entry = json!({"model":model,"passed":passed,"http_status":status.as_u16()});
        println!("{entry}");
        results.push(entry);
        fs::write(
            state.join("model-validation.json"),
            serde_json::to_vec_pretty(
                &json!({"checked_at":chrono::Utc::now().to_rfc3339(),"implementation":"rust","results":results}),
            )?,
        )?;
    }
    anyhow::ensure!(
        results.iter().all(|r| r["passed"] == true),
        "Some model checks failed."
    );
    Ok(())
}
