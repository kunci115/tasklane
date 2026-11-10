//! Runs a plug-in as a child process and talks to it in JSON lines
//! (docs/design/05-plugin-protocol.md). Calls may overlap; responses are matched
//! by id, so they may come back in any order.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tasklane_plugin::{ErrorKind, PluginError, Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

/// How to start a plug-in. The child gets exactly `env` and nothing else.
#[derive(Clone)]
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

struct Live {
    /// Did this process declare a protocol version the core knows? Checked once, on the first call.
    verdict: tokio::sync::OnceCell<Result<(), String>>,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    _child: Child,
}

/// A plug-in process and the pipe to it. If the process dies, the call that
/// notices fails with `Closed` and the next call starts it again.
pub struct PluginClient {
    spec: std::sync::Mutex<PluginSpec>,
    live: Mutex<Arc<Live>>,
    next_id: AtomicU64,
}

fn start(spec: &PluginSpec) -> Result<Arc<Live>, CallError> {
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
    let alive = Arc::new(AtomicBool::new(true));
    let (reader_pending, reader_alive) = (pending.clone(), alive.clone());
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
        reader_alive.store(false, Ordering::SeqCst);
        reader_pending.lock().await.clear();
    });
    Ok(Arc::new(Live {
        verdict: tokio::sync::OnceCell::new(),
        stdin: Mutex::new(stdin),
        pending,
        alive,
        _child: child,
    }))
}

impl PluginClient {
    pub fn spawn(spec: &PluginSpec) -> Result<PluginClient, CallError> {
        Ok(PluginClient {
            spec: std::sync::Mutex::new(spec.clone()),
            live: Mutex::new(start(spec)?),
            next_id: AtomicU64::new(1),
        })
    }

    /// The command and environment the plug-in was started with (to run a one-shot helper of the same program).
    pub fn spec(&self) -> PluginSpec {
        self.spec.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replace the plug-in's command or environment (for example after the user connected an account
    /// in the console) and start it again with the new settings. In-flight calls to the old process fail.
    pub async fn reconfigure(&self, spec: PluginSpec) -> Result<(), CallError> {
        let fresh = start(&spec)?;
        *self.spec.lock().unwrap_or_else(|e| e.into_inner()) = spec;
        *self.live.lock().await = fresh;
        Ok(())
    }

    /// The running process, restarted first if the last one died.
    async fn current(&self) -> Result<Arc<Live>, CallError> {
        let mut live = self.live.lock().await;
        if !live.alive.load(Ordering::SeqCst) {
            let spec = self.spec.lock().unwrap_or_else(|e| e.into_inner()).clone();
            eprintln!("plug-in {} stopped; starting it again", spec.program);
            *live = start(&spec)?;
        }
        Ok(live.clone())
    }

    pub async fn call(
        &self,
        op: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        let live = self.current().await?;
        if op != "describe" {
            // The core refuses a plug-in that declares a protocol major it does not know
            // (docs/design/05-plugin-protocol.md). A plug-in that does not say is accepted.
            let verdict = live
                .verdict
                .get_or_init(|| async {
                    match Self::raw_call(
                        &self.next_id,
                        &live,
                        "describe",
                        json!({}),
                        Duration::from_secs(5),
                    )
                    .await
                    {
                        Ok(v) => match v.get("protocol_major").and_then(Value::as_u64) {
                            Some(m) if m != u64::from(tasklane_plugin::PROTOCOL_MAJOR) => {
                                Err(format!(
                                    "the plug-in speaks protocol {m}; this tasklane speaks {}",
                                    tasklane_plugin::PROTOCOL_MAJOR
                                ))
                            }
                            _ => Ok(()),
                        },
                        Err(_) => Ok(()),
                    }
                })
                .await;
            if let Err(message) = verdict {
                return Err(CallError::Plugin(PluginError {
                    kind: ErrorKind::Permanent,
                    message: message.clone(),
                    retry_after_secs: None,
                }));
            }
        }
        Self::raw_call(&self.next_id, &live, op, params, timeout).await
    }

    async fn raw_call(
        next_id: &AtomicU64,
        live: &Arc<Live>,
        op: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        let id = next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        live.pending.lock().await.insert(id, tx);

        let mut line = serde_json::to_string(&Request {
            id,
            op: op.to_string(),
            params,
        })
        .expect("request is json");
        line.push('\n');
        {
            let mut stdin = live.stdin.lock().await;
            let written = async {
                stdin.write_all(line.as_bytes()).await?;
                stdin.flush().await
            }
            .await;
            if written.is_err() {
                live.alive.store(false, Ordering::SeqCst);
                live.pending.lock().await.remove(&id);
                return Err(CallError::Closed);
            }
        }
        match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                live.pending.lock().await.remove(&id);
                Err(CallError::Timeout)
            }
            Ok(Err(_)) => Err(CallError::Closed),
            Ok(Ok(resp)) if resp.ok => Ok(resp.result.unwrap_or(Value::Null)),
            Ok(Ok(resp)) => Err(CallError::Plugin(resp.error.unwrap_or(PluginError {
                kind: ErrorKind::Permanent,
                message: "error without detail".into(),
                retry_after_secs: None,
            }))),
        }
    }
}
