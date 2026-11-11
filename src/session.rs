use crate::{
    browser::{Driver, ORIGIN},
    error::{Error, ErrorKind, Result},
    paths::{Paths, private_write},
    upstream::{self, Event, Upstream},
};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::{Instant, timeout},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserState {
    Starting,
    LoginRequired,
    Ready,
    Error,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub ready: bool,
    pub browser_state: BrowserState,
    pub browser_error: Option<String>,
    #[serde(skip)]
    _process_id: Option<u32>,
}
impl Status {
    fn new(state: BrowserState, error: Option<String>) -> Self {
        Self {
            ready: state == BrowserState::Ready,
            browser_state: state,
            browser_error: error,
            _process_id: None,
        }
    }
    fn running(state: BrowserState, driver: &Driver) -> Self {
        let mut status = Self::new(state, None);
        status._process_id = driver.process_id();
        status
    }
}

struct Job {
    payload: Value,
    endpoint: String,
    reply: oneshot::Sender<Result<Vec<Event>>>,
    deadline: Instant,
}

#[derive(Clone)]
pub struct Session {
    jobs: mpsc::Sender<Job>,
    status: watch::Receiver<Status>,
    stop: CancellationToken,
}

impl Session {
    pub fn start(paths: Paths, stop: CancellationToken) -> (Self, JoinHandle<()>) {
        Self::start_at(paths, stop, ORIGIN.to_string())
    }

    fn start_at(paths: Paths, stop: CancellationToken, origin: String) -> (Self, JoinHandle<()>) {
        let (jobs, mut receiver) = mpsc::channel::<Job>(4);
        let (status, reader) = watch::channel(Status::new(BrowserState::Starting, None));
        let session = Self {
            jobs,
            status: reader,
            stop: stop.clone(),
        };
        let worker = tokio::spawn(async move {
            let result = run(&paths, &mut receiver, &status, &stop, &origin).await;
            let failure = result.err();
            status.send_replace(Status::new(
                if failure.is_some() {
                    BrowserState::Error
                } else {
                    BrowserState::Stopped
                },
                failure,
            ));
            receiver.close();
            while let Some(job) = receiver.recv().await {
                let _ = job.reply.send(Err(Error::new(
                    ErrorKind::Unavailable,
                    "The browser helper stopped.",
                )));
            }
        });
        (session, worker)
    }

    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }
    pub fn stop(&self) {
        self.stop.cancel();
    }
}

impl Upstream for Session {
    async fn request(&self, payload: Value, endpoint: &str) -> Result<Vec<Event>> {
        if self.stop.is_cancelled() {
            return Err(Error::new(
                ErrorKind::Unavailable,
                "The browser helper stopped.",
            ));
        }
        if !self.status().ready {
            return Err(Error::new(
                ErrorKind::Unavailable,
                "UvA login is required. Complete the login window, then retry.",
            ));
        }
        if !matches!(endpoint, "generic" | "chat") {
            return Err(Error::bad_request("Unknown upstream endpoint."));
        }
        let (reply, receiver) = oneshot::channel();
        self.jobs
            .try_send(Job {
                payload,
                endpoint: endpoint.into(),
                reply,
                deadline: Instant::now() + Duration::from_secs(145),
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => Error::new(
                    ErrorKind::Busy,
                    "The browser request queue is full. Retry after the current request finishes.",
                ),
                mpsc::error::TrySendError::Closed(_) => {
                    Error::new(ErrorKind::Unavailable, "The browser helper stopped.")
                }
            })?;
        timeout(Duration::from_secs(145), receiver)
            .await
            .map_err(|_| Error::new(ErrorKind::Timeout, "The browser request timed out."))?
            .map_err(|_| Error::new(ErrorKind::Unavailable, "The browser helper stopped."))?
    }
}

