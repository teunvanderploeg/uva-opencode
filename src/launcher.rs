use crate::{
    api::{self, ApiState},
    models,
    paths::{Paths, home, private_dir, private_write},
    session::Session,
    transport::ToolMode,
};
use anyhow::{Context, Result, bail};
use rand::RngCore;
use serde_json::{Value, json};
use std::{
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    process::Command,
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;

pub const BASE: &str = "http://127.0.0.1:8765";

fn lock_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new().create(true).append(true).open(path)?)
}

pub fn local_token(paths: &Paths) -> Result<String> {
    private_dir(&paths.state)?;
    let file = lock_file(&paths.state.join("token.lock"))?;
    file.lock()?;
    let path = paths.state.join("local-token");
    if !path.exists() {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        private_write(&path, token)?;
    }
    let token = fs::read_to_string(path)?.trim().to_string();
    anyhow::ensure!(
        token.len() >= 32 && token.bytes().all(|b| b.is_ascii_graphic()),
        "The local bridge token is invalid."
    );
    Ok(token)
}

pub fn client() -> Result<reqwest::Client> {
    // Loopback must never pass through a user's HTTP proxy or follow redirects with credentials.
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()?)
}

pub async fn api(paths: &Paths, route: &str, payload: Option<Value>) -> Result<Value> {
    let token = fs::read_to_string(paths.state.join("local-token"))?;
    let client = client()?;
    let request = if let Some(payload) = payload {
        client.post(format!("{BASE}{route}")).json(&payload)
    } else {
        client.get(format!("{BASE}{route}"))
    };
    let response = request.bearer_auth(token.trim()).send().await?;
    anyhow::ensure!(
        response.status().is_success(),
        "The local bridge rejected the request with HTTP {}.",
        response.status()
    );
    response
        .json()
        .await
        .context("The local bridge returned invalid JSON.")
}

pub async fn health(paths: &Paths) -> Result<Option<Value>> {
    // Check the socket even if there is no token, so an unrelated service is not mistaken for an absent one.
    let response = client()?
        .get(format!("{BASE}/health"))
        .bearer_auth(
            fs::read_to_string(paths.state.join("local-token"))
                .unwrap_or_default()
                .trim(),
        )
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) if error.is_connect() => return Ok(None),
        Err(error) if error.is_timeout() => bail!("A service on port 8765 is not responding."),
        Err(_) => bail!("Could not contact the local bridge."),
    };
    anyhow::ensure!(
        response.status().is_success(),
        "Port 8765 is occupied by an inaccessible service. Stop the existing helper before continuing."
    );
    let value: Value = response
        .json()
        .await
        .context("Port 8765 is occupied by an unrelated service.")?;
    anyhow::ensure!(
        value["service"] == "uva-opencode" && matches!(value["protocol"].as_u64(), Some(2 | 3)),
        "An incompatible service is running on port 8765."
    );
    Ok(Some(value))
}

