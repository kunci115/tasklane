//! Small, dependency-light crypto helpers: HS256 tokens from the apps that
//! embed the report button, and signed values for the console session.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

pub fn hmac_hex(key: &[u8], msg: &str) -> String {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac takes any key length");
    m.update(msg.as_bytes());
    m.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn ct_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

#[derive(Debug, PartialEq, Eq)]
pub enum JwtError {
    Malformed,
    UnsupportedAlg,
    BadSignature,
    Expired,
    NotYetValid,
}

/// Verify an HS256 token and return its claims. `now` is unix seconds.
/// `exp` is required; `nbf` is honoured if present. A little clock skew (60 s) is allowed.
pub fn verify_hs256(token: &str, secret: &[u8], now: i64) -> Result<Value, JwtError> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Malformed);
    };
    let header: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h).map_err(|_| JwtError::Malformed)?)
            .map_err(|_| JwtError::Malformed)?;
    if header.get("alg").and_then(Value::as_str) != Some("HS256") {
        return Err(JwtError::UnsupportedAlg);
    }
    let sig = URL_SAFE_NO_PAD.decode(s).map_err(|_| JwtError::Malformed)?;
    let mut m = HmacSha256::new_from_slice(secret).expect("hmac takes any key length");
    m.update(format!("{h}.{p}").as_bytes());
    if m.verify_slice(&sig).is_err() {
        return Err(JwtError::BadSignature);
    }
    let claims: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).map_err(|_| JwtError::Malformed)?)
            .map_err(|_| JwtError::Malformed)?;
    let exp = claims
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or(JwtError::Malformed)?;
    if now > exp + 60 {
        return Err(JwtError::Expired);
    }
    if let Some(nbf) = claims.get("nbf").and_then(Value::as_i64) {
        if now + 60 < nbf {
            return Err(JwtError::NotYetValid);
        }
    }
    Ok(claims)
}

/// Make an HS256 token (used by tests and by the documentation example).
pub fn sign_hs256(claims: &Value, secret: &[u8]) -> String {
    let h = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let p = URL_SAFE_NO_PAD.encode(claims.to_string());
    let mut m = HmacSha256::new_from_slice(secret).expect("hmac takes any key length");
    m.update(format!("{h}.{p}").as_bytes());
    format!(
        "{h}.{p}.{}",
        URL_SAFE_NO_PAD.encode(m.finalize().into_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_good_token_verifies() {
        let t = sign_hs256(&json!({"sub": "u1", "exp": 2000}), b"secret");
        assert_eq!(verify_hs256(&t, b"secret", 1000).unwrap()["sub"], "u1");
    }

    #[test]
    fn wrong_secret_expired_and_tampered_tokens_are_refused() {
        let t = sign_hs256(&json!({"sub": "u1", "exp": 2000}), b"secret");
        assert_eq!(
            verify_hs256(&t, b"other", 1000),
            Err(JwtError::BadSignature)
        );
        assert_eq!(verify_hs256(&t, b"secret", 2061), Err(JwtError::Expired));
        let mut parts: Vec<&str> = t.split('.').collect();
        let forged = URL_SAFE_NO_PAD.encode(json!({"sub": "admin", "exp": 2000}).to_string());
        parts[1] = &forged;
        assert_eq!(
            verify_hs256(&parts.join("."), b"secret", 1000),
            Err(JwtError::BadSignature)
        );
    }

    #[test]
    fn the_none_algorithm_and_garbage_are_refused() {
        let h = URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#);
        let p = URL_SAFE_NO_PAD.encode(br#"{"exp":9999999999}"#);
        assert_eq!(
            verify_hs256(&format!("{h}.{p}."), b"secret", 1),
            Err(JwtError::UnsupportedAlg)
        );
        assert_eq!(verify_hs256("a.b", b"secret", 1), Err(JwtError::Malformed));
        assert_eq!(
            verify_hs256("a.b.c.d", b"secret", 1),
            Err(JwtError::Malformed)
        );
    }

    #[test]
    fn tokens_without_exp_are_refused_and_nbf_is_honoured() {
        let t = sign_hs256(&json!({"sub": "u1"}), b"s");
        assert_eq!(verify_hs256(&t, b"s", 1), Err(JwtError::Malformed));
        let t = sign_hs256(&json!({"exp": 9000, "nbf": 5000}), b"s");
        assert_eq!(verify_hs256(&t, b"s", 1000), Err(JwtError::NotYetValid));
    }

    #[test]
    fn constant_time_compare_works() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }
}
