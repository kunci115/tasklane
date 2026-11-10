//! Who asked for a change and who approved it, written where people look for it: trailers on the
//! commit and a line on the pull request.
//!
//! The agent works on behalf of a person, so the person it answers to gets the credit in the history:
//! whoever approved the change (the last approver first), or whoever asked for it when no approval was
//! needed, becomes the commit's **author**, with tasklane as committer, and so the commit counts in their
//! GitHub contribution graph. Other people who chose to be credited are `Co-authored-by`. Nobody is credited
//! unless they turned that on for themselves. `Requested-by` and `Approved-by` trailers always say who asked
//! and who approved, so the history stays honest about how the change came about. Only a login that matches
//! GitHub's own rules ever reaches a commit message, so no text from a ticket can add trailers.

use serde_json::{json, Value};

use crate::{Error, Orchestrator};

/// A GitHub login: letters, digits and single hyphens, 1 to 39 characters, no leading or trailing hyphen.
pub fn valid_login(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 39
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
        && !s.contains("--")
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Credit {
    pub requested_by: Option<String>,
    pub approved_by: Vec<String>,
    /// (login, GitHub user id) of the person the work is credited to: the commit's author.
    pub author: Option<(String, i64)>,
    /// (login, GitHub user id) of other people who chose to be credited.
    pub coauthors: Vec<(String, i64)>,
}

/// The value of `git commit --author` for a person: GitHub links the noreply address to their account.
pub fn author_arg(login: &str, id: i64) -> String {
    format!("{login} <{id}+{login}@users.noreply.github.com>")
}

impl Credit {
    /// Trailer lines for a commit message (each on its own line, no leading blank line).
    pub fn trailers(&self) -> String {
        let mut out = vec![];
        if let Some(r) = &self.requested_by {
            out.push(format!("Requested-by: {r}"));
        }
        for a in &self.approved_by {
            out.push(format!("Approved-by: {a}"));
        }
        for (login, id) in &self.coauthors {
            out.push(format!(
                "Co-authored-by: {login} <{id}+{login}@users.noreply.github.com>"
            ));
        }
        out.join("\n")
    }

    /// One plain sentence for a pull request. Logins are not written as `@mentions` (no notification).
    pub fn pr_line(&self) -> String {
        let mut parts = vec![];
        if let Some(r) = &self.requested_by {
            parts.push(format!("Requested by {r}."));
        }
        if !self.approved_by.is_empty() {
            parts.push(format!("Approved by {}.", self.approved_by.join(", ")));
        }
        if let Some((a, _)) = &self.author {
            parts.push(format!("Credited to {a}, who this change was made for."));
        }
        parts.join(" ")
    }

    /// `--author` for the commit, if somebody is to be credited as its author.
    pub fn author_arg(&self) -> Option<String> {
        self.author.as_ref().map(|(l, i)| author_arg(l, *i))
    }

    pub fn is_empty(&self) -> bool {
        self.requested_by.is_none() && self.approved_by.is_empty()
    }
}

/// The commit message: one line (a title can carry anything, so line breaks are removed) and the trailers.
pub fn commit_message(title: &str, reference: &str, credit: &Credit) -> String {
    let title: String = title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let head = format!("tasklane: {} ({reference})", title.trim());
    let t = credit.trailers();
    if t.is_empty() {
        head
    } else {
        format!("{head}\n\n{t}")
    }
}

struct Person {
    login: String,
    id: Option<i64>,
    credit: bool,
    principal: i64,
}

/// Find a person by what the system knows: a GitHub-channel principal whose reference is the login, or a
/// principal of any channel (a Telegram approver, say) that has the login filled in.
async fn person_by_login(o: &Orchestrator, login: &str) -> Result<Option<Person>, Error> {
    let r: Option<(i64, Option<i64>, bool)> = sqlx::query_as(
        "SELECT id, github_id, credit_coauthor FROM principals
         WHERE lower(github_login) = lower($1) OR (channel = 'github' AND lower(ref) = lower($1))
         ORDER BY credit_coauthor DESC LIMIT 1",
    )
    .bind(login)
    .fetch_optional(o.pool())
    .await?;
    Ok(r.map(|(principal, id, credit)| Person {
        login: login.to_string(),
        id,
        credit,
        principal,
    }))
}

/// Who approved this ticket (decision `approve`), as GitHub logins, in the order they approved (the last
/// one is the one who answers for the change). People without a known login are left out.
async fn approvers(o: &Orchestrator, ticket_id: i64) -> Result<Vec<String>, Error> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT a.approver, p.github_login
         FROM approvals a
         LEFT JOIN principals p ON a.approver = p.channel || ':' || p.ref
         WHERE a.ticket_id = $1 AND a.decision = 'approve'
         ORDER BY a.id",
    )
    .bind(ticket_id)
    .fetch_all(o.pool())
    .await?;
    let mut out: Vec<String> = vec![];
    for (who, login) in rows {
        let login = login.or_else(|| who.strip_prefix("github:").map(str::to_string));
        if let Some(l) = login.filter(|l| valid_login(l)) {
            out.retain(|x| !x.eq_ignore_ascii_case(&l));
            out.push(l);
        }
    }
    Ok(out)
}

