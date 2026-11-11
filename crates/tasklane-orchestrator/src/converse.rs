//! Conversations with a repository's AI: a person asks, learns, asks again; the agent reads the code
//! (read-only) and sees what was said before. A conversation can be turned into the text of an issue,
//! which a person edits and creates. Nothing here changes the repository.

use crate::{Error, Orchestrator};

const MAX_QUESTION: usize = 2000;
const DAILY_QUESTIONS: i64 = 40;
const HISTORY_MESSAGES: i64 = 12;
const HISTORY_CHARS: usize = 12_000;

fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn transcript(history: &[(String, String)], per_message: usize) -> String {
    let mut out = String::new();
    for (role, body) in history {
        let who = if role == "person" { "Person" } else { "You" };
        out.push_str(&format!("[{who}] {}\n\n", clip(body, per_message)));
    }
    // keep the end of a long conversation: the latest messages matter most
    let n = out.chars().count();
    if n > HISTORY_CHARS {
        out = out.chars().skip(n - HISTORY_CHARS).collect();
    }
    out
}

impl Orchestrator {
    pub async fn conv_start(&self, service_id: i64, owner: &str) -> Result<i64, Error> {
        sqlx::query_scalar(
            "INSERT INTO conversations (service_id, owner)
             SELECT id, $2 FROM services WHERE id = $1 AND NOT archived RETURNING id",
        )
        .bind(service_id)
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::Fatal("unknown repository".into()))
    }

    async fn conv_service(&self, conv: i64, owner: &str) -> Result<String, Error> {
        sqlx::query_scalar(
            "SELECT s.external_id FROM conversations c JOIN services s ON s.id = c.service_id
             WHERE c.id = $1 AND c.owner = $2 AND NOT s.archived",
        )
        .bind(conv)
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| Error::Fatal("unknown conversation".into()))
    }

    async fn conv_history(&self, conv: i64, last: i64) -> Result<Vec<(String, String)>, Error> {
        let mut rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT role, body FROM conversation_messages WHERE conversation_id = $1 ORDER BY id DESC LIMIT $2",
        )
        .bind(conv)
        .bind(last)
        .fetch_all(&self.pool)
        .await?;
        rows.reverse();
        Ok(rows)
    }

    /// Ask the next question of a conversation and keep both sides.
    pub async fn conv_say(&self, conv: i64, owner: &str, text: &str) -> Result<String, Error> {
        let text = text.trim();
        let n = text.chars().count();
        if n == 0 || n > MAX_QUESTION {
            return Err(Error::Fatal(format!(
                "a question needs 1 to {MAX_QUESTION} characters"
            )));
        }
        let service = self.conv_service(conv, owner).await?;
        let used: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM conversation_messages m JOIN conversations c ON c.id = m.conversation_id
             WHERE c.owner = $1 AND m.role = 'person' AND m.created_at > date_trunc('day', now())",
        )
        .bind(owner)
        .fetch_one(&self.pool)
        .await?;
        if used >= DAILY_QUESTIONS {
            return Err(Error::Fatal(
                "you have used your questions for today".into(),
            ));
        }
        let history = self.conv_history(conv, HISTORY_MESSAGES).await?;
        let prompt = format!(
            "This is a continuing conversation with a person who wants to understand this repository, or to work out what to ask the team for. Answer from the code and the notes; say so when you do not know. Earlier in the conversation:\n\n{}Their new question:\n{text}",
            transcript(&history, 3000)
        );
        sqlx::query("INSERT INTO conversation_messages (conversation_id, role, body) VALUES ($1, 'person', $2)")
            .bind(conv)
            .bind(text)
            .execute(&self.pool)
            .await?;
        let answer = self.ask_run(&service, &prompt).await?;
        sqlx::query(
            "INSERT INTO conversation_messages (conversation_id, role, body) VALUES ($1, 'ai', $2)",
        )
        .bind(conv)
        .bind(&answer)
        .execute(&self.pool)
        .await?;
        sqlx::query("UPDATE conversations SET updated_at = now(), title = CASE WHEN title = '' THEN $2 ELSE title END WHERE id = $1")
            .bind(conv)
            .bind(clip(text, 80))
            .execute(&self.pool)
            .await?;
        Ok(answer)
    }

    /// The conversation as the text of an issue: (title, body). Nothing is created.
    pub async fn conv_draft(&self, conv: i64, owner: &str) -> Result<(String, String), Error> {
        let service = self.conv_service(conv, owner).await?;
        let history = self.conv_history(conv, 40).await?;
        if history.is_empty() {
            return Err(Error::Fatal(
                "there is nothing to turn into an issue yet".into(),
            ));
        }
        let prompt = format!(
            "Write an issue for the development team from this conversation.\n\nFirst line: the title only, at most 100 characters, plain text.\nThen a blank line.\nThen the body in Markdown with these parts: what is wanted and why; what the conversation established that a developer needs (behaviour, files or areas of the code, constraints, decisions); acceptance criteria as a checklist; open questions, if any. Use only what was said or what you saw in the code; do not invent requirements.\n\nThe conversation:\n\n{}",
            transcript(&history, 3000)
        );
        let answer = self.ask_run(&service, &prompt).await?;
        let mut lines = answer.trim().lines();
        let title = lines
            .by_ref()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .trim_start_matches('#')
            .trim()
            .trim_start_matches("Title:")
            .trim()
            .to_string();
        let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
        if title.is_empty() || body.is_empty() {
            return Err(Error::BadOutput("the draft had no title or no body".into()));
        }
        Ok((clip(&title, 200), clip(&body, 15_000)))
    }
}
