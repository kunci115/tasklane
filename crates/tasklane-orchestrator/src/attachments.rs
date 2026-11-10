//! Pictures and PDFs attached to an issue. The daemon downloads them through the source plug-in (the agent
//! has no network and no GitHub key), keeps them in a private directory for the length of one run, and tells
//! the engine where they are so the agent can open them with its Read tool. What they show is data from the
//! reporter, like the issue text; a stranger's files never reach the developer.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::{ctx::Ticket, Orchestrator};

/// Files downloaded for one run; removed when the run ends.
pub(crate) struct Attached {
    pub files: Vec<Value>,
    dir: PathBuf,
}

impl Attached {
    pub fn json(&self) -> Value {
        Value::Array(self.files.clone())
    }
}

impl Orchestrator {
    /// Download what the issue shows. Never fails a run: when the plug-in cannot do it, the agent works
    /// from the text alone. A repository turns this off with `"attachments": false` in its profile.
    pub(crate) async fn attachments_for(&self, t: &Ticket, role: &str) -> Attached {
        let dir = self
            .cfg
            .data_dir
            .join("attachments")
            .join(format!("t{}-{role}", t.id));
        let none = Attached {
            files: vec![],
            dir: dir.clone(),
        };
        if t.agent_profile.get("attachments").and_then(Value::as_bool) == Some(false) {
            return none;
        }
        let _ = std::fs::remove_dir_all(&dir);
        if let Some(parent) = dir.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return none;
            }
        }
        let r = self
            .source_call(
                "fetch_attachments",
                json!({"service": t.service, "ref": t.external_ref, "dest": dir.to_string_lossy(),
                       "trusted_only": role == "dev", "max_files": 5}),
            )
            .await;
        match r {
            Ok(v) => Attached {
                files: v
                    .get("files")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                dir,
            },
            Err(e) => {
                // an older plug-in without the operation, or a network problem: not worth stopping the work
                eprintln!("attachments: ticket {}: {e}", t.id);
                none
            }
        }
    }

    pub(crate) fn drop_attachments(&self, a: &Attached) {
        let _ = std::fs::remove_dir_all(&a.dir);
    }
}
