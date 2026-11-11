use anyhow::{Context, Result};
use clap::Parser;
use std::{path::PathBuf, time::Duration};
use uva_opencode::{
    browser::{Driver, ORIGIN},
    launcher,
    paths::Paths,
    transport::ToolMode,
};

#[derive(Parser)]
#[command(
    version,
    about = "Launch OpenCode through your dedicated UvA Chat browser session."
)]
struct Args {
    #[arg(default_value = ".")]
    directory: PathBuf,
    #[arg(long, alias = "setup")]
    login: bool,
    #[arg(long, default_value_t = 600.0)]
    wait: f64,
    #[arg(long, help = "Verify the connection without launching OpenCode.")]
    check: bool,
    #[arg(long, help = "Stop the local helper and its browser.")]
    stop: bool,
    #[arg(
        long,
        help = "Check the bundled browser and OpenCode without UvA login."
    )]
    self_test: bool,
    #[arg(long, hide = true)]
    internal_bridge: bool,
    #[arg(long, hide = true, value_enum, default_value_t = ToolMode::Prompt)]
    tool_mode: ToolMode,
    #[arg(long, hide = true)]
    no_browser: bool,
    #[arg(long, hide = true, default_value = "browser", value_parser = ["browser"])]
    backend: String,
    #[arg(long, hide = true)]
    internal_browser_probe: bool,
    #[arg(long, hide = true, requires = "internal_browser_probe")]
    probe_request: bool,
}

#[tokio::main]
async fn main() {
    #[cfg(windows)]
    {
        let raw: Vec<_> = std::env::args_os().skip(1).collect();
        if raw
            .first()
            .is_some_and(|arg| arg == "--internal-browser-child")
        {
            let result = if raw.get(1).is_some_and(|arg| arg == "--") {
                uva_opencode::browser::windows::run(&raw[2..])
            } else {
                Err(anyhow::anyhow!("Invalid private browser launch request."))
            };
            let code = match result {
                Ok(code) => code,
                Err(error) => {
                    eprintln!("{error}");
                    1
                }
            };
            std::process::exit(code);
        }
    }
    let args = Args::parse();
    let internal = args.internal_bridge;
    let outcome = if internal {
        run(args).await
    } else {
        tokio::select! {
            result = run(args) => result,
            _ = tokio::signal::ctrl_c() => {
                eprintln!("Stopped. The bridge stays available for the next launch.");
                Ok(130)
            }
        }
    };
    let code = match outcome {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    };
    std::process::exit(code);
}

async fn run(args: Args) -> Result<i32> {
    let duration =
        Duration::try_from_secs_f64(args.wait).context("--wait must be positive and finite.")?;
    anyhow::ensure!(
        !duration.is_zero() && tokio::time::Instant::now().checked_add(duration).is_some(),
        "--wait must be positive and finite."
    );
    let paths = Paths::discover()?;
    if args.self_test {
        self_test(&paths).await?;
        return Ok(0);
    }
    if args.internal_bridge {
        launcher::serve(paths, args.tool_mode).await?;
        return Ok(0);
    }
    if args.stop {
        launcher::stop(&paths).await?;
        return Ok(0);
    }
    if args.internal_browser_probe {
        let ua = std::fs::read_to_string(paths.state.join("browser-user-agent.txt")).ok();
        let driver = Driver::launch(
            &paths.browser_executable()?,
            &paths.state.join("browser-profile"),
            true,
            ua.as_deref(),
        )
        .await?;
        let result: Result<()> = async {
            driver.navigate(&format!("{ORIGIN}/chat")).await?;
            let authenticated = driver.logged_in().await;
            println!("{}", serde_json::json!({"browser_started":true,"authenticated":authenticated}));
            if !authenticated {
                let diagnostic = driver.evaluate("async () => {try { const r=await fetch('/api/v1/auth/get-session',{credentials:'same-origin'}); const d=await r.json(); return {host:location.hostname,status:r.status,has_user:Boolean(d&&d.user),has_session:Boolean(d&&d.session)}; } catch { return {host:location.hostname,session_check_failed:true}; }}").await?;
                println!("{diagnostic}");
            }
            if args.probe_request {
                anyhow::ensure!(authenticated, "UvA login is required.");
                let result = driver.fetch(&serde_json::json!({"model":"gpt-6-luna","messages":[{"id":"rust-probe","role":"user","parts":[{"type":"text","text":"Reply with exactly UVA_RUST_OK."}]}]}), "generic").await?;
                let events = uva_opencode::upstream::parse_events(result["stream"].as_str().unwrap_or(""))?;
                let text = uva_opencode::upstream::text_of(&events)?;
                anyhow::ensure!(text.trim().trim_end_matches('.') == "UVA_RUST_OK", "Upstream response did not match the probe marker.");
                println!("{}", serde_json::json!({"upstream_status":result["status"],"marker_found":true}));
            }
            Ok(())
        }.await;
        driver.close().await;
        result?;
        return Ok(0);
    }
    launcher::launch(&paths, &args.directory, args.login, args.check, args.wait).await
}

async fn self_test(paths: &Paths) -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let driver = Driver::launch(
        &paths.bundled_browser_executable()?,
        &scratch.path().join("browser"),
        true,
        None,
    )
    .await?;
    let result = driver.evaluate("1 + 1").await;
    driver.close().await;
    anyhow::ensure!(
        result? == serde_json::json!(2),
        "Bundled Chromium failed its self-test."
    );
    let binary = paths.app.join("vendor").join(if cfg!(windows) {
        "opencode.exe"
    } else {
        "opencode"
    });
    anyhow::ensure!(binary.is_file(), "Bundled OpenCode was not found.");
    let mut command = tokio::process::Command::new(binary);
    command.arg("--version");
    for (name, _) in std::env::vars_os().filter(|(key, _)| {
        key.to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("OPENCODE_")
    }) {
        command.env_remove(name);
    }
    for name in [
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
    ] {
        command.env(name, scratch.path().join(name));
    }
    command.kill_on_drop(true);
    let result = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .context("Bundled OpenCode timed out.")??;
    anyhow::ensure!(result.status.success(), "Bundled OpenCode did not start.");
    println!(
        "{}",
        serde_json::json!({"bundled_browser":"passed","bundled_opencode":String::from_utf8_lossy(&result.stdout).trim(),"external_python_required":false})
    );
    Ok(())
}
