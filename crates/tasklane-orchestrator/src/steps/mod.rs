pub mod ci;
pub mod develop;
pub mod merge;
pub mod pr;
pub mod review;
pub mod triage;

use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn sha256_hex(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

pub(crate) fn text_of(item: &Value, key: &str) -> String {
    item.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
