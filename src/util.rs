use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

pub fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn trunc(value: &str, limit: usize) -> String {
    let end = if value.len() <= limit {
        value.len()
    } else {
        value
            .char_indices()
            .nth(limit)
            .map_or(value.len(), |(index, _)| index)
    };
    value[..end].to_owned()
}

pub fn required_string_arg<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    optional_string_arg(args, key).ok_or_else(|| format!("{key} is required"))
}

pub fn optional_string_arg<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub fn constant_time_equal(left: &str, right: &str) -> bool {
    let left = Sha256::digest(left.as_bytes());
    let right = Sha256::digest(right.as_bytes());
    bool::from(left.ct_eq(&right))
}

pub fn random_token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("operating system randomness is unavailable");
    format!("{prefix}_{}", URL_SAFE_NO_PAD.encode(bytes))
}

pub fn token_fingerprint(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{constant_time_equal, trunc};

    #[test]
    fn compares_tokens_without_accepting_prefixes() {
        assert!(constant_time_equal("token", "token"));
        assert!(!constant_time_equal("token", "token-extra"));
        assert!(!constant_time_equal("token", "Token"));
    }

    #[test]
    fn limits_text_by_characters() {
        assert_eq!(trunc("hello", 10), "hello");
        assert_eq!(trunc("héllo", 3), "hél");
        assert_eq!(trunc("🦀你好", 2), "🦀你");
        assert_eq!(trunc("🦀你好", 3), "🦀你好");
        assert_eq!(trunc("🦀你好", 0), "");
        assert_eq!(trunc("", 0), "");
        assert_eq!(trunc("hello", 5), "hello");
        assert_eq!(trunc("héllo", usize::MAX), "héllo");
    }
}
