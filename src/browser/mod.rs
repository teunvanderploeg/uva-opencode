mod cdp;
#[cfg(windows)]
pub mod windows;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::{ffi::OsString, fs::File, path::Path, process::Stdio, time::Duration};
use tokio::{
    process::{Child, Command},
    time::timeout,
};

pub const ORIGIN: &str = "https://aichat.uva.nl";
const AUTH: &str = "async () => { try { const r = await fetch('/api/v1/auth/get-session', {credentials:'same-origin',redirect:'error'}); if (!r.ok) return false; const d = await r.json(); return Boolean(d && d.user && d.session); } catch { return false; } }";
const FETCH: &str = include_str!("fetch.js");

#[cfg(unix)]
fn pipe_file(pipe: impl Into<std::os::fd::OwnedFd>) -> File {
    File::from(pipe.into())
}
#[cfg(windows)]
fn pipe_file(pipe: impl Into<std::os::windows::io::OwnedHandle>) -> File {
    File::from(pipe.into())
}

/// Chromium and its private CDP pipes have one owner. There is no debugging TCP port.
pub struct Driver {
    child: Child,
    cdp: cdp::Client,
    target: String,
    session: String,
    _profile_lock: File,
}

impl Driver {
    pub async fn launch(
        executable: &Path,
        profile: &Path,
        headless: bool,
        user_agent: Option<&str>,
    ) -> Result<Self> {
        crate::paths::private_dir(profile)?;
        let profile_lock = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(profile.join(".uva-owner.lock"))?;
        profile_lock
            .try_lock()
            .map_err(|_| anyhow!("This browser profile is already in use."))?;
        let mut args: Vec<OsString> = [
            "--remote-debugging-pipe",
            "--no-first-run",
            "--no-default-browser-check",
            "--enable-automation",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--disable-background-timer-throttling",
            "--disable-renderer-backgrounding",
            "--password-store=basic",
            "--use-mock-keychain",
            "--window-size=1100,800",
            "--lang=en-US",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let mut data = OsString::from("--user-data-dir=");
        data.push(dunce::canonicalize(profile)?);
        args.push(data);
        if headless {
            args.push("--headless=new".into());
        }
        if let Some(ua) = user_agent {
            args.push(format!("--user-agent={ua}").into());
        }
        args.push("about:blank".into());
        let (child_read, parent_write) = os_pipe::pipe()?;
        let (parent_read, child_write) = os_pipe::pipe()?;
        #[cfg(unix)]
        let mut command = {
            use command_fds::{CommandFdExt, FdMapping};
            let mut command = Command::new(executable);
            command
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command.as_std_mut().fd_mappings(vec![
                FdMapping {
                    parent_fd: child_read.into(),
                    child_fd: 3,
                },
                FdMapping {
                    parent_fd: child_write.into(),
                    child_fd: 4,
                },
            ])?;
            command
        };
        #[cfg(windows)]
        let mut command = {
            let mut helper = dunce::canonicalize(std::env::current_exe()?)?;
            // Cargo's library and integration test harnesses are not executable entry points.
            if helper
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "deps" || name == "examples")
            {
                helper = helper
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("uva-opencode.exe");
                anyhow::ensure!(
                    helper.is_file(),
                    "Build the uva-opencode binary before running browser tests or examples."
                );
            }
            let mut command = Command::new(helper);
            command
                .args(["--internal-browser-child", "--"])
                .arg(executable)
                .args(&args)
                .stdin(child_read)
                .stdout(child_write)
                .stderr(Stdio::null())
                .creation_flags(0x08000000);
            command
        };
        command.kill_on_drop(true);
        let child = command.spawn().map_err(|_| {
            anyhow!("Chromium could not start. Check its installation and profile ownership.")
        })?;
        // Close the parent's copies of the inherited child ends, including command-fds' mappings.
        drop(command);
        let cdp = cdp::Client::new(pipe_file(parent_read), pipe_file(parent_write));
        let mut driver = Self {
            child,
            cdp,
            target: String::new(),
            session: String::new(),
            _profile_lock: profile_lock,
        };
        let result: Result<()> = async {
            driver
                .cdp
                .call_with_timeout(
                    "Browser.getVersion",
                    json!({}),
                    None,
                    Duration::from_secs(90),
                )
                .await?;
            driver
                .cdp
                .call(
                    "Browser.setDownloadBehavior",
                    json!({"behavior":"deny"}),
                    None,
                )
                .await?;
            let target = driver
                .cdp
                .call("Target.createTarget", json!({"url":"about:blank"}), None)
                .await?;
            driver.target = target["targetId"]
                .as_str()
                .context("Chromium returned no page ID.")?
                .into();
            let session = driver
                .cdp
                .call(
                    "Target.attachToTarget",
                    json!({"targetId":driver.target,"flatten":true}),
                    None,
                )
                .await?;
            driver.session = session["sessionId"]
                .as_str()
                .context("Chromium returned no page session.")?
                .into();
            driver
                .cdp
                .call("Page.enable", json!({}), Some(&driver.session))
                .await?;
            driver
                .cdp
                .call(
                    "Page.setLifecycleEventsEnabled",
                    json!({"enabled":true}),
                    Some(&driver.session),
                )
                .await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            driver.close().await;
            return Err(anyhow!(
                "Chromium did not initialize its private control connection. Check its installation and whether this profile is already open."
            ));
        }
        Ok(driver)
    }

