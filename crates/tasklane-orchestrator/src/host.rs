//! Runs a plug-in as a child process and talks to it in JSON lines
//! (docs/design/05-plugin-protocol.md). Calls may overlap; responses are matched
//! by id, so they may come back in any order.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tasklane_plugin::{ErrorKind, PluginError, Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

/// How to start a plug-in. The child gets exactly `env` and nothing else.
pub struct PluginSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("plug-in error ({:?}): {}", .0.kind, .0.message)]
    Plugin(PluginError),
    #[error("plug-in did not answer in time")]
    Timeout,
    #[error("plug-in closed its output")]
    Closed,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl CallError {
    /// Worth retrying later?
    pub fn is_transient(&self) -> bool {
        match self {
            CallError::Plugin(e) => matches!(e.kind, ErrorKind::Transient | ErrorKind::RateLimited),
            _ => true,
        }
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>;

pub struct PluginClient {
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    _child: Child,
}

impl PluginClient {
    pub fn spawn(spec: &PluginSpec) -> Result<PluginClient, CallError> {
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .env_clear()
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().ok_or(CallError::Closed)?;
        let stdout = child.stdout.take().ok_or(CallError::Closed)?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // Lines that are not a valid response are ignored; the caller times out.
                if let Ok(resp) = serde_json::from_str::<Response>(&line) {
                    if let Some(tx) = reader_pending.lock().await.remove(&resp.id) {
                        let _ = tx.send(resp);
                    }
                }
            }
            // Output closed: dropping the senders wakes every waiting call.
            reader_pending.lock().await.clear();
        });
        Ok(PluginClient {
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            _child: child,
        })
    }

    pub async fn call(
        &self,
        op: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let mut line = serde_json::to_string(&Request {
            id,
            op: op.to_string(),
            params,
        })
        .expect("request is json");
        line.push('\n');
        {
            let mut stdin = self.stdin.lock().await;
            let written = async {
                stdin.write_all(line.as_bytes()).await?;
                stdin.flush().await
            }
            .await;
            if written.is_err() {
                self.pending.lock().await.remove(&id);
                return Err(CallError::Closed);
            }
        }
        match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(CallError::Timeout)
            }
            Ok(Err(_)) => Err(CallError::Closed),
            Ok(Ok(resp)) if resp.ok => Ok(resp.result.unwrap_or(Value::Null)),
            Ok(Ok(resp)) => Err(CallError::Plugin(resp.error.unwrap_or(PluginError {
                kind: ErrorKind::Permanent,
                message: "error without detail".into(),
            }))),
        }
    }
}
