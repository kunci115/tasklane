//! Configuration from environment variables only (twelve-factor). Every value
//! has one documented name; a missing required value is a clear startup error.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use tasklane_orchestrator::host::PluginSpec;

pub struct Settings {
    pub database_url: String,
    pub bind: String,
    pub data_dir: PathBuf,
    /// From the environment, if set. Otherwise one is generated on first start and stored in the data directory.
    pub admin_token: Option<String>,
    /// platform name -> shared secret for report-button tokens
    pub jwt_secrets: HashMap<String, String>,
    pub workers: usize,
    pub sync_every: Duration,
    pub user_daily_limit: i64,
    pub source: PluginSpec,
    pub engine: PluginSpec,
    pub chat: Option<PluginSpec>,
    pub source_name: String,
    pub secure_cookies: bool,
    pub max_dev_parallel: i64,
    pub max_tokens_per_ticket: i64,
    pub metrics_token: Option<String>,
    pub max_in_ci: i64,
    pub duplicate_model: bool,
    pub dev_timeout_secs: u64,
    pub ci_timeout_secs: i64,
    pub allow_no_checks: bool,
    pub retention_days: i64,
    pub public_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} is required")]
    Missing(&'static str),
    #[error("{0}: {1}")]
    Invalid(&'static str, String),
}

fn get(vars: &HashMap<String, String>, k: &'static str) -> Option<String> {
    vars.get(k)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn num<T: std::str::FromStr>(
    vars: &HashMap<String, String>,
    k: &'static str,
    default: T,
) -> Result<T, ConfigError> {
    match get(vars, k) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|_| ConfigError::Invalid(k, format!("{v:?} is not a valid number"))),
    }
}

/// `TASKLANE_<ROLE>_CMD` is the command line (split on spaces); `TASKLANE_<ROLE>_ENV`
/// lists the names of variables to pass through to it. Nothing else is inherited,
/// except PATH.
fn plugin(
    vars: &HashMap<String, String>,
    cmd: &'static str,
    env: &'static str,
) -> Result<Option<PluginSpec>, ConfigError> {
    let Some(line) = get(vars, cmd) else {
        return Ok(None);
    };
    let mut words = line.split_whitespace().map(str::to_string);
    let program = words.next().ok_or(ConfigError::Missing(cmd))?;
    let mut e: Vec<(String, String)> = vec![];
    if let Some(p) = vars.get("PATH") {
        e.push(("PATH".into(), p.clone()));
    }
    for name in get(vars, env)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        if let Some(v) = vars.get(name) {
            e.push((name.to_string(), v.clone()));
        }
    }
    Ok(Some(PluginSpec {
        program,
        args: words.collect(),
        env: e,
    }))
}

/// The plug-ins that ship with tasklane, and the environment variables they may be given. These
/// defaults mean a plain install needs no plug-in settings; everything else is set in the console.
const SOURCE_ENV: &str = "GITHUB_APP_ID,GITHUB_APP_PRIVATE_KEY_FILE,GITHUB_INSTALLATION_ID,GITHUB_WEBHOOK_SECRET,GITHUB_ORG,GITHUB_API_URL";
const ENGINE_ENV: &str = "ANTHROPIC_API_KEY,CLAUDE_CODE_OAUTH_TOKEN,TASKLANE_ALLOW_UNSANDBOXED,TASKLANE_SANDBOX,TASKLANE_SANDBOX_IMAGE,TASKLANE_SANDBOX_NETWORK,TASKLANE_SANDBOX_PROXY,TASKLANE_SANDBOX_CLAUDE_BIN,TASKLANE_CLAUDE_BIN,TASKLANE_CLAUDE_POOL_FILE,TASKLANE_CLAUDE_COOLDOWN_SECS";

fn with_defaults(vars: &HashMap<String, String>) -> HashMap<String, String> {
    let mut v = vars.clone();
    let dir = get(vars, "TASKLANE_PLUGIN_DIR").unwrap_or_else(|| "/opt/tasklane/plugins".into());
    v.entry("TASKLANE_SOURCE_CMD".into())
        .or_insert_with(|| format!("python3 -I {dir}/source-github/source_github.py"));
    v.entry("TASKLANE_ENGINE_CMD".into())
        .or_insert_with(|| format!("python3 -I {dir}/engine-claude/engine_claude.py"));
    v.entry("TASKLANE_SOURCE_ENV".into())
        .or_insert_with(|| SOURCE_ENV.into());
    v.entry("TASKLANE_ENGINE_ENV".into())
        .or_insert_with(|| ENGINE_ENV.into());
    v
}

