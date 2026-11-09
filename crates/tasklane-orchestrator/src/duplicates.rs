//! Duplicate proposals. A new ticket whose title is very close to an earlier ticket of the same
//! repository is proposed as a duplicate; only a person can close it as one (the model never
//! decides this, and a wrong guess costs one click). Similarity is plain word overlap, so it is
//! predictable and needs no extension in the database.

use std::collections::HashSet;

use sqlx::PgPool;

/// Words that carry no meaning for matching.
const STOP: &[&str] = &[
    "the", "and", "for", "with", "from", "that", "this", "when", "have", "has", "are", "was",
    "not", "can", "cant", "does", "doesnt", "dont", "into", "onto", "after", "before", "about",
    "please", "add", "fix", "bug", "error", "issue", "problem", "use", "using", "make", "need",
    "want",
];

/// Share of words two titles have in common (Jaccard), and how many significant words they share.
pub fn similarity(a: &str, b: &str) -> (f64, usize) {
    let (x, y) = (words(a), words(b));
    if x.is_empty() || y.is_empty() {
        return (0.0, 0);
    }
    let both = x.intersection(&y).count();
    let all = x.union(&y).count();
    (both as f64 / all as f64, both)
}

fn words(s: &str) -> HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(w))
        .map(str::to_string)
        .collect()
}

/// Titles this close are proposed as duplicates.
const THRESHOLD: f64 = 0.7;
const MIN_SHARED: usize = 2;

pub struct Candidate {
    pub reference: String,
    pub score: f64,
}

/// The earlier ticket of the same repository that `title` most resembles, if any resembles it enough.
pub async fn find(
    pool: &PgPool,
    ticket_id: i64,
    title: &str,
) -> Result<Option<Candidate>, sqlx::Error> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT o.external_ref, o.title
         FROM tickets t JOIN tickets o ON o.service_id = t.service_id AND o.id <> t.id
         WHERE t.id = $1 AND o.title IS NOT NULL AND o.id < t.id
           AND o.state NOT IN ('closed_duplicate', 'closed_rejected', 'closed_stale')
         ORDER BY o.id DESC LIMIT 300",
    )
    .bind(ticket_id)
    .fetch_all(pool)
    .await?;
    let mut best: Option<Candidate> = None;
    for (reference, other) in rows {
        let (score, shared) = similarity(title, other.as_deref().unwrap_or(""));
        if score >= THRESHOLD
            && shared >= MIN_SHARED
            && best.as_ref().is_none_or(|b| score > b.score)
        {
            best = Some(Candidate { reference, score });
        }
    }
    Ok(best)
}

/// The most recent tickets of the same repository, as (ref, title), for the model to compare against.
pub async fn recent(pool: &PgPool, ticket_id: i64) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT o.external_ref, o.title
         FROM tickets t JOIN tickets o ON o.service_id = t.service_id AND o.id <> t.id
         WHERE t.id = $1 AND o.title IS NOT NULL AND o.id < t.id
           AND o.state NOT IN ('closed_duplicate', 'closed_rejected', 'closed_stale')
         ORDER BY o.id DESC LIMIT 30",
    )
    .bind(ticket_id)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_words_in_another_order_match() {
        let (s, n) = similarity(
            "Checkout button hidden on small phones",
            "small phones: checkout button is hidden",
        );
        assert!(s >= THRESHOLD && n >= MIN_SHARED, "{s} {n}");
    }

    #[test]
    fn different_tickets_do_not_match() {
        assert!(similarity("Add gift-wrap option", "Fix refund webhook 500").0 < 0.2);
        // a shared topic is not enough
        let (s, _) = similarity(
            "Checkout button hidden on small phones",
            "Checkout total is wrong for gift cards",
        );
        assert!(s < THRESHOLD, "{s}");
    }

    #[test]
    fn short_or_empty_titles_never_match() {
        assert_eq!(similarity("", "anything here"), (0.0, 0));
        assert!(similarity("fix bug", "fix bug").1 < MIN_SHARED);
    }
}
