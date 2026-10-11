//! One child, one bounded writer, and an independent reader. RPC responses never
//! wait for Telegram; overload closes the connection instead of losing approvals.
use crate::wire::{Envelope, Id};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    process::Command,
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

pub const FRAME_LIMIT: usize = 2 * 1024 * 1024;
pub const CAPACITY: usize = 128;
#[derive(Debug)]
pub enum Event {
    Message(Envelope),
    Disconnected,
}
enum Write {
    Call {
        method: String,
        params: Value,
        reply: oneshot::Sender<Result<Value>>,
    },
    Message(Value),
}
#[derive(Clone)]
pub struct Rpc {
    tx: mpsc::Sender<Write>,
    stop: watch::Sender<bool>,
    timeout: Duration,
}
pub struct Connection {
    pub rpc: Rpc,
    pub events: mpsc::Receiver<Event>,
    task: JoinHandle<()>,
}

impl Rpc {
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let (reply, rx) = oneshot::channel();
        let operation = async {
            self.tx
                .send(Write::Call {
                    method: method.into(),
                    params,
                    reply,
                })
                .await
                .map_err(|_| anyhow!("Codex disconnected"))?;
            rx.await.context("Codex disconnected")?
        };
        match tokio::time::timeout(self.timeout, operation).await {
            Ok(result) => result,
            Err(_) => {
                self.stop();
                bail!("Codex RPC timed out; connection closed, action was not retried")
            }
        }
    }
    pub async fn send(&self, message: Value) -> Result<()> {
        match tokio::time::timeout(self.timeout, self.tx.send(Write::Message(message))).await {
            Ok(Ok(())) => Ok(()),
            _ => {
                self.stop();
                bail!("Codex writer unavailable")
            }
        }
    }
    pub async fn respond(&self, id: Id, result: Value) -> Result<()> {
        self.send(json!({"id": id, "result":result})).await
    }
    pub async fn reject(&self, id: Id) -> Result<()> {
        self.send(json!({"id":id,"error":{"code":-32601,"message":"Unsupported or unauthorized request"}})).await
    }
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed() || *self.stop.borrow()
    }
}

impl Connection {
    pub async fn spawn(executable: &Path, cwd: &Path, timeout: Duration) -> Result<Self> {
        let mut command = Command::new(executable);
        command
            .args(["app-server", "--listen", "stdio://"])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .env_remove("TELOXIDE_TOKEN")
            .env_remove("TELEGRAM_BOT_TOKEN");
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW; no shell interpolation.
        let mut child = command
            .spawn()
            .context("Unable to start Codex executable")?;
        let stdout = child.stdout.take().unwrap();
        let stdin = child.stdin.take().unwrap();
        let (rpc, events, driver) = connect(stdout, stdin, timeout);
        let task = tokio::spawn(async move {
            driver.await;
            let _ = child.kill().await;
            let _ = child.wait().await;
        });
        let connection = Self { rpc, events, task };
        connection.rpc.call("initialize", json!({"clientInfo":{"name":"telegram_codex_bridge","title":"Telegram Codex Bridge","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":false}})).await?;
        connection.rpc.send(json!({"method":"initialized"})).await?;
        Ok(connection)
    }
    pub async fn shutdown(mut self) {
        self.rpc.stop();
        let _ = (&mut self.task).await;
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.rpc.stop();
    }
}