impl Settings {
    pub fn from_env() -> Result<Settings, ConfigError> {
        Settings::from_map(&std::env::vars().collect())
    }

    pub fn from_map(vars: &HashMap<String, String>) -> Result<Settings, ConfigError> {
        let admin_token = get(vars, "TASKLANE_ADMIN_TOKEN");
        if let Some(t) = &admin_token {
            if t.len() < 24 {
                return Err(ConfigError::Invalid(
                    "TASKLANE_ADMIN_TOKEN",
                    "must be at least 24 characters".into(),
                ));
            }
            if t.chars().collect::<std::collections::HashSet<_>>().len() < 8 {
                return Err(ConfigError::Invalid(
                    "TASKLANE_ADMIN_TOKEN",
                    "looks too repetitive; use a long random string".into(),
                ));
            }
        }
        let mut jwt_secrets = HashMap::new();
        for pair in get(vars, "TASKLANE_JWT_SECRETS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            let (name, secret) = pair.split_once(':').ok_or_else(|| {
                ConfigError::Invalid(
                    "TASKLANE_JWT_SECRETS",
                    "expected name:secret,name:secret".into(),
                )
            })?;
            if secret.len() < 16 {
                return Err(ConfigError::Invalid(
                    "TASKLANE_JWT_SECRETS",
                    format!("secret for {name} must be at least 16 characters"),
                ));
            }
            jwt_secrets.insert(name.to_string(), secret.to_string());
        }
        let public = get(vars, "TASKLANE_PUBLIC_URL").unwrap_or_default();
        Ok(Settings {
            database_url: get(vars, "TASKLANE_DATABASE_URL")
                .ok_or(ConfigError::Missing("TASKLANE_DATABASE_URL"))?,
            bind: get(vars, "TASKLANE_BIND").unwrap_or_else(|| "127.0.0.1:8080".into()),
            data_dir: get(vars, "TASKLANE_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("./data")),
            admin_token,
            jwt_secrets,
            workers: num(vars, "TASKLANE_WORKERS", 2usize)?.max(1),
            sync_every: Duration::from_secs(num(vars, "TASKLANE_SYNC_SECS", 60u64)?.max(5)),
            user_daily_limit: num(vars, "TASKLANE_USER_DAILY_LIMIT", 10i64)?,
            source: plugin(
                &with_defaults(vars),
                "TASKLANE_SOURCE_CMD",
                "TASKLANE_SOURCE_ENV",
            )?
            .ok_or(ConfigError::Missing("TASKLANE_SOURCE_CMD"))?,
            engine: plugin(
                &with_defaults(vars),
                "TASKLANE_ENGINE_CMD",
                "TASKLANE_ENGINE_ENV",
            )?
            .ok_or(ConfigError::Missing("TASKLANE_ENGINE_CMD"))?,
            chat: plugin(vars, "TASKLANE_CHAT_CMD", "TASKLANE_CHAT_ENV")?,
            source_name: get(vars, "TASKLANE_SOURCE_NAME").unwrap_or_else(|| "github".into()),
            secure_cookies: public.starts_with("https://"),
            max_dev_parallel: num(vars, "TASKLANE_MAX_DEV", 1i64)?.max(1),
            max_tokens_per_ticket: num(vars, "TASKLANE_MAX_TOKENS_PER_TICKET", 3_000_000i64)?
                .max(10_000),
            metrics_token: get(vars, "TASKLANE_METRICS_TOKEN").filter(|t| t.len() >= 16),
            duplicate_model: get(vars, "TASKLANE_DUPLICATE_MODEL").as_deref() == Some("1"),
            max_in_ci: num(vars, "TASKLANE_MAX_IN_CI", 5i64)?.max(1),
            dev_timeout_secs: num(vars, "TASKLANE_DEV_TIMEOUT_SECS", 1800u64)?.clamp(60, 86_400),
            ci_timeout_secs: num(vars, "TASKLANE_CI_TIMEOUT_SECS", 3600i64)?.clamp(60, 172_800),
            allow_no_checks: get(vars, "TASKLANE_ALLOW_NO_CHECKS").as_deref() == Some("1"),
            retention_days: num(vars, "TASKLANE_RETENTION_DAYS", 365i64)?.max(7),
            public_url: get(vars, "TASKLANE_PUBLIC_URL")
                .map(|u| u.trim_end_matches('/').to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> HashMap<String, String> {
        [
            ("TASKLANE_DATABASE_URL", "postgres://x"),
            ("TASKLANE_ADMIN_TOKEN", "a-long-enough-admin-token-123"),
            ("TASKLANE_SOURCE_CMD", "python3 /p/source.py --x"),
            ("TASKLANE_ENGINE_CMD", "python3 /p/engine.py"),
            ("TASKLANE_SOURCE_ENV", "GITHUB_APP_ID, GITHUB_MISSING"),
            ("GITHUB_APP_ID", "42"),
            ("OTHER_SECRET", "nope"),
            ("PATH", "/usr/bin"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    #[test]
    fn defaults_and_plugin_environment_are_explicit() {
        let s = Settings::from_map(&base()).unwrap();
        assert_eq!(
            (s.workers, s.bind.as_str(), s.max_dev_parallel),
            (2, "127.0.0.1:8080", 1)
        );
        assert_eq!(s.source.program, "python3");
        assert_eq!(s.source.args, vec!["/p/source.py", "--x"]);
        let names: Vec<&str> = s.source.env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            names,
            vec!["PATH", "GITHUB_APP_ID"],
            "only PATH and the listed variables that exist"
        );
        assert!(s.chat.is_none());
    }

    #[test]
    fn a_plain_install_needs_only_the_database_and_gets_the_shipped_plugins() {
        let vars: HashMap<String, String> = [
            ("TASKLANE_DATABASE_URL", "postgres://x"),
            ("PATH", "/usr/bin"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let s = Settings::from_map(&vars).unwrap();
        assert!(s.admin_token.is_none(), "generated on first start instead");
        assert_eq!(s.source.program, "python3");
        assert_eq!(
            s.source.args,
            vec!["-I", "/opt/tasklane/plugins/source-github/source_github.py"]
        );
        assert_eq!(
            s.engine.args,
            vec!["-I", "/opt/tasklane/plugins/engine-claude/engine_claude.py"]
        );
        assert!(s.public_url.is_none());
    }

    #[test]
    fn the_plugin_directory_and_public_url_can_be_set() {
        let vars: HashMap<String, String> = [
            ("TASKLANE_DATABASE_URL", "postgres://x"),
            ("TASKLANE_PLUGIN_DIR", "/p"),
            ("TASKLANE_PUBLIC_URL", "https://t.example.com/"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let s = Settings::from_map(&vars).unwrap();
        assert_eq!(s.source.args[1], "/p/source-github/source_github.py");
        assert_eq!(s.public_url.as_deref(), Some("https://t.example.com"));
    }

    #[test]
    fn missing_and_weak_values_are_clear_errors() {
        let mut v = base();
        v.remove("TASKLANE_DATABASE_URL");
        assert_eq!(
            Settings::from_map(&v).err().unwrap().to_string(),
            "TASKLANE_DATABASE_URL is required"
        );
        let mut v = base();
        v.insert("TASKLANE_ADMIN_TOKEN".into(), "short".into());
        assert!(Settings::from_map(&v)
            .err()
            .unwrap()
            .to_string()
            .contains("at least 24"));
        let mut v = base();
        v.insert("TASKLANE_JWT_SECRETS".into(), "shop:tiny".into());
        assert!(Settings::from_map(&v)
            .err()
            .unwrap()
            .to_string()
            .contains("at least 16"));
        let mut v = base();
        v.insert("TASKLANE_WORKERS".into(), "many".into());
        assert!(Settings::from_map(&v)
            .err()
            .unwrap()
            .to_string()
            .contains("not a valid number"));
    }

    #[test]
    fn jwt_secrets_parse_per_platform() {
        let mut v = base();
        v.insert(
            "TASKLANE_JWT_SECRETS".into(),
            "shop:0123456789abcdef, hr:fedcba9876543210".into(),
        );
        let s = Settings::from_map(&v).unwrap();
        assert_eq!(s.jwt_secrets.len(), 2);
        assert_eq!(s.jwt_secrets["hr"], "fedcba9876543210");
    }
}
