//! Connections made in the console: where their settings and secrets live, and how they turn into
//! the environment of the plug-in programs.
//!
//! * Secrets (the GitHub App's private key and webhook secret, the model credential, a generated admin
//!   token) are files in `<data dir>/secrets`, mode 0600, in a directory with mode 0700. They are never
//!   put in the database, never shown again after they are saved, and never logged.
//! * Non-secret settings (the App id, its name, where it is installed, how the agent is isolated)
//!   are rows in the `settings` table.
//! * Values given by the operator as environment variables win; the console fills in what is missing.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use tasklane_orchestrator::host::PluginSpec;

pub const GITHUB_KEY: &str = "github_app.pem";
pub const GITHUB_WEBHOOK: &str = "github_webhook_secret";
pub const AGENT_KEY: &str = "anthropic_key";
pub const ADMIN_TOKEN: &str = "admin_token";

#[derive(Clone)]
pub struct Secrets {
    dir: PathBuf,
}

impl Secrets {
    pub fn new(data_dir: &Path) -> std::io::Result<Secrets> {
        let dir = data_dir.join("secrets");
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(Secrets { dir })
    }

    pub fn path(&self, name: &str) -> PathBuf {
        // names are fixed constants; refuse anything that could leave the directory
        assert!(
            !name.contains('/') && !name.contains(".."),
            "bad secret name"
        );
        self.dir.join(name)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.path(name).is_file()
    }

    pub fn read(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.path(name))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Write atomically: a temporary file created with mode 0600, then renamed over the target.
    pub fn write(&self, name: &str, content: &str) -> std::io::Result<()> {
        let target = self.path(name);
        let tmp = self.dir.join(format!(".{name}.tmp"));
        let _ = std::fs::remove_file(&tmp);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &target)
    }

    pub fn delete(&self, name: &str) {
        let _ = std::fs::remove_file(self.path(name));
    }
}

/// The admin token: from the environment, else a stored one, else a new random one (returned with
/// `true` so the caller can show it once).
pub fn admin_token(from_env: Option<String>, secrets: &Secrets) -> std::io::Result<(String, bool)> {
    if let Some(t) = from_env {
        return Ok((t, false));
    }
    if let Some(t) = secrets.read(ADMIN_TOKEN) {
        return Ok((t, false));
    }
    let mut buf = [0u8; 24];
    getrandom::fill(&mut buf).map_err(|e| std::io::Error::other(e.to_string()))?;
    let token: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    secrets.write(ADMIN_TOKEN, &token)?;
    Ok((token, true))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GithubConn {
    pub app_id: String,
    pub slug: String,
    pub owner: String,
    /// `Organization` or `User`, as GitHub reports it; decides which settings page lists the installations.
    #[serde(default)]
    pub owner_type: String,
    pub html_url: String,
    pub installation_id: Option<String>,
}

/// How the agent is isolated.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentSettings {
    /// `docker`, `none` (the user accepted the risk) or empty (not chosen yet).
    #[serde(default)]
    pub sandbox: String,
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub network: String,
    /// `api_key` (default) or `subscription`: a token from `claude setup-token`, for people who
    /// sign in to Claude Code with their Claude plan and have no API key.
    #[serde(default)]
    pub auth: String,
}

impl AgentSettings {
    /// The environment variable the Claude CLI reads the stored credential from.
    pub fn credential_var(&self) -> &'static str {
        if self.auth == "subscription" {
            "CLAUDE_CODE_OAUTH_TOKEN"
        } else {
            "ANTHROPIC_API_KEY"
        }
    }
}