fn connect<R, W>(
    reader: R,
    writer: W,
    timeout: Duration,
) -> (Rpc, mpsc::Receiver<Event>, impl Future<Output = ()> + Send)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel(CAPACITY);
    let (event_tx, events) = mpsc::channel(CAPACITY);
    let (stop, mut stopped) = watch::channel(false);
    let rpc = Rpc { tx, stop, timeout };
    let driver = async move {
        let mut input = FramedRead::new(reader, LinesCodec::new_with_max_length(FRAME_LIMIT));
        let mut output = FramedWrite::new(writer, LinesCodec::new_with_max_length(FRAME_LIMIT));
        let mut pending: HashMap<Id, oneshot::Sender<Result<Value>>> = HashMap::new();
        let mut next = 0i64;
        loop {
            tokio::select! {
                biased;
                _ = stopped.changed() => break,
                line = input.next() => {
                    let Some(Ok(line)) = line else { break };
                    let Ok(message) = serde_json::from_str::<Envelope>(&line) else { break };
                    if message.method.is_none() {
                        if let Some(id) = message.id {
                            if let Some(reply) = pending.remove(&id) {
                                let result = match message.error { Some(e) => Err(anyhow!("Codex RPC error: {}", e)), None => Ok(message.result.unwrap_or(Value::Null)) };
                                let _ = reply.send(result);
                            }
                        }
                    } else if event_tx.try_send(Event::Message(message)).is_err() { break; }
                }
                request = rx.recv() => {
                    let Some(request) = request else { break };
                    let message = match request {
                        Write::Call { method, params, reply } => {
                            if pending.len() >= CAPACITY { let _ = reply.send(Err(anyhow!("Too many RPC requests"))); continue; }
                            next += 1;
                            let id = Id::Number(next);
                            pending.insert(id.clone(), reply);
                            json!({"id":id,"method":method,"params":params})
                        }
                        Write::Message(v) => v,
                    };
                    let line = message.to_string();
                    if line.len() > FRAME_LIMIT { break; }
                    tokio::select! {
                        _ = stopped.changed() => break,
                        result = tokio::time::timeout(timeout, output.send(line)) => if !matches!(result, Ok(Ok(()))) { break; }
                    }
                }
            }
        }
        for (_, reply) in pending {
            let _ = reply.send(Err(anyhow!(
                "Codex disconnected; request outcome may be unknown"
            )));
        }
        drop(rx);
        let _ = event_tx.try_send(Event::Disconnected);
    };
    (rpc, events, driver)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    #[tokio::test]
    async fn multiplexes_requests_and_server_approvals() {
        let (client, server) = tokio::io::duplex(8192);
        let (r, w) = tokio::io::split(client);
        let (rpc, mut events, driver) = connect(r, w, Duration::from_secs(2));
        let task = tokio::spawn(driver);
        let fake = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(server);
            let mut lines = BufReader::new(r).lines();
            let a: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            let b: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            for v in [
                json!({"id":"approval","method":"item/fileChange/requestApproval","params":{}}),
                json!({"id":b["id"],"result":2}),
                json!({"id":a["id"],"result":1}),
            ] {
                w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
            }
            let answer: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(answer["result"]["decision"], "decline");
        });
        let (a, b) = tokio::join!(rpc.call("a", json!({})), rpc.call("b", json!({})));
        assert_eq!(a.unwrap(), 1);
        assert_eq!(b.unwrap(), 2);
        let Event::Message(m) = events.recv().await.unwrap() else {
            panic!()
        };
        rpc.respond(m.id.unwrap(), json!({"decision":"decline"}))
            .await
            .unwrap();
        fake.await.unwrap();
        task.await.unwrap();
        assert!(rpc.is_closed());
    }
    #[tokio::test]
    async fn timeout_invalidates_connection() {
        let (client, _server) = tokio::io::duplex(128);
        let (r, w) = tokio::io::split(client);
        let (rpc, _events, driver) = connect(r, w, Duration::from_millis(20));
        let task = tokio::spawn(driver);
        assert!(rpc.call("hang", json!({})).await.is_err());
        task.await.unwrap();
        assert!(rpc.is_closed());
    }
    #[tokio::test]
    async fn malformed_input_and_eof_fail_pending_calls() {
        for payload in ["not json\n", ""] {
            let (client, mut server) = tokio::io::duplex(1024);
            let (r, w) = tokio::io::split(client);
            let (rpc, _events, driver) = connect(r, w, Duration::from_secs(1));
            let task = tokio::spawn(driver);
            server.write_all(payload.as_bytes()).await.unwrap();
            drop(server);
            assert!(rpc.call("test", json!({})).await.is_err());
            task.await.unwrap();
        }
    }
}
