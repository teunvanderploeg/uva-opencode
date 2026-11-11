//! The small CDP request/response subset used by the helper, over inherited pipes.
//! No debugging port, HTTP discovery endpoint, or remotely attachable browser exists.
use anyhow::{Result, anyhow};
use futures::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs::File,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    sync::{broadcast, oneshot},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::codec::{AnyDelimiterCodec, FramedRead};

type Reply = std::result::Result<Value, &'static str>;
#[derive(Default)]
struct Pending {
    closed: bool,
    requests: HashMap<u64, oneshot::Sender<Reply>>,
}
type Shared = Arc<Mutex<Pending>>;

#[derive(Clone, Debug)]
pub struct Event {
    pub method: String,
    pub session: Option<String>,
    pub loader: Option<String>,
    pub name: Option<String>,
}

pub struct Client {
    writer: tokio::sync::Mutex<tokio::fs::File>,
    pending: Shared,
    sequence: AtomicU64,
    events: broadcast::Sender<Event>,
    reader: JoinHandle<()>,
}

fn disconnected(pending: &Shared) {
    let mut state = pending.lock().unwrap_or_else(|error| error.into_inner());
    state.closed = true;
    for (_, reply) in state.requests.drain() {
        let _ = reply.send(Err("Browser control connection closed."));
    }
}

struct RequestGuard {
    id: u64,
    pending: Shared,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .requests
            .remove(&self.id);
    }
}

impl Client {
    pub fn new(reader: File, writer: File) -> Self {
        let pending: Shared = Arc::new(Mutex::new(Pending::default()));
        let dispatch = pending.clone();
        let (events, _) = broadcast::channel(64);
        let notifications = events.clone();
        let reader = tokio::spawn(async move {
            // CDP pipe frames are NUL-delimited JSON. Bound responses and unsolicited events.
            let codec = AnyDelimiterCodec::new_with_max_length(vec![0], vec![0], 64_000_000);
            let mut frames = FramedRead::new(tokio::fs::File::from_std(reader), codec);
            while let Some(Ok(frame)) = frames.next().await {
                let Ok(message) = serde_json::from_slice::<Value>(&frame) else {
                    break;
                };
                if let Some(id) = message["id"].as_u64() {
                    let reply = dispatch
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .requests
                        .remove(&id);
                    if let Some(reply) = reply {
                        // Protocol error messages can contain page data. Return a fixed error instead.
                        let result = if message.get("error").is_some() {
                            Err("Browser command failed.")
                        } else {
                            Ok(message["result"].clone())
                        };
                        let _ = reply.send(result);
                    }
                } else if let Some(method) = message["method"].as_str()
                    && method == "Page.lifecycleEvent"
                    && matches!(
                        message["params"]["name"].as_str(),
                        Some("DOMContentLoaded" | "load")
                    )
                {
                    // Do not retain page URLs, console output, or other session data from CDP events.
                    let _ = notifications.send(Event {
                        method: method.into(),
                        session: message["sessionId"].as_str().map(String::from),
                        loader: message["params"]["loaderId"].as_str().map(String::from),
                        name: message["params"]["name"].as_str().map(String::from),
                    });
                }
            }
            disconnected(&dispatch);
        });
        Self {
            writer: tokio::sync::Mutex::new(tokio::fs::File::from_std(writer)),
            pending,
            sequence: AtomicU64::new(1),
            events,
            reader,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closed
    }

    pub async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        self.call_with_timeout(method, params, session, Duration::from_secs(130))
            .await
    }

    pub async fn call_with_timeout(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
        duration: Duration,
    ) -> Result<Value> {
        timeout(duration, async {
            let id = self.sequence.fetch_add(1, Ordering::Relaxed);
            let (reply, response) = oneshot::channel();
            {
                let mut state = self
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if state.closed {
                    return Err(anyhow!("Browser control connection closed."));
                }
                state.requests.insert(id, reply);
            }
            let _guard = RequestGuard {
                id,
                pending: self.pending.clone(),
            };
            let mut message = json!({"id":id,"method":method,"params":params});
            if let Some(session) = session {
                message["sessionId"] = json!(session);
            }
            let mut frame = serde_json::to_vec(&message)?;
            frame.push(0);
            {
                let mut writer = self.writer.lock().await;
                writer
                    .write_all(&frame)
                    .await
                    .map_err(|_| anyhow!("Browser control connection closed."))?;
                writer
                    .flush()
                    .await
                    .map_err(|_| anyhow!("Browser control connection closed."))?;
            }
            response
                .await
                .map_err(|_| anyhow!("Browser control connection closed."))?
                .map_err(anyhow::Error::msg)
        })
        .await
        .map_err(|_| anyhow!("Browser command timed out."))?
    }

    pub fn close(&self) {
        disconnected(&self.pending);
        self.reader.abort();
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[tokio::test]
    async fn pipe_requests_are_correlated_and_protocol_errors_are_redacted() {
        let (server_read, client_write) = os_pipe::pipe().unwrap();
        let (client_read, server_write) = os_pipe::pipe().unwrap();
        let server = std::thread::spawn(move || {
            let mut input = BufReader::new(server_read);
            let mut output = server_write;
            for error in [false, true] {
                let mut bytes = Vec::new();
                input.read_until(0, &mut bytes).unwrap();
                bytes.pop();
                let message: Value = serde_json::from_slice(&bytes).unwrap();
                let response = if error {
                    json!({"id":message["id"],"error":{"message":"private page contents"}})
                } else {
                    json!({"id":message["id"],"result":{"value":42}})
                };
                output.write_all(response.to_string().as_bytes()).unwrap();
                output.write_all(&[0]).unwrap();
                output.flush().unwrap();
            }
        });
        let client = Client::new(
            super::super::pipe_file(client_read),
            super::super::pipe_file(client_write),
        );
        assert_eq!(
            client
                .call("fixture", json!({}), Some("session"))
                .await
                .unwrap(),
            json!({"value":42})
        );
        assert_eq!(
            client
                .call("fixture", json!({}), None)
                .await
                .unwrap_err()
                .to_string(),
            "Browser command failed."
        );
        server.join().unwrap();
        client.close();
    }
}
