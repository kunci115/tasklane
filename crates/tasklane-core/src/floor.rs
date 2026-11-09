//! Deterministic risk floor from ticket text. A model may raise the level
//! (`Level::with_model_suggestion`) but never go below this floor.
//!
//! These are starting defaults, not final policy: the keyword table is meant to
//! be edited per install. Matching is a case-insensitive substring search, so it
//! errs on the side of raising the level.

use crate::Level;

const RULES: &[(&str, Level, &[&str])] = &[
    (
        "database migration",
        Level::L2,
        &["migration", "alter table", "schema change"],
    ),
    (
        "login or permissions",
        Level::L2,
        &["login", "password", "permission", "authentication", "roles"],
    ),
    ("secrets", Level::L3, &["secret", "api key", "credential"]),
    (
        "deploy or infrastructure",
        Level::L2,
        &["deploy", "infrastructure", "dockerfile", "terraform"],
    ),
    ("payments", Level::L3, &["payment", "invoice", "refund"]),
    ("message templates", Level::L2, &["template", "bot wording"]),
    (
        "deleting or altering data",
        Level::L3,
        &["delete data", "drop table", "truncate", "purge"],
    ),
    (
        "personal data",
        Level::L3,
        &["personal data", "gdpr", "passport"],
    ),
    (
        "security or outage",
        Level::L2,
        &["leak", "vulnerab", "outage", "security"],
    ),
];

/// Floor level and the names of the rules that matched.
pub fn floor_from_text(text: &str) -> (Level, Vec<&'static str>) {
    let text = text.to_lowercase();
    let mut level = Level::L1;
    let mut why = Vec::new();
    for (name, lvl, words) in RULES {
        if words.iter().any(|w| text.contains(w)) {
            level = level.max(*lvl);
            why.push(*name);
        }
    }
    (level, why)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_level_one() {
        assert_eq!(
            floor_from_text("Button colour is wrong on the report page"),
            (Level::L1, vec![])
        );
    }

    #[test]
    fn migration_is_level_two() {
        let (l, why) = floor_from_text("Needs a Database MIGRATION for the new column");
        assert_eq!(l, Level::L2);
        assert_eq!(why, vec!["database migration"]);
    }

    #[test]
    fn highest_matching_rule_wins_and_all_reasons_are_listed() {
        let (l, why) = floor_from_text("login broken after payment refund");
        assert_eq!(l, Level::L3);
        assert_eq!(why, vec!["login or permissions", "payments"]);
    }
}
