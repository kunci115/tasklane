# Contributing

- Rust core: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. The integration tests need a Postgres superuser URL in `TASKLANE_TEST_DATABASE_URL` (see README); `TASKLANE_REQUIRE_DB=1` turns a missing database into a failure.
- Plug-ins are separate programs in any language that speak the JSON-lines protocol in `docs/design/05-plugin-protocol.md` with the operations in `06-plugin-operations.md`. The Python ones use only the standard library: `python3 -m unittest discover -s plugins/<name>`.
- Keep the rules in `docs/design/04-threat-model.md` (section 4) true in every change. A change that weakens one needs a design discussion first.
- Tests that depend on a real service must say so and must be skipped by default.
