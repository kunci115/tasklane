//! Paths a change must never reach without a person looking at it. These rules
//! are a floor that always applies, even when a repository's own list is empty
//! or could not be detected; the repository's list only adds to it.

/// Paths that, when touched, raise a change to at least level 2.
/// A pattern ending in `/` matches a directory anywhere in the tree; any other
/// pattern matches a file name (the last path part) that starts with it, or the
/// whole path exactly.
const BUILT_IN: &[&str] = &[
    ".github/",
    ".gitlab-ci.yml",
    ".circleci/",
    "Jenkinsfile",
    "Dockerfile",
    "docker-compose",
    "compose.",
    "migrations/",
    "migrate/",
    "terraform/",
    ".env",
    ".git/",
    ".gitmodules",
    ".gitattributes",
    "CODEOWNERS",
    // the repository's own tightening rules: a change must not be able to remove them unseen
    ".tasklane.yml",
    ".tasklane/",
];

/// File extensions that hold secrets or keys.
const SECRET_EXTENSIONS: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".kdbx"];

fn normalize(path: &str) -> String {
    let mut p = path.trim().replace('\\', "/");
    while p.starts_with("./") {
        p.drain(..2);
    }
    p.trim_start_matches('/').to_string()
}

fn matches(path: &str, pattern: &str) -> bool {
    let pattern = normalize(pattern);
    if pattern.is_empty() || pattern.contains('*') {
        return false; // globs are not supported; a bare wildcard must not match everything
    }
    if let Some(dir) = pattern.strip_suffix('/') {
        return path == dir
            || path.starts_with(&format!("{dir}/"))
            || path.contains(&format!("/{dir}/"))
            || path.ends_with(&format!("/{dir}"));
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    path == pattern || path.starts_with(&format!("{pattern}/")) || name.starts_with(&pattern)
}

/// The changed paths (renames and deletions listed with both names) that fall
/// under the built-in floor or the repository's own `extra` patterns.
pub fn protected_hits(files: &[String], extra: &[String]) -> Vec<String> {
    let mut hits = Vec::new();
    for f in files {
        let p = normalize(f);
        let lower = p.to_lowercase();
        let secret = SECRET_EXTENSIONS.iter().any(|e| lower.ends_with(e));
        if secret || BUILT_IN.iter().any(|b| matches(&p, b)) || extra.iter().any(|e| matches(&p, e))
        {
            hits.push(f.clone());
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(paths: &[&str]) -> Vec<String> {
        protected_hits(
            &paths.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &[],
        )
    }

    #[test]
    fn the_repositorys_own_rules_file_is_protected() {
        assert_eq!(hit(&[".tasklane.yml"]), vec![".tasklane.yml"]);
        assert_eq!(hit(&["sub/.tasklane.yml", ".tasklane/extra.yml"]).len(), 2);
    }

    #[test]
    fn ci_config_anywhere_is_protected() {
        assert_eq!(hit(&[".github/workflows/ci.yml"]).len(), 1);
        assert_eq!(hit(&["sub/.github/workflows/x.yml"]).len(), 1);
        assert_eq!(
            hit(&[".gitlab-ci.yml", "Jenkinsfile", ".circleci/config.yml"]).len(),
            3
        );
    }

    #[test]
    fn nested_migrations_dockerfiles_and_infrastructure_are_protected() {
        assert_eq!(
            hit(&[
                "apps/x/migrations/0001.sql",
                "svc/Dockerfile",
                "svc/Dockerfile.prod",
                "infra/terraform/main.tf",
                "docker-compose.yml",
                "compose.yaml"
            ])
            .len(),
            6
        );
    }

    #[test]
    fn secrets_and_git_internals_are_protected() {
        assert_eq!(
            hit(&[
                "deploy/server.pem",
                "id.key",
                ".env",
                "config/.env.production",
                ".gitmodules",
                ".gitattributes"
            ])
            .len(),
            6
        );
    }

    #[test]
    fn ordinary_files_are_not() {
        assert!(hit(&[
            "src/main.rs",
            "README.md",
            "docs/migrations-explained.md",
            "src/environment.rs",
            "tests/test_dockerfile_lint.py"
        ])
        .is_empty());
    }

    #[test]
    fn the_repository_list_adds_to_the_floor_and_a_bare_wildcard_never_matches_everything() {
        let files = vec!["billing/rates.py".to_string(), "src/a.rs".to_string()];
        assert_eq!(
            protected_hits(&files, &["billing/".into()]),
            vec!["billing/rates.py".to_string()]
        );
        assert!(protected_hits(&files, &["*".into(), "*.js".into(), "".into()]).is_empty());
    }

    #[test]
    fn leading_dot_slash_and_backslashes_do_not_hide_a_path() {
        assert_eq!(
            hit(&[
                "./.github/workflows/ci.yml",
                ".github\\workflows\\ci.yml",
                "/.github/x"
            ])
            .len(),
            3
        );
    }

    #[test]
    fn a_renamed_away_file_counts_when_both_names_are_listed() {
        // `git diff --name-only -z --no-renames` lists the old and the new name
        assert_eq!(
            hit(&[".github/workflows/ci.yml", "docs/ci.txt"]),
            vec![".github/workflows/ci.yml".to_string()]
        );
    }
}
