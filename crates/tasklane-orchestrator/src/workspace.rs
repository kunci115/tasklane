//! Git work for the development step: a bare mirror per service, and one
//! worktree per run.
//!
//! Safety rules, because the development agent controls the worktree's files:
//! * Git is never run "inside" the agent's directory in the usual way. Commands
//!   that touch the worktree name its git directory and work tree explicitly
//!   (taken from the mirror before the agent starts), and switch off every config
//!   key that can run a program (`core.fsmonitor`, `core.hooksPath`, ...), so a
//!   planted `.git` directory or file cannot execute anything.
//! * Credentials never appear on a command line: a URL carrying a token is split,
//!   and the token goes in as an HTTP header through git's environment config.
//! * Every git call has a time limit and is killed with its process group on drop.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::Error;

const GIT_TIMEOUT: Duration = Duration::from_secs(600);

/// Settings that stop git from running programs named in repository config.
const HARDEN: [&str; 12] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.untrackedCache=false",
    "-c",
    "protocol.ext.allow=never",
    "-c",
    "core.sshCommand=false",
    "-c",
    "diff.external=",
];

#[derive(Clone)]
pub struct Workspaces {
    root: PathBuf,
}

pub struct Prepared {
    pub mirror: PathBuf,
    pub worktree: PathBuf,
    /// The worktree's administrative directory inside the mirror (trusted; the agent cannot change it).
    pub gitdir: PathBuf,
    pub branch: String,
}

/// Split `https://user:token@host/path` into the clean URL and the `user:token` pair.
fn split_credentials(url: &str) -> (String, Option<String>) {
    if let (Some(i), Some(j)) = (url.find("://"), url.rfind('@')) {
        let after_scheme = &url[i + 3..];
        let host_part_end = after_scheme
            .find('/')
            .map(|p| p + i + 3)
            .unwrap_or(url.len());
        if j > i && j < host_part_end {
            return (
                format!("{}{}", &url[..i + 3], &url[j + 1..]),
                Some(url[i + 3..j].to_string()),
            );
        }
    }
    (url.to_string(), None)
}

/// Remove the credential from a URL before it can reach a log line.
pub fn scrub_url(url: &str) -> String {
    match split_credentials(url) {
        (clean, Some(_)) => {
            let i = clean.find("://").map(|i| i + 3).unwrap_or(0);
            format!("{}***@{}", &clean[..i], &clean[i..])
        }
        (clean, None) => clean,
    }
}

struct Run<'a> {
    dir: &'a Path,
    /// (git dir, work tree) to use instead of discovering them from `dir`.
    explicit: Option<(&'a Path, &'a Path)>,
    credentials: Option<String>,
}

async fn run(r: Run<'_>, args: &[&str]) -> Result<String, Error> {
    let mut cmd = Command::new("git");
    cmd.args(HARDEN).args(args).current_dir(r.dir);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", r.dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tasklane")
        .env("GIT_AUTHOR_EMAIL", "tasklane@users.noreply.invalid")
        .env("GIT_COMMITTER_NAME", "tasklane")
        .env("GIT_COMMITTER_EMAIL", "tasklane@users.noreply.invalid");
    if let Some((gitdir, tree)) = r.explicit {
        cmd.env("GIT_DIR", gitdir).env("GIT_WORK_TREE", tree);
    }
    // Slow or stalled transfers fail instead of hanging forever.
    let mut config: Vec<(&str, String)> = vec![
        ("http.lowSpeedLimit", "1000".into()),
        ("http.lowSpeedTime", "60".into()),
    ];
    if let Some(c) = &r.credentials {
        config.push((
            "http.extraHeader",
            format!("Authorization: Basic {}", STANDARD.encode(c)),
        ));
    }
    cmd.env("GIT_CONFIG_COUNT", config.len().to_string());
    for (i, (k, v)) in config.iter().enumerate() {
        cmd.env(format!("GIT_CONFIG_KEY_{i}"), k)
            .env(format!("GIT_CONFIG_VALUE_{i}"), v);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = match tokio::time::timeout(GIT_TIMEOUT, cmd.output()).await {
        Err(_) => {
            return Err(Error::Git(format!(
                "git {} timed out",
                args.first().copied().unwrap_or("")
            )))
        }
        Ok(Err(e)) => return Err(Error::Git(e.to_string())),
        Ok(Ok(o)) => o,
    };
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let mut msg = format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr)
        );
        if let Some(c) = r.credentials.as_deref() {
            for part in c.split(':') {
                if part.len() > 3 {
                    msg = msg.replace(part, "***");
                }
            }
        }
        Err(Error::Git(msg.trim().to_string()))
    }
}

async fn git(dir: &Path, args: &[&str]) -> Result<String, Error> {
    run(
        Run {
            dir,
            explicit: None,
            credentials: None,
        },
        args,
    )
    .await
}

async fn git_net(dir: &Path, args: &[&str], credentials: Option<String>) -> Result<String, Error> {
    run(
        Run {
            dir,
            explicit: None,
            credentials,
        },
        args,
    )
    .await
}

