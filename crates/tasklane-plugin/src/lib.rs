//! Wire protocol: one JSON object per line on the plug-in's stdin (requests)
//! and stdout (responses). stderr is for logs. See
//! docs/design/05-plugin-protocol.md.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_MAJOR: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub op: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Transient,
    Permanent,
    Unauthorized,
    RateLimited,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginError {
    pub kind: ErrorKind,
    pub message: String,
    /// For `rate_limited`: how long until the plug-in expects to be usable again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PluginError>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Response {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }
    pub fn err(id: u64, kind: ErrorKind, message: impl Into<String>) -> Self {
        Response {
            id,
            ok: false,
            result: None,
            error: Some(PluginError {
                kind,
                message: message.into(),
                retry_after_secs: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_round_trip() {
        let line = r#"{"id":1,"op":"describe"}"#;
        let r: Request = serde_json::from_str(line).unwrap();
        assert_eq!(r.id, 1);
        assert_eq!(r.op, "describe");
        assert_eq!(r.params, Value::Null);
    }

    #[test]
    fn error_response_shape() {
        let r = Response::err(7, ErrorKind::RateLimited, "slow down");
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&s).unwrap(),
            json!({"id":7,"ok":false,"error":{"kind":"rate_limited","message":"slow down"}})
        );
    }

    #[test]
    fn ok_response_has_no_error_field() {
        let s = serde_json::to_string(&Response::ok(2, json!({"x":1}))).unwrap();
        assert!(!s.contains("error"));
    }
}