pub async fn get<T: for<'de> Deserialize<'de>>(pool: &PgPool, key: &str) -> Option<T> {
    let v: Option<Value> = sqlx::query_scalar("SELECT value FROM settings WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    v.and_then(|v| serde_json::from_value(v).ok())
}

pub async fn put<T: Serialize>(pool: &PgPool, key: &str, value: &T) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO settings (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = now()")
        .bind(key)
        .bind(serde_json::to_value(value).unwrap_or(Value::Null))
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn remove(pool: &PgPool, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM settings WHERE key = $1")
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

fn has(env: &[(String, String)], name: &str) -> bool {
    env.iter().any(|(k, v)| k == name && !v.is_empty())
}

fn set_missing(env: &mut Vec<(String, String)>, name: &str, value: String) {
    if !has(env, name) {
        env.retain(|(k, _)| k != name);
        env.push((name.to_string(), value));
    }
}

/// The ticket-source plug-in's spec: what the operator gave, plus what the console connected.
pub fn source_spec(base: &PluginSpec, gh: Option<&GithubConn>, secrets: &Secrets) -> PluginSpec {
    let mut spec = base.clone();
    if let Some(gh) = gh {
        set_missing(&mut spec.env, "GITHUB_APP_ID", gh.app_id.clone());
        if secrets.exists(GITHUB_KEY) {
            set_missing(
                &mut spec.env,
                "GITHUB_APP_PRIVATE_KEY_FILE",
                secrets.path(GITHUB_KEY).display().to_string(),
            );
        }
        if let Some(id) = &gh.installation_id {
            set_missing(&mut spec.env, "GITHUB_INSTALLATION_ID", id.clone());
        }
        if let Some(w) = secrets.read(GITHUB_WEBHOOK) {
            set_missing(&mut spec.env, "GITHUB_WEBHOOK_SECRET", w);
        }
    }
    spec
}

/// The agent-engine plug-in's spec.
pub fn engine_spec(base: &PluginSpec, agent: &AgentSettings, secrets: &Secrets) -> PluginSpec {
    let mut spec = base.clone();
    if let Some(key) = secrets.read(AGENT_KEY) {
        if !has(&spec.env, "CLAUDE_CODE_OAUTH_TOKEN") && !has(&spec.env, "ANTHROPIC_API_KEY") {
            set_missing(&mut spec.env, agent.credential_var(), key);
        }
    }
    match agent.sandbox.as_str() {
        "docker" => {
            set_missing(&mut spec.env, "TASKLANE_SANDBOX", "docker".into());
            if !agent.image.is_empty() {
                set_missing(&mut spec.env, "TASKLANE_SANDBOX_IMAGE", agent.image.clone());
            }
            if !agent.network.is_empty() {
                set_missing(
                    &mut spec.env,
                    "TASKLANE_SANDBOX_NETWORK",
                    agent.network.clone(),
                );
            }
        }
        "none" => set_missing(&mut spec.env, "TASKLANE_ALLOW_UNSANDBOXED", "1".into()),
        _ => {}
    }
    spec
}

/// Rebuild both plug-in specs from the current settings and restart the plug-ins.
pub async fn apply(st: &crate::AppState) -> Result<(), tasklane_orchestrator::Error> {
    let gh: Option<GithubConn> = get(st.orch.pool(), "github").await;
    let agent: AgentSettings = get(st.orch.pool(), "agent").await.unwrap_or_default();
    st.orch
        .reconfigure_source(source_spec(&st.base_source, gh.as_ref(), &st.secrets))
        .await?;
    st.orch
        .reconfigure_engine(engine_spec(&st.base_engine, &agent, &st.secrets))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(env: &[(&str, &str)]) -> PluginSpec {
        PluginSpec {
            program: "p".into(),
            args: vec![],
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
    fn names(s: &PluginSpec) -> Vec<String> {
        s.env.iter().map(|(k, _)| k.clone()).collect()
    }

    #[test]
    fn secrets_are_files_with_private_modes_written_atomically() {
        let d = tempfile::tempdir().unwrap();
        let s = Secrets::new(d.path()).unwrap();
        assert_eq!(
            std::fs::metadata(d.path().join("secrets"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        s.write("anthropic_key", "sk-ant-secret\n").unwrap();
        assert_eq!(
            std::fs::metadata(s.path("anthropic_key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(s.read("anthropic_key").as_deref(), Some("sk-ant-secret"));
        s.write("anthropic_key", "second").unwrap();
        assert_eq!(
            s.read("anthropic_key").as_deref(),
            Some("second"),
            "overwrites"
        );
        assert!(
            !d.path().join("secrets/.anthropic_key.tmp").exists(),
            "no temporary file is left"
        );
        s.delete("anthropic_key");
        assert!(s.read("anthropic_key").is_none() && !s.exists("anthropic_key"));
    }

    #[test]
    fn an_empty_file_counts_as_no_secret() {
        let d = tempfile::tempdir().unwrap();
        let s = Secrets::new(d.path()).unwrap();
        s.write("anthropic_key", "  \n").unwrap();
        assert!(s.read("anthropic_key").is_none());
    }

    #[test]
    #[should_panic(expected = "bad secret name")]
    fn a_secret_name_cannot_leave_its_directory() {
        let d = tempfile::tempdir().unwrap();
        Secrets::new(d.path()).unwrap().path("../etc/passwd");
    }

    #[test]
    fn the_admin_token_comes_from_the_environment_then_the_file_then_is_generated_once() {
        let d = tempfile::tempdir().unwrap();
        let s = Secrets::new(d.path()).unwrap();
        assert_eq!(
            admin_token(Some("from-env".into()), &s).unwrap(),
            ("from-env".into(), false)
        );
        let (t, fresh) = admin_token(None, &s).unwrap();
        assert!(fresh && t.len() == 48);
        assert_eq!(
            admin_token(None, &s).unwrap(),
            (t, false),
            "the same token next time, not shown as new"
        );
    }

    #[test]
    fn a_connected_account_fills_in_what_the_operator_did_not_set() {
        let d = tempfile::tempdir().unwrap();
        let s = Secrets::new(d.path()).unwrap();
        s.write(GITHUB_KEY, "PEM").unwrap();
        s.write(GITHUB_WEBHOOK, "wh").unwrap();
        let gh = GithubConn {
            app_id: "42".into(),
            slug: "x".into(),
            owner: "o".into(),
            owner_type: String::new(),
            html_url: String::new(),
            installation_id: Some("7".into()),
        };
        let out = source_spec(&spec(&[("PATH", "/bin")]), Some(&gh), &s);
        let n = names(&out);
        for k in [
            "GITHUB_APP_ID",
            "GITHUB_APP_PRIVATE_KEY_FILE",
            "GITHUB_INSTALLATION_ID",
            "GITHUB_WEBHOOK_SECRET",
        ] {
            assert!(n.contains(&k.to_string()), "{k}");
        }
        let key_path = out
            .env
            .iter()
            .find(|(k, _)| k == "GITHUB_APP_PRIVATE_KEY_FILE")
            .unwrap()
            .1
            .clone();
        assert!(
            key_path.ends_with("secrets/github_app.pem"),
            "the key is passed as a path, never as text"
        );
        // operator values win
        let out = source_spec(&spec(&[("GITHUB_APP_ID", "operator")]), Some(&gh), &s);
        assert_eq!(
            out.env
                .iter()
                .find(|(k, _)| k == "GITHUB_APP_ID")
                .unwrap()
                .1,
            "operator"
        );
        // nothing connected, nothing added
        assert_eq!(
            names(&source_spec(&spec(&[]), None, &s)),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_agent_gets_its_key_and_its_isolation_choice() {
        let d = tempfile::tempdir().unwrap();
        let s = Secrets::new(d.path()).unwrap();
        s.write(AGENT_KEY, "sk-ant-x").unwrap();
        let docker = AgentSettings {
            sandbox: "docker".into(),
            image: "img".into(),
            network: "net".into(),
            auth: String::new(),
        };
        let out = engine_spec(&spec(&[]), &docker, &s);
        let get = |k: &str| out.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("ANTHROPIC_API_KEY").as_deref(), Some("sk-ant-x"));
        assert_eq!(
            (
                get("TASKLANE_SANDBOX").as_deref(),
                get("TASKLANE_SANDBOX_IMAGE").as_deref(),
                get("TASKLANE_SANDBOX_NETWORK").as_deref()
            ),
            (Some("docker"), Some("img"), Some("net"))
        );
        assert!(get("TASKLANE_ALLOW_UNSANDBOXED").is_none());
        let none = engine_spec(
            &spec(&[]),
            &AgentSettings {
                sandbox: "none".into(),
                ..Default::default()
            },
            &s,
        );
        assert_eq!(
            none.env
                .iter()
                .find(|(k, _)| k == "TASKLANE_ALLOW_UNSANDBOXED")
                .unwrap()
                .1,
            "1"
        );
        // not chosen yet: no opt-in is invented
        assert!(
            !names(&engine_spec(&spec(&[]), &AgentSettings::default(), &s))
                .contains(&"TASKLANE_ALLOW_UNSANDBOXED".to_string())
        );
    }

    #[test]
    fn a_subscription_token_is_handed_over_as_the_cli_login_not_as_an_api_key() {
        let d = tempfile::tempdir().unwrap();
        let s = Secrets::new(d.path()).unwrap();
        s.write(AGENT_KEY, "sk-ant-oat-x").unwrap();
        let sub = AgentSettings { auth: "subscription".into(), ..Default::default() };
        let out = engine_spec(&spec(&[]), &sub, &s);
        let get = |k: &str| out.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("CLAUDE_CODE_OAUTH_TOKEN").as_deref(), Some("sk-ant-oat-x"));
        assert_eq!(get("ANTHROPIC_API_KEY"), None);
        let key = engine_spec(&spec(&[]), &AgentSettings::default(), &s);
        assert!(key.env.iter().any(|(n, _)| n == "ANTHROPIC_API_KEY"));
    }
}