pub async fn serve(paths: Paths, mode: ToolMode) -> Result<()> {
    let token = local_token(&paths)?;
    let service_lock = lock_file(&paths.state.join("service.lock"))?;
    service_lock
        .try_lock()
        .context("This helper is already running.")?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8765")
        .await
        .context("Could not bind port 8765. Another helper may be running.")?;
    let stop = CancellationToken::new();
    let (session, mut worker) = Session::start(paths, stop.clone());
    let state = Arc::new(ApiState::new(
        session.clone(),
        Some(session),
        token,
        mode,
        stop.clone(),
    ));
    let signal_stop = stop.clone();
    let signal = tokio::spawn(async move {
        #[cfg(unix)]
        {
            if let Ok(mut term) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            } else {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
        #[cfg(not(unix))]
        {
            if tokio::signal::ctrl_c().await.is_err() {
                // A detached Windows helper may have no console. HTTP shutdown remains available.
                std::future::pending::<()>().await;
            }
        }
        signal_stop.cancel();
    });
    println!("UvA bridge on 127.0.0.1:8765. Backend: Rust Chromium");
    let shutdown = stop.clone();
    let result = axum::serve(listener, api::router(state))
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await;
    stop.cancel();
    if timeout(Duration::from_secs(20), &mut worker).await.is_err() {
        worker.abort();
        let _ = worker.await;
    }
    signal.abort();
    result?;
    Ok(())
}

async fn wait_stopped(paths: &Paths) -> Result<()> {
    for _ in 0..250 {
        let released = if paths.state.join("service.lock").exists() {
            lock_file(&paths.state.join("service.lock"))?
                .try_lock()
                .is_ok()
        } else {
            true
        };
        if health(paths).await?.is_none() && released {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    bail!("The previous bridge did not shut down in time.")
}

pub async fn ensure_bridge(paths: &Paths) -> Result<Value> {
    private_dir(&paths.state)?;
    let file = lock_file(&paths.state.join("launch.lock"))?;
    // File locks may block; keep that off Tokio's event-loop threads.
    let _lock = tokio::task::spawn_blocking(move || -> Result<File> {
        file.lock()?;
        Ok(file)
    })
    .await??;
    if let Some(existing) = health(paths).await? {
        anyhow::ensure!(
            existing["implementation"] == "rust"
                && existing["version"] == env!("CARGO_PKG_VERSION"),
            "A different helper version is running. Run its --stop command before starting this version."
        );
        return Ok(existing);
    }
    let log_path = paths.state.join("bridge.log");
    if !log_path.exists() {
        private_write(&log_path, "")?;
    }
    let log = OpenOptions::new().append(true).open(&log_path)?;
    let mut command = Command::new(env::current_exe()?);
    command
        .arg("--internal-bridge")
        .env("UVA_STATE_DIR", &paths.state)
        .env("UVA_BROWSERS_DIR", &paths.browsers)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.as_std_mut().pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
    }
    #[cfg(windows)]
    {
        command.creation_flags(0x00000200 | 0x08000000);
    }
    let mut child = command
        .spawn()
        .context("Could not start the background helper.")?;
    for _ in 0..100 {
        if let Some(status) = health(paths).await? {
            return Ok(status);
        }
        if child.try_wait()?.is_some() {
            bail!("The bridge could not start. See {}.", log_path.display());
        }
        sleep(Duration::from_millis(100)).await;
    }
    let _ = child.kill().await;
    bail!(
        "The bridge did not start within ten seconds. See {}.",
        log_path.display()
    )
}

pub async fn stop(paths: &Paths) -> Result<()> {
    if health(paths).await?.is_some() {
        api(paths, "/shutdown", Some(json!({}))).await?;
        wait_stopped(paths).await?;
        println!("UvA bridge stopped.");
    } else {
        println!("UvA bridge is not running.");
    }
    Ok(())
}

pub async fn wait_ready(paths: &Paths, seconds: f64) -> Result<()> {
    println!("Connecting to UvA Chat...");
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    let mut prompted = false;
    while Instant::now() < deadline {
        if let Some(status) = health(paths).await? {
            if status["ready"] == true {
                return Ok(());
            }
            if status["browser_state"] == "error" {
                bail!(
                    "{}",
                    status["browser_error"]
                        .as_str()
                        .unwrap_or("Browser helper failed.")
                );
            }
            if status["browser_state"] == "login_required" && !prompted {
                println!(
                    "Sign in with UvA in the Chromium window. It will close when login is complete."
                );
                prompted = true;
            }
        } else {
            bail!("The browser helper stopped while connecting.");
        }
        sleep(Duration::from_millis(500)).await;
    }
    bail!("Login did not finish in time. Run uva-opencode --login to retry.")
}

pub fn opencode_binary(paths: &Paths) -> Result<PathBuf> {
    let name = if cfg!(windows) {
        "opencode.exe"
    } else {
        "opencode"
    };
    let bundled = paths.app.join("vendor").join(name);
    if bundled.is_file() {
        return Ok(bundled);
    }
    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    let fallback = home()?.join(".opencode/bin").join(name);
    if fallback.is_file() {
        return Ok(fallback);
    }
    bail!("OpenCode must be installed and on PATH, or included in the native package.")
}

pub fn opencode_environment(paths: &Paths, project: &Path) -> Result<Vec<(OsString, OsString)>> {
    let profile = paths.state.join("opencode-profile");
    let config = profile.join("config/opencode");
    private_dir(&config)?;
    private_write(&config.join("opencode.json"), models::CONFIG)?;
    let mut environment: Vec<_> = env::vars_os()
        .filter(|(key, _)| {
            !key.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("OPENCODE_")
        })
        .collect();
    let managed = [
        "UVA_BRIDGE_KEY",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
        "PWD",
    ];
    environment.retain(|(key, _)| !managed.iter().any(|name| key.eq_ignore_ascii_case(name)));
    environment.push((
        "UVA_BRIDGE_KEY".into(),
        fs::read_to_string(paths.state.join("local-token"))?
            .trim()
            .into(),
    ));
    for (name, leaf) in [
        ("XDG_DATA_HOME", "data"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_STATE_HOME", "state"),
    ] {
        environment.push((name.into(), profile.join(leaf).into_os_string()));
    }
    environment.push(("PWD".into(), project.as_os_str().into()));
    Ok(environment)
}

pub async fn launch(
    paths: &Paths,
    directory: &Path,
    login: bool,
    check: bool,
    wait: f64,
) -> Result<i32> {
    let directory = if directory.starts_with("~") {
        home()?.join(directory.strip_prefix("~").unwrap())
    } else {
        directory.to_path_buf()
    }
    .canonicalize()
    .context("Choose an existing project directory.")?;
    anyhow::ensure!(directory.is_dir(), "Choose a project directory.");
    let binary = if !login && !check {
        Some(opencode_binary(paths)?)
    } else {
        None
    };
    if login
        && let Some(status) = health(paths).await?
        && status["implementation"] == "rust"
        && status["browser_state"] == "error"
    {
        stop(paths).await?;
    }
    let status = ensure_bridge(paths).await?;
    if status["ready"] != true {
        wait_ready(paths, wait).await?;
    }
    println!("UvA connected. Browser running in the background.");
    if let Some(binary) = binary {
        let status = Command::new(binary)
            .arg("--standalone")
            .current_dir(&directory)
            .env_clear()
            .envs(opencode_environment(paths, &directory)?)
            .status()
            .await?;
        return Ok(status.code().unwrap_or(130));
    }
    Ok(0)
}