impl Workspaces {
    pub fn new(root: PathBuf) -> Self {
        Workspaces { root }
    }

    fn mirror_path(&self, service: &str) -> PathBuf {
        let h = format!("{:x}", Sha256::digest(service.as_bytes()));
        self.root.join("mirrors").join(format!("{}.git", &h[..16]))
    }

    async fn ensure_mirror(&self, service: &str) -> Result<PathBuf, Error> {
        let mirror = self.mirror_path(service);
        tokio::fs::create_dir_all(&mirror)
            .await
            .map_err(|e| Error::Git(e.to_string()))?;
        if !mirror.join("HEAD").exists() {
            git(&mirror, &["init", "--bare", "-q", "."]).await?;
        }
        Ok(mirror)
    }

    /// Remove any worktree (registered or leftover on disk) that holds `branch`,
    /// so a crashed earlier run cannot block this one.
    async fn clear_branch(&self, mirror: &Path, branch: &str) {
        if let Ok(list) = git(mirror, &["worktree", "list", "--porcelain"]).await {
            let mut path: Option<String> = None;
            for line in list.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    path = Some(p.to_string());
                } else if line == format!("branch refs/heads/{branch}") {
                    if let Some(p) = &path {
                        if Path::new(p) != mirror {
                            let _ = git(mirror, &["worktree", "remove", "--force", p]).await;
                            let _ = tokio::fs::remove_dir_all(p).await;
                        }
                    }
                }
            }
        }
        let _ = git(mirror, &["worktree", "prune"]).await;
    }

    /// Update the mirror from `url` and make a worktree on `branch`, starting
    /// from the branch itself if it exists in the mirror, else from `base`.
    pub async fn prepare(
        &self,
        service: &str,
        url: &str,
        base: &str,
        branch: &str,
        run_id: i64,
    ) -> Result<Prepared, Error> {
        let mirror = self.ensure_mirror(service).await?;
        let (clean, creds) = split_credentials(url);
        // Branches we made ourselves (refs/heads/tasklane/*) stay; everything else mirrors the source.
        git_net(
            &mirror,
            &[
                "fetch",
                "--prune",
                "-q",
                &clean,
                "+refs/heads/*:refs/remotes/origin/*",
            ],
            creds,
        )
        .await?;
        self.clear_branch(&mirror, branch).await;
        let worktree = self.root.join("work").join(format!("{run_id}"));
        if worktree.exists() {
            let _ = git(
                &mirror,
                &["worktree", "remove", "--force", worktree.to_str().unwrap()],
            )
            .await;
            let _ = tokio::fs::remove_dir_all(&worktree).await;
        }
        let start = if git(
            &mirror,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}"),
            ],
        )
        .await
        .is_ok()
        {
            format!("refs/heads/{branch}")
        } else {
            format!("refs/remotes/origin/{base}")
        };
        tokio::fs::create_dir_all(worktree.parent().unwrap())
            .await
            .map_err(|e| Error::Git(e.to_string()))?;
        git(
            &mirror,
            &[
                "worktree",
                "add",
                "-q",
                "-B",
                branch,
                worktree.to_str().unwrap(),
                &start,
            ],
        )
        .await?;
        let gitdir = self.worktree_gitdir(&worktree).await?;
        Ok(Prepared {
            mirror,
            worktree,
            gitdir,
            branch: branch.to_string(),
        })
    }

    /// Read where git put the worktree's admin directory, before the agent can touch the file.
    async fn worktree_gitdir(&self, worktree: &Path) -> Result<PathBuf, Error> {
        let text = tokio::fs::read_to_string(worktree.join(".git"))
            .await
            .map_err(|e| Error::Git(e.to_string()))?;
        let dir = text
            .trim()
            .strip_prefix("gitdir:")
            .map(str::trim)
            .ok_or_else(|| Error::Git("worktree has no gitdir file".into()))?;
        Ok(PathBuf::from(dir))
    }

    /// A read-only checkout of the base branch, for questions. Detached, so it
    /// cannot move any branch.
    pub async fn prepare_readonly(
        &self,
        service: &str,
        url: &str,
        base: &str,
        run_id: i64,
    ) -> Result<Prepared, Error> {
        let mirror = self.ensure_mirror(service).await?;
        let (clean, creds) = split_credentials(url);
        git_net(
            &mirror,
            &[
                "fetch",
                "--prune",
                "-q",
                &clean,
                "+refs/heads/*:refs/remotes/origin/*",
            ],
            creds,
        )
        .await?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let worktree = self.root.join("ask").join(format!("{run_id}-{nonce}"));
        tokio::fs::create_dir_all(worktree.parent().unwrap())
            .await
            .map_err(|e| Error::Git(e.to_string()))?;
        git(
            &mirror,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                &format!("refs/remotes/origin/{base}"),
            ],
        )
        .await?;
        let gitdir = self.worktree_gitdir(&worktree).await?;
        Ok(Prepared {
            mirror,
            worktree,
            gitdir,
            branch: String::new(),
        })
    }

    /// Commit everything the agent changed. Returns false if nothing changed.
    /// Runs with an explicit, trusted git directory; see the module notes.
    /// Commit everything. `author` ("Name <email>") is whom the commit is credited to; the committer is always tasklane.
    pub async fn commit_all(
        &self,
        p: &Prepared,
        message: &str,
        author: Option<&str>,
    ) -> Result<bool, Error> {
        let r = || Run {
            dir: &p.mirror,
            explicit: Some((&p.gitdir, &p.worktree)),
            credentials: None,
        };
        run(r(), &["add", "-A"]).await?;
        if run(r(), &["diff", "--cached", "--quiet"]).await.is_ok() {
            return Ok(false);
        }
        let mut args = vec!["commit", "-q", "--no-verify"];
        if let Some(a) = author {
            args.extend(["--author", a]);
        }
        args.extend(["-m", message]);
        run(r(), &args).await?;
        Ok(true)
    }

    pub async fn remove_worktree(&self, p: &Prepared) {
        let _ = git(
            &p.mirror,
            &[
                "worktree",
                "remove",
                "--force",
                p.worktree.to_str().unwrap(),
            ],
        )
        .await;
        let _ = tokio::fs::remove_dir_all(&p.worktree).await;
    }

    /// Delete leftover worktree directories older than `max_age` (from crashed runs).
    pub async fn sweep(&self, max_age: Duration) {
        for sub in ["work", "ask"] {
            let Ok(mut rd) = tokio::fs::read_dir(self.root.join(sub)).await else {
                continue;
            };
            while let Ok(Some(e)) = rd.next_entry().await {
                let old = e
                    .metadata()
                    .await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .map(|a| a > max_age)
                    .unwrap_or(false);
                if old {
                    let _ = tokio::fs::remove_dir_all(e.path()).await;
                }
            }
        }
        if let Ok(mut rd) = tokio::fs::read_dir(self.root.join("mirrors")).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                let _ = git(&e.path(), &["worktree", "prune"]).await;
            }
        }
    }

    /// Unified diff and changed paths of `branch` against `origin/<base>` in the
    /// mirror. Paths come from `-z --no-renames`: no quoting, and a rename lists
    /// both the old and the new name, so moving a protected file away is visible.
    pub async fn diff(
        &self,
        service: &str,
        base: &str,
        branch: &str,
    ) -> Result<(String, Vec<String>), Error> {
        let mirror = self.mirror_path(service);
        let range = format!("refs/remotes/origin/{base}...refs/heads/{branch}");
        let diff = git(&mirror, &["diff", "--no-renames", "--no-ext-diff", &range]).await?;
        let names = git(
            &mirror,
            &["diff", "--name-only", "-z", "--no-renames", &range],
        )
        .await?;
        Ok((
            diff,
            names
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
        ))
    }

    pub async fn head(&self, service: &str, branch: &str) -> Result<String, Error> {
        let mirror = self.mirror_path(service);
        Ok(
            git(&mirror, &["rev-parse", &format!("refs/heads/{branch}")])
                .await?
                .trim()
                .to_string(),
        )
    }

    /// Fetch one branch of the source into the mirror's local branches.
    pub async fn fetch_branch(&self, service: &str, url: &str, branch: &str) -> Result<(), Error> {
        let mirror = self.mirror_path(service);
        let (clean, creds) = split_credentials(url);
        git_net(
            &mirror,
            &[
                "fetch",
                "-q",
                &clean,
                &format!("+refs/heads/{branch}:refs/heads/{branch}"),
            ],
            creds,
        )
        .await?;
        Ok(())
    }

    pub async fn push(&self, service: &str, url: &str, branch: &str) -> Result<(), Error> {
        let mirror = self.mirror_path(service);
        let (clean, creds) = split_credentials(url);
        git_net(
            &mirror,
            &[
                "push",
                "-q",
                &clean,
                &format!("refs/heads/{branch}:refs/heads/{branch}"),
            ],
            creds,
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_lose_their_credentials() {
        assert_eq!(
            scrub_url("https://x-access-token:abc123@github.com/o/r.git"),
            "https://***@github.com/o/r.git"
        );
        assert_eq!(
            scrub_url("https://github.com/o/r.git"),
            "https://github.com/o/r.git"
        );
        assert_eq!(scrub_url("file:///tmp/x.git"), "file:///tmp/x.git");
    }

    #[test]
    fn credentials_are_split_off_the_url() {
        assert_eq!(
            split_credentials("https://x-access-token:tok@github.com/o/r.git"),
            (
                "https://github.com/o/r.git".to_string(),
                Some("x-access-token:tok".to_string())
            )
        );
        assert_eq!(
            split_credentials("https://github.com/o/r.git"),
            ("https://github.com/o/r.git".to_string(), None)
        );
        // an @ in the path is not a credential
        assert_eq!(
            split_credentials("https://github.com/o/r@v1.git"),
            ("https://github.com/o/r@v1.git".to_string(), None)
        );
    }
}
