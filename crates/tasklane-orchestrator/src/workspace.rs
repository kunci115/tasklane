//! Git work for the development step: a bare mirror per service, and one
//! worktree per run. All git calls use a clean environment, never prompt, and
//! never store a credential: URLs that carry a token are passed on the command
//! line only and are scrubbed from error text.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::Error;

#[derive(Clone)]
pub struct Workspaces {
    root: PathBuf,
}

pub struct Prepared {
    pub mirror: PathBuf,
    pub worktree: PathBuf,
    pub branch: String,
}

pub async fn git(dir: &Path, args: &[&str], secret: Option<&str>) -> Result<String, Error> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "tasklane")
        .env("GIT_AUTHOR_EMAIL", "tasklane@users.noreply.invalid")
        .env("GIT_COMMITTER_NAME", "tasklane")
        .env("GIT_COMMITTER_EMAIL", "tasklane@users.noreply.invalid")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| Error::Git(e.to_string()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let mut msg = format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr)
        );
        if let Some(s) = secret.filter(|s| !s.is_empty()) {
            msg = msg.replace(s, "***");
        }
        Err(Error::Git(msg.trim().to_string()))
    }
}

/// Remove the credential from a URL before it can reach a log line.
pub fn scrub_url(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(i), Some(j)) if j > i => format!("{}***@{}", &url[..i + 3], &url[j + 1..]),
        _ => url.to_string(),
    }
}

impl Workspaces {
    pub fn new(root: PathBuf) -> Self {
        Workspaces { root }
    }

    fn mirror_path(&self, service: &str) -> PathBuf {
        let h = format!("{:x}", Sha256::digest(service.as_bytes()));
        self.root.join("mirrors").join(format!("{}.git", &h[..16]))
    }

    pub fn mirror_for(&self, service: &str) -> PathBuf {
        self.mirror_path(service)
    }

    /// Update the mirror from `url` and make a worktree on `branch`, starting
    /// from `start` (the branch itself if it exists in the mirror, else `base`).
    pub async fn prepare(
        &self,
        service: &str,
        url: &str,
        base: &str,
        branch: &str,
        run: i64,
    ) -> Result<Prepared, Error> {
        let mirror = self.mirror_path(service);
        tokio::fs::create_dir_all(&mirror)
            .await
            .map_err(|e| Error::Git(e.to_string()))?;
        if !mirror.join("HEAD").exists() {
            git(&mirror, &["init", "--bare", "-q", "."], None).await?;
        }
        // Branches we made ourselves (refs/heads/tasklane/*) stay; everything else mirrors the source.
        let secret = url
            .rsplit_once('@')
            .map(|(left, _)| left.rsplit(':').next().unwrap_or("").to_string());
        git(
            &mirror,
            &[
                "fetch",
                "--prune",
                "-q",
                url,
                "+refs/heads/*:refs/remotes/origin/*",
            ],
            secret.as_deref(),
        )
        .await?;
        let worktree = self.root.join("work").join(format!("{run}"));
        if worktree.exists() {
            let _ = git(
                &mirror,
                &["worktree", "remove", "--force", worktree.to_str().unwrap()],
                None,
            )
            .await;
            let _ = tokio::fs::remove_dir_all(&worktree).await;
        }
        git(&mirror, &["worktree", "prune"], None).await?;
        let start = if git(
            &mirror,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("refs/heads/{branch}"),
            ],
            None,
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
            None,
        )
        .await?;
        Ok(Prepared {
            mirror,
            worktree,
            branch: branch.to_string(),
        })
    }

    /// A read-only checkout of the base branch, for questions. Detached, so it
    /// cannot move any branch.
    pub async fn prepare_readonly(
        &self,
        service: &str,
        url: &str,
        base: &str,
        run: i64,
    ) -> Result<Prepared, Error> {
        let mirror = self.mirror_path(service);
        tokio::fs::create_dir_all(&mirror)
            .await
            .map_err(|e| Error::Git(e.to_string()))?;
        if !mirror.join("HEAD").exists() {
            git(&mirror, &["init", "--bare", "-q", "."], None).await?;
        }
        let secret = url
            .rsplit_once('@')
            .map(|(left, _)| left.rsplit(':').next().unwrap_or("").to_string());
        git(
            &mirror,
            &[
                "fetch",
                "--prune",
                "-q",
                url,
                "+refs/heads/*:refs/remotes/origin/*",
            ],
            secret.as_deref(),
        )
        .await?;
        let worktree = self
            .root
            .join("ask")
            .join(format!("{run}-{}", std::process::id()));
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
            None,
        )
        .await?;
        Ok(Prepared {
            mirror,
            worktree,
            branch: String::new(),
        })
    }

    /// Commit everything the agent changed. Returns false if nothing changed.
    pub async fn commit_all(&self, worktree: &Path, message: &str) -> Result<bool, Error> {
        git(worktree, &["add", "-A"], None).await?;
        if git(worktree, &["diff", "--cached", "--quiet"], None)
            .await
            .is_ok()
        {
            return Ok(false);
        }
        git(worktree, &["commit", "-q", "-m", message], None).await?;
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
            None,
        )
        .await;
        let _ = tokio::fs::remove_dir_all(&p.worktree).await;
    }

    /// Unified diff and changed paths of `branch` against `origin/<base>` in the mirror.
    pub async fn diff(
        &self,
        service: &str,
        base: &str,
        branch: &str,
    ) -> Result<(String, Vec<String>), Error> {
        let mirror = self.mirror_path(service);
        let range = format!("refs/remotes/origin/{base}...refs/heads/{branch}");
        let diff = git(&mirror, &["diff", &range], None).await?;
        let names = git(&mirror, &["diff", "--name-only", &range], None).await?;
        Ok((diff, names.lines().map(str::to_string).collect()))
    }

    pub async fn head(&self, service: &str, branch: &str) -> Result<String, Error> {
        let mirror = self.mirror_path(service);
        Ok(git(
            &mirror,
            &["rev-parse", &format!("refs/heads/{branch}")],
            None,
        )
        .await?
        .trim()
        .to_string())
    }

    /// Fetch one branch of the source into the mirror's local branches.
    pub async fn fetch_branch(&self, service: &str, url: &str, branch: &str) -> Result<(), Error> {
        let mirror = self.mirror_path(service);
        let secret = url
            .rsplit_once('@')
            .map(|(left, _)| left.rsplit(':').next().unwrap_or("").to_string());
        git(
            &mirror,
            &[
                "fetch",
                "-q",
                url,
                &format!("+refs/heads/{branch}:refs/heads/{branch}"),
            ],
            secret.as_deref(),
        )
        .await?;
        Ok(())
    }

    pub async fn push(&self, service: &str, url: &str, branch: &str) -> Result<(), Error> {
        let mirror = self.mirror_path(service);
        let secret = url
            .rsplit_once('@')
            .map(|(left, _)| left.rsplit(':').next().unwrap_or("").to_string());
        git(
            &mirror,
            &[
                "push",
                "-q",
                url,
                &format!("refs/heads/{branch}:refs/heads/{branch}"),
            ],
            secret.as_deref(),
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
}
