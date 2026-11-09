// `sqlx::migrate!` embeds the migration files at compile time; without this, adding a migration
// file alone would not rebuild the crate and the new schema would be missing from the binary.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