async fn open(paths: &Paths, headless: bool, origin: &str) -> std::result::Result<Driver, String> {
    let executable = paths.browser_executable().map_err(|e| e.to_string())?;
    let ua = std::fs::read_to_string(paths.state.join("browser-user-agent.txt")).ok();
    let driver = Driver::launch(
        &executable,
        &paths.state.join("browser-profile"),
        headless,
        ua.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Err(error) = driver.navigate(&format!("{origin}/chat")).await {
        driver.close().await;
        return Err(error.to_string());
    }
    Ok(driver)
}

async fn run(
    paths: &Paths,
    jobs: &mut mpsc::Receiver<Job>,
    status: &watch::Sender<Status>,
    stop: &CancellationToken,
    origin: &str,
) -> std::result::Result<(), String> {
    while !stop.is_cancelled() {
        status.send_replace(Status::new(BrowserState::Starting, None));
        let mut driver = open(paths, true, origin).await?;
        if !driver.logged_in_at(origin).await {
            driver.close().await;
            driver = open(paths, false, origin).await?;
            status.send_replace(Status::running(BrowserState::LoginRequired, &driver));
            loop {
                if driver.logged_in_at(origin).await {
                    break;
                }
                if !driver.is_open().await {
                    driver.close().await;
                    return Err("Login window was closed before sign-in finished. Run uva-opencode --login to retry.".into());
                }
                tokio::select! {
                    _ = stop.cancelled() => { driver.close().await; return Ok(()); },
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                }
            }
            let identity = driver.user_agent().await.map_err(|e| e.to_string())?;
            private_write(&paths.state.join("browser-user-agent.txt"), identity)
                .map_err(|_| "Could not save the browser identity.".to_string())?;
            driver.close().await;
            driver = open(paths, true, origin).await?;
            if !driver.logged_in_at(origin).await {
                driver.close().await;
                return Err(
                    "UvA did not retain this login in the browser profile. Try --login again."
                        .into(),
                );
            }
        }
        status.send_replace(Status::running(BrowserState::Ready, &driver));
        let mut last_check = Instant::now();
        let mut lifecycle = tokio::time::interval(Duration::from_secs(2));
        loop {
            let mut job = tokio::select! {
                _ = stop.cancelled() => { driver.close().await; return Ok(()); },
                _ = lifecycle.tick() => {
                    if driver.has_exited() {
                        driver.close().await;
                        return Err("The browser process stopped. Run uva-opencode --login to reconnect.".into());
                    }
                    continue;
                },
                job = jobs.recv() => match job { Some(job) => job, None => { driver.close().await; return Ok(()); } },
            };
            if job.reply.is_closed() {
                continue;
            }
            if Instant::now() >= job.deadline {
                let _ = job.reply.send(Err(Error::new(
                    ErrorKind::Timeout,
                    "The queued browser request expired.",
                )));
                continue;
            }
            if last_check.elapsed() > Duration::from_secs(30) {
                if !driver.logged_in_at(origin).await {
                    status.send_replace(Status::new(BrowserState::LoginRequired, None));
                    let _ = job.reply.send(Err(Error::new(
                        ErrorKind::Unavailable,
                        "UvA session expired. Complete the new login window, then retry.",
                    )));
                    break;
                }
                last_check = Instant::now();
            }
            let result = tokio::select! {
                _ = stop.cancelled() => {
                    let _ = job.reply.send(Err(Error::new(ErrorKind::Unavailable, "The browser helper stopped.")));
                    driver.abort_request().await;
                    driver.close().await;
                    return Ok(());
                },
                _ = job.reply.closed() => { driver.abort_request().await; continue; },
                result = tokio::time::timeout_at(job.deadline, driver.fetch(&job.payload, &job.endpoint)) => result,
            };
            let response = match result {
                Err(_) => {
                    driver.abort_request().await;
                    Err(Error::new(
                        ErrorKind::Timeout,
                        "The browser request timed out.",
                    ))
                }
                Ok(Err(_)) => {
                    if !driver.is_open().await {
                        let _ = job.reply.send(Err(Error::new(
                            ErrorKind::Unavailable,
                            "The browser closed. Run uva-opencode --login to reconnect.",
                        )));
                        driver.close().await;
                        return Err(
                            "The browser closed. Run uva-opencode --login to reconnect.".into()
                        );
                    }
                    Err(Error::bridge(
                        "UvA browser request failed. Check login and network.",
                    ))
                }
                Ok(Ok(response)) => {
                    let code = response["status"].as_u64().unwrap_or(0);
                    if code == 401 || code == 403 {
                        status.send_replace(Status::new(BrowserState::LoginRequired, None));
                        let _ = job.reply.send(Err(Error::new(
                            ErrorKind::Unavailable,
                            "UvA session expired. Complete the new login window, then retry.",
                        )));
                        break;
                    }
                    if code != 200 {
                        Err(Error::bridge(format!("UvA returned HTTP {code}.")))
                    } else {
                        upstream::parse_events(response["stream"].as_str().unwrap_or(""))
                    }
                }
            };
            let _ = job.reply.send(response);
        }
        // All queued work belongs to the old login; do not execute it after reauthentication.
        while let Ok(job) = jobs.try_recv() {
            let _ = job.reply.send(Err(Error::new(
                ErrorKind::Unavailable,
                "UvA session expired. Retry after login.",
            )));
        }
        driver.close().await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(state: BrowserState) -> (Session, mpsc::Receiver<Job>) {
        let (jobs, receiver) = mpsc::channel(4);
        let (_, status) = watch::channel(Status::new(state, None));
        (
            Session {
                jobs,
                status,
                stop: CancellationToken::new(),
            },
            receiver,
        )
    }

    #[tokio::test]
    async fn login_and_stopped_states_never_queue_requests() {
        let (session, receiver) = fixture(BrowserState::LoginRequired);
        assert_eq!(
            session
                .request(json!({}), "generic")
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unavailable
        );
        assert!(receiver.is_empty());
        let (session, receiver) = fixture(BrowserState::Ready);
        session.stop();
        assert_eq!(
            session
                .request(json!({}), "generic")
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unavailable
        );
        assert!(receiver.is_empty());
    }

    #[tokio::test]
    async fn queue_is_bounded_and_abandoned_jobs_are_detectable() {
        let (session, mut receiver) = fixture(BrowserState::Ready);
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let session = session.clone();
            tasks.push(tokio::spawn(async move {
                session.request(json!({"model":"fixture"}), "generic").await
            }));
        }
        tokio::task::yield_now().await;
        assert_eq!(receiver.len(), 4);
        assert_eq!(
            session
                .request(json!({}), "generic")
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Busy
        );
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
        while let Ok(job) = receiver.try_recv() {
            assert!(job.reply.is_closed());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_includes_time_spent_waiting_in_the_queue() {
        let (session, mut receiver) = fixture(BrowserState::Ready);
        let request = tokio::spawn(async move { session.request(json!({}), "generic").await });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(146)).await;
        assert_eq!(request.await.unwrap().unwrap_err().kind, ErrorKind::Timeout);
        assert!(receiver.recv().await.unwrap().reply.is_closed());
    }

    #[tokio::test]
    #[ignore = "requires bundled Chromium and a desktop; closes only disposable fixture browsers"]
    async fn closed_login_window_and_idle_browser_exit_become_recoverable_errors() {
        use axum::{Json, Router, routing::get};
        for authenticated in [false, true] {
            let app = Router::new()
                .route("/chat", get(|| async { "fixture login" }))
                .route(
                    "/api/v1/auth/get-session",
                    get(move || async move {
                        Json(if authenticated {
                            json!({"user":{},"session":{}})
                        } else {
                            Value::Null
                        })
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let scratch = tempfile::tempdir().unwrap();
            let mut paths = Paths::discover().unwrap();
            paths.state = scratch.path().to_path_buf();
            let (session, worker) = Session::start_at(paths, CancellationToken::new(), origin);
            let target = if authenticated {
                BrowserState::Ready
            } else {
                BrowserState::LoginRequired
            };
            timeout(Duration::from_secs(15), async {
                while session.status().browser_state != target {
                    assert_ne!(session.status().browser_state, BrowserState::Error);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            assert!(
                !scratch
                    .path()
                    .join("browser-profile/DevToolsActivePort")
                    .exists()
            );
            let pid = session.status()._process_id.unwrap();
            #[cfg(unix)]
            {
                // This PID belongs to this test's disposable profile, never a user's browser.
                assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
            }
            #[cfg(windows)]
            {
                assert!(
                    std::process::Command::new("taskkill")
                        .args(["/PID", &pid.to_string(), "/F"])
                        .output()
                        .unwrap()
                        .status
                        .success()
                );
            }
            timeout(Duration::from_secs(15), worker)
                .await
                .unwrap()
                .unwrap();
            let status = session.status();
            assert_eq!(status.browser_state, BrowserState::Error);
            assert!(!status.ready);
            let message = status.browser_error.unwrap();
            assert!(
                message.contains(if authenticated {
                    "browser process stopped"
                } else {
                    "Login window was closed"
                }),
                "{message}"
            );
            server.abort();
        }
    }

    #[tokio::test]
    #[ignore = "requires bundled Chromium and a desktop; uses a local fixture only"]
    async fn lifecycle_recovers_from_expiry_cancels_fetches_and_closes_the_profile() {
        use axum::{
            Json, Router,
            http::{HeaderMap, StatusCode, header},
            response::IntoResponse,
            routing::{get, post},
        };
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let expired = Arc::new(AtomicBool::new(true));
        let auth_state = expired.clone();
        let login_state = expired.clone();
        let login_allowed = Arc::new(AtomicBool::new(false));
        let allow_login = login_allowed.clone();
        let upstream_state = expired.clone();
        let pending = Arc::new(tokio::sync::Notify::new());
        let upstream_pending = pending.clone();
        let app = Router::new()
            .route(
                "/chat",
                get(|| async {
                    axum::response::Html(
                        "<script>setInterval(() => fetch('/fixture-login'), 100)</script>",
                    )
                }),
            )
            .route(
                "/fixture-login",
                get(move || {
                    let state = login_state.clone();
                    let allowed = allow_login.clone();
                    async move {
                        if !allowed.load(Ordering::SeqCst) {
                            return StatusCode::UNAUTHORIZED.into_response();
                        }
                        state.store(false, Ordering::SeqCst);
                        (
                            [(
                                header::SET_COOKIE,
                                "fixture=valid; HttpOnly; SameSite=Strict; Max-Age=3600; Path=/",
                            )],
                            "logged in",
                        )
                            .into_response()
                    }
                }),
            )
            .route(
                "/api/v1/auth/get-session",
                get(move |headers: HeaderMap| {
                    let state = auth_state.clone();
                    async move {
                        let valid = !state.load(Ordering::SeqCst)
                            && headers.get(header::COOKIE).is_some_and(|v| {
                                v.to_str().unwrap_or("").contains("fixture=valid")
                            });
                        Json(if valid {
                            json!({"user":{},"session":{}})
                        } else {
                            Value::Null
                        })
                    }
                }),
            )
            .route(
                "/api/v1/chat/generic",
                post(move |Json(body): Json<Value>| {
                    let state = upstream_state.clone();
                    let pending = upstream_pending.clone();
                    async move {
                        if body["model"] == "expire" {
                            state.store(true, Ordering::SeqCst);
                            return StatusCode::UNAUTHORIZED.into_response();
                        }
                        if body["model"] == "pending" {
                            pending.notify_one();
                            std::future::pending::<()>().await;
                        }
                        "data: {\"type\":\"text-delta\",\"delta\":\"fixture-ok\"}\n\n"
                            .into_response()
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let scratch = tempfile::tempdir().unwrap();
        let mut paths = Paths::discover().unwrap();
        paths.state = scratch.path().to_path_buf();
        let (session, worker) = Session::start_at(paths, CancellationToken::new(), origin);
        async fn wait_state(session: &Session, state: BrowserState) {
            timeout(Duration::from_secs(60), async {
                loop {
                    let status = session.status();
                    assert_ne!(status.browser_state, BrowserState::Error, "{status:?}");
                    if status.browser_state == state && status._process_id.is_some() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "Waiting for {state:?}; current state: {:?}",
                    session.status()
                )
            });
        }
        wait_state(&session, BrowserState::LoginRequired).await;
        login_allowed.store(true, Ordering::SeqCst);
        wait_state(&session, BrowserState::Ready).await;
        login_allowed.store(false, Ordering::SeqCst);
        assert!(scratch.path().join("browser-user-agent.txt").is_file());
        // A dropped client aborts its fetch; the following request must not wait for the upstream timer.
        let pending_session = session.clone();
        let request = tokio::spawn(async move {
            pending_session
                .request(json!({"model":"pending"}), "generic")
                .await
        });
        timeout(Duration::from_secs(5), pending.notified())
            .await
            .unwrap();
        request.abort();
        let _ = request.await;
        let events = timeout(
            Duration::from_secs(5),
            session.request(json!({"model":"normal"}), "generic"),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(upstream::text_of(&events).unwrap(), "fixture-ok");
        let error = session
            .request(json!({"model":"expire"}), "generic")
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unavailable);
        wait_state(&session, BrowserState::LoginRequired).await;
        login_allowed.store(true, Ordering::SeqCst);
        wait_state(&session, BrowserState::Ready).await;
        let events = session
            .request(json!({"model":"normal"}), "generic")
            .await
            .unwrap();
        assert_eq!(upstream::text_of(&events).unwrap(), "fixture-ok");
        session.stop();
        timeout(Duration::from_secs(15), worker)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.status().browser_state, BrowserState::Stopped);
        let lock = std::fs::OpenOptions::new()
            .append(true)
            .open(scratch.path().join("browser-profile/.uva-owner.lock"))
            .unwrap();
        assert!(lock.try_lock().is_ok());
        server.abort();
    }
}
