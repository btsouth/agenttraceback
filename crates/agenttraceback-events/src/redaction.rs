use std::sync::LazyLock;

use agenttraceback_crypto::{KeyError, MasterKey};
use regex::{Captures, Regex};
use serde_json::Value;
use thiserror::Error;

static SECRET_PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    vec![
        (
            "openai_key",
            Regex::new(r"\bsk-[A-Za-z0-9_-]{16,}\b").expect("valid openai key pattern"),
        ),
        (
            "github_token",
            Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{20,}\b").expect("valid github pattern"),
        ),
        (
            "aws_access_key",
            Regex::new(r"\bAKIA[0-9A-Z]{16}\b").expect("valid aws pattern"),
        ),
        (
            "bearer_token",
            Regex::new(r"(?i)(\bbearer\s+)([A-Za-z0-9._~+/-]{12,}=*)")
                .expect("valid bearer pattern"),
        ),
        (
            "credential_url",
            Regex::new(r"(?i)\b(?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis)://[^\s:@/]+:[^\s@/]+@")
                .expect("valid credential URL pattern"),
        ),
        (
            "password_assignment",
            Regex::new(
                r#"(?i)\b(password|passwd|pwd|secret|api[_-]?key|access[_-]?token)\s*[:=]\s*["']?([^\s"',;]{4,})"#,
            )
            .expect("valid assignment pattern"),
        ),
        (
            "private_key",
            Regex::new(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----")
                .expect("valid private key pattern"),
        ),
    ]
});

/// Redaction failures.
#[derive(Debug, Error)]
pub enum RedactionError {
    /// The keyed digest could not be derived.
    #[error(transparent)]
    Key(#[from] KeyError),
    /// JSON traversal failed.
    #[error("redacted JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// Applies keyed, repeatable redaction before persistence or export.
#[derive(Clone, Debug)]
pub struct Redactor {
    master_key: MasterKey,
}

impl Redactor {
    /// Creates a redactor backed by the installation key.
    #[must_use]
    pub const fn new(master_key: MasterKey) -> Self {
        Self { master_key }
    }

    /// Redacts every recognized secret in a text value.
    pub fn redact_text(&self, input: &str) -> Result<String, RedactionError> {
        let mut output = input.to_owned();
        for (kind, pattern) in SECRET_PATTERNS.iter() {
            output = replace_matches(&output, pattern, kind, &self.master_key)?;
        }
        Ok(output)
    }

    /// Redacts string values recursively in a JSON tree.
    pub fn redact_json(&self, value: &Value) -> Result<Value, RedactionError> {
        match value {
            Value::String(text) => Ok(Value::String(self.redact_text(text)?)),
            Value::Array(items) => items
                .iter()
                .map(|item| self.redact_json(item))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            Value::Object(entries) => entries
                .iter()
                .map(|(key, value)| Ok((key.clone(), self.redact_json(value)?)))
                .collect::<Result<serde_json::Map<_, _>, RedactionError>>()
                .map(Value::Object),
            _ => Ok(value.clone()),
        }
    }
}

fn replace_matches(
    input: &str,
    pattern: &Regex,
    kind: &str,
    master_key: &MasterKey,
) -> Result<String, RedactionError> {
    let mut output = String::with_capacity(input.len());
    let mut last_end = 0;
    for captures in pattern.captures_iter(input) {
        let Some(matched) = captures.get(0) else {
            continue;
        };
        output.push_str(&input[last_end..matched.start()]);
        output.push_str(&redacted_match(&captures, kind, master_key)?);
        last_end = matched.end();
    }
    output.push_str(&input[last_end..]);
    Ok(output)
}

fn redacted_match(
    captures: &Captures<'_>,
    kind: &str,
    master_key: &MasterKey,
) -> Result<String, RedactionError> {
    let secret = captures
        .get(2)
        .or_else(|| captures.get(1))
        .map_or_else(|| captures.get(0), Some)
        .map(|matched| matched.as_str())
        .unwrap_or_default();
    let digest = master_key.keyed_digest(secret.as_bytes())?;
    let placeholder = format!("[REDACTED:{kind}:{digest}]");

    if let (Some(prefix), Some(secret)) = (captures.get(1), captures.get(2)) {
        let mut result = String::with_capacity(prefix.len() + placeholder.len());
        result.push_str(prefix.as_str());
        result.push_str(&placeholder);
        let full = captures.get(0).expect("full match");
        result.push_str(&full.as_str()[secret.end() - full.start()..]);
        Ok(result)
    } else {
        Ok(placeholder)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::Redactor;
    use agenttraceback_crypto::MasterKey;

    fn redactor() -> Redactor {
        Redactor::new(MasterKey::from_bytes(&[7_u8; 32]).expect("key"))
    }

    #[test]
    fn openai_key_is_redacted_with_stable_suffix() {
        let redactor = redactor();
        let first = redactor
            .redact_text("key=sk-abcdefghijklmnopqrstuvwxyz0123456789")
            .expect("redact");
        let second = redactor
            .redact_text("key=sk-abcdefghijklmnopqrstuvwxyz0123456789")
            .expect("redact");
        assert_eq!(first, second);
        assert!(!first.contains("abcdefghijklmnopqrstuvwxyz"));
        assert!(first.contains("[REDACTED:openai_key:"));
    }

    #[test]
    fn bearer_and_database_credentials_are_redacted() {
        let redactor = redactor();
        let text = "Authorization: Bearer abcdefghijklmnopqrstuvwxyz\nDATABASE_URL=postgres://user:password@db.example/app";
        let redacted = redactor.redact_text(text).expect("redact");
        assert!(!redacted.contains("abcdefghijklmnopqrstuvwxyz"));
        assert!(!redacted.contains("password@"));
        assert!(redacted.contains("[REDACTED:credential_url:"));
    }

    #[test]
    fn private_key_body_is_redacted() {
        let redactor = redactor();
        let text = "-----BEGIN PRIVATE KEY-----\nvery-secret-body\n-----END PRIVATE KEY-----";
        let redacted = redactor.redact_text(text).expect("redact");
        assert!(!redacted.contains("very-secret-body"));
        assert!(redacted.contains("[REDACTED:private_key:"));
    }

    #[test]
    fn json_redaction_traverses_arrays_and_objects() {
        let redactor = redactor();
        let value = json!({
            "token": "ghp_abcdefghijklmnopqrstuvwxyz123456",
            "nested": ["safe", "Bearer abcdefghijklmnopqrstuvwxyz"]
        });
        let redacted = redactor.redact_json(&value).expect("redact");
        let serialized = serde_json::to_string(&redacted).expect("serialize");
        assert!(!serialized.contains("abcdefghijklmnopqrstuvwxyz"));
        assert!(serialized.contains("[REDACTED:github_token:"));
        assert!(serialized.contains("[REDACTED:bearer_token:"));
    }
}