    pub async fn navigate(&self, url: &str) -> Result<()> {
        timeout(Duration::from_secs(60), async {
            let mut events = self.cdp.subscribe();
            let result = self
                .cdp
                .call("Page.navigate", json!({"url":url}), Some(&self.session))
                .await?;
            if result.get("errorText").is_some() || result["isDownload"] == true {
                return Err(anyhow!(
                    "Browser navigation failed. Check your network connection."
                ));
            }
            let Some(loader) = result["loaderId"].as_str() else {
                return Ok(());
            };
            loop {
                let event = events
                    .recv()
                    .await
                    .map_err(|_| anyhow!("Browser navigation failed."))?;
                if event.session.as_deref() == Some(&self.session)
                    && event.method == "Page.lifecycleEvent"
                    && event.loader.as_deref() == Some(loader)
                    && matches!(event.name.as_deref(), Some("DOMContentLoaded" | "load"))
                {
                    break;
                }
            }
            Ok(())
        })
        .await
        .context("Browser navigation timed out.")?
    }

    pub async fn evaluate(&self, expression: impl Into<String>) -> Result<Value> {
        let expression = expression.into();
        let expression = if expression.trim_start().starts_with("async ")
            || expression.trim_start().starts_with("() =>")
        {
            format!("({expression})()")
        } else {
            expression
        };
        let result = self
            .cdp
            .call(
                "Runtime.evaluate",
                json!({"expression":expression,"awaitPromise":true,"returnByValue":true}),
                Some(&self.session),
            )
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err(anyhow!("Browser operation failed."));
        }
        result["result"]
            .get("value")
            .cloned()
            .context("Browser returned an invalid result.")
    }

    pub async fn logged_in(&self) -> bool {
        self.logged_in_at(ORIGIN).await
    }
    pub(crate) async fn logged_in_at(&self, origin: &str) -> bool {
        self.evaluate("location.origin")
            .await
            .ok()
            .as_ref()
            .and_then(Value::as_str)
            == Some(origin)
            && self.evaluate(AUTH).await.ok() == Some(json!(true))
    }
    pub async fn user_agent(&self) -> Result<String> {
        self.evaluate("navigator.userAgent")
            .await?
            .as_str()
            .map(String::from)
            .context("Browser did not return its user agent.")
    }
    pub async fn is_open(&self) -> bool {
        self.cdp
            .call(
                "Target.getTargetInfo",
                json!({"targetId":self.target}),
                None,
            )
            .await
            .is_ok()
    }
    pub fn has_exited(&mut self) -> bool {
        self.cdp.is_closed() || !matches!(self.child.try_wait(), Ok(None))
    }
    pub(crate) fn process_id(&self) -> Option<u32> {
        self.child.id()
    }

    pub async fn fetch(&self, payload: &Value, endpoint: &str) -> Result<Value> {
        anyhow::ensure!(
            matches!(endpoint, "generic" | "chat"),
            "Unknown upstream endpoint."
        );
        let args = serde_json::to_string(&json!({"endpoint":endpoint,"payload":payload}))?;
        self.evaluate(format!("({FETCH})({args})")).await
    }
    pub async fn abort_request(&self) {
        let _ = timeout(
            Duration::from_secs(2),
            self.evaluate("globalThis.__uvaRequest?.abort(); true"),
        )
        .await;
    }
    pub async fn close(mut self) {
        let _ = timeout(
            Duration::from_secs(5),
            self.cdp.call("Browser.close", json!({}), None),
        )
        .await;
        if timeout(Duration::from_secs(5), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
        self.cdp.close();
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.cdp.close();
    }
}
