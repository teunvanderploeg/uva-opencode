//! Manual fresh-account login acceptance check, without occupying the API port.
use anyhow::{Result, bail};
use serde_json::json;
use std::time::Duration;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;
use uva_opencode::{
    paths::Paths,
    session::{BrowserState, Session},
    upstream::{self, Upstream},
};

#[tokio::main]
async fn main() -> Result<()> {
    let (session, worker) = Session::start(Paths::discover()?, CancellationToken::new());
    let result: Result<()> = async {
        let deadline = Instant::now() + Duration::from_secs(600);
        let mut prompted = false;
        loop {
            let status = session.status();
            match status.browser_state {
                BrowserState::Ready => break,
                BrowserState::Error => bail!("{}", status.browser_error.unwrap_or_default()),
                BrowserState::LoginRequired if !prompted => {
                    println!("Complete your UvA sign-in and MFA in the Chromium window.");
                    prompted = true;
                },
                _ => {},
            }
            anyhow::ensure!(Instant::now() < deadline, "Fresh login check timed out.");
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let payload=json!({"model":"gpt-6-luna","messages":[{"id":"fresh-login-check","role":"user","parts":[{"type":"text","text":"Reply with exactly UVA_RUST_OK."}]}]});
        let events=session.request(payload,"generic").await?;
        anyhow::ensure!(upstream::text_of(&events)?.trim().trim_end_matches('.') == "UVA_RUST_OK", "Unexpected upstream reply.");
        println!("{}",json!({"fresh_login":"passed","headless_reuse":"passed","authenticated_request":"passed"}));
        Ok(())
    }.await;
    session.stop();
    if timeout(Duration::from_secs(20), worker).await.is_err() {
        bail!("Browser shutdown timed out.");
    }
    result
}