impl Orchestrator {
    /// Who to name for a ticket. `author` is the login that opened it. Never fails a job: if something
    /// cannot be looked up, that person is simply left out.
    pub(crate) async fn credit_for(&self, ticket_id: i64, author: &str) -> Credit {
        if !self.cfg.attribution {
            return Credit::default();
        }
        match self.build_credit(ticket_id, author).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("ticket {ticket_id}: could not work out who to credit: {e}");
                Credit::default()
            }
        }
    }

    async fn build_credit(&self, ticket_id: i64, author: &str) -> Result<Credit, Error> {
        let mut credit = Credit {
            requested_by: valid_login(author).then(|| author.to_string()),
            approved_by: approvers(self, ticket_id).await?,
            ..Default::default()
        };
        // who answers for the change first: the last approver, then earlier ones, then whoever asked
        let mut order: Vec<String> = credit.approved_by.iter().rev().cloned().collect();
        if let Some(r) = &credit.requested_by {
            if !order.iter().any(|w| w.eq_ignore_ascii_case(r)) {
                order.push(r.clone());
            }
        }
        for login in order {
            let Some(p) = person_by_login(self, &login).await? else {
                continue;
            };
            if !p.credit {
                continue;
            }
            let id = match p.id {
                Some(id) => Some(id),
                None => self.resolve_github_id(p.principal, &p.login).await,
            };
            let Some(id) = id else { continue };
            if credit.author.is_none() {
                credit.author = Some((p.login, id));
            } else {
                credit.coauthors.push((p.login, id));
            }
        }
        Ok(credit)
    }

    /// Ask the source for the numeric id behind a login and remember it. None if it cannot say.
    async fn resolve_github_id(&self, principal: i64, login: &str) -> Option<i64> {
        let v: Value = self
            .source
            .call("get_user", json!({"login": login}), self.cfg.call_timeout)
            .await
            .ok()?;
        let id = v.get("id").and_then(Value::as_i64).filter(|i| *i > 0)?;
        let _ = sqlx::query("UPDATE principals SET github_id = $2 WHERE id = $1")
            .bind(principal)
            .bind(id)
            .execute(self.pool())
            .await;
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logins_follow_githubs_rules_and_nothing_else_gets_through() {
        for ok in ["rino", "Nabil-1", "a", "a1-b2"] {
            assert!(valid_login(ok), "{ok}");
        }
        for bad in [
            "",
            "-x",
            "x-",
            "a--b",
            "a b",
            "a\nCo-authored-by: x",
            "x@y",
            "é",
            &"a".repeat(40),
            "a[bot]",
        ] {
            assert!(!valid_login(bad), "{bad:?}");
        }
    }

    #[test]
    fn trailers_and_the_pull_request_line() {
        let c = Credit {
            requested_by: Some("nabil".into()),
            approved_by: vec!["rino".into()],
            author: Some(("rino".into(), 7)),
            coauthors: vec![("nabil".into(), 42)],
        };
        assert_eq!(
            c.trailers(),
            "Requested-by: nabil\nApproved-by: rino\nCo-authored-by: nabil <42+nabil@users.noreply.github.com>"
        );
        assert_eq!(
            c.author_arg().as_deref(),
            Some("rino <7+rino@users.noreply.github.com>")
        );
        assert_eq!(
            c.pr_line(),
            "Requested by nabil. Approved by rino. Credited to rino, who this change was made for."
        );
        assert_eq!(Credit::default().trailers(), "");
        assert_eq!(Credit::default().pr_line(), "");
        assert!(Credit::default().is_empty());
    }

    #[test]
    fn a_title_cannot_add_trailers_or_lines() {
        let c = Credit {
            requested_by: Some("nabil".into()),
            ..Default::default()
        };
        let m = commit_message(
            "fix\n\nCo-authored-by: evil <1+evil@users.noreply.github.com>",
            "7",
            &c,
        );
        let (head, trailers) = m.split_once("\n\n").unwrap();
        assert!(!head.contains('\n'), "{head:?}");
        assert_eq!(trailers, "Requested-by: nabil");
        assert_eq!(
            commit_message("plain", "7", &Credit::default()),
            "tasklane: plain (7)"
        );
    }
}
