use memnoth_engine::audit::redact;
use regex::Regex;
use std::sync::LazyLock;

/// Regex to detect credential-like keys
static CREDENTIAL_KEY_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(api[_-]?key|secret|token|password|passwd|auth|credential|access[_-]?key)")
        .unwrap()
});

/// Check if a string looks like it contains a credential key
fn is_credential_key(key: &str) -> bool {
    CREDENTIAL_KEY_PATTERN.is_match(key)
}

/// Redact every string inside a JSON value, recursively.
fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => serde_json::Value::String(redact(s)),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_value).collect())
        }
        serde_json::Value::Object(map) => {
            let mut new_map = serde_json::Map::new();
            for (k, v) in map {
                new_map.insert(k.clone(), redact_value(v));
            }
            serde_json::Value::Object(new_map)
        }
        other => other.clone(),
    }
}

/// Redact a bearer token from an Authorization header value.
///
/// Input: "Bearer sk-1234567890abcdef"
/// Output: "Bearer ***"
pub fn redact_header_value(header: &str) -> String {
    let lower = header.to_ascii_lowercase();
    if let Some(pos) = lower.find("bearer ") {
        let token_start = pos + "bearer ".len();
        let before = &header[..token_start];
        format!("{}***", before)
    } else {
        redact(header)
    }
}

/// Redact tokens from a string using common patterns.
///
/// Matches and redacts:
/// - Bearer tokens: `Bearer <token>`
/// - API key patterns: `sk-...`, `pk-...`, `api_key=...`, `token=...`
/// - Generic key=value where key suggests a credential
pub fn redact_token(input: &str) -> String {
    redact(input)
}

/// Redact a token value specifically, keeping a prefix for identification.
///
/// Input: "sk-1234567890abcdef"
/// Output: "sk-1234***"
pub fn redact_token_value(token: &str) -> String {
    if token.len() <= 8 {
        return "***".to_string();
    }
    format!("{}***", &token[..4])
}

/// Redact an Authorization header for logging.
///
/// Keeps the scheme (Bearer, Basic, etc.) but redacts the credential.
pub fn redact_auth_header(header: &str) -> String {
    let parts: Vec<&str> = header.splitn(2, ' ').collect();
    if parts.len() == 2 {
        format!("{} ***", parts[0])
    } else {
        "***".to_string()
    }
}

/// Redact URL credentials.
///
/// Input: "https://user:pass@example.com/path"
/// Output: "https://user:***@example.com/path"
pub fn redact_url(url: &str) -> String {
    redact(url)
}

/// Check if a string looks like it contains a credential.
pub fn looks_like_credential(key: &str) -> bool {
    is_credential_key(key)
}

/// Redact JSON value recursively, masking credential-like fields.
pub fn redact_json_value(value: &serde_json::Value) -> serde_json::Value {
    redact_value(value)
}

/// Redact a command line for logging, masking --token and similar flags.
pub fn redact_command_line(args: &[String]) -> Vec<String> {
    let mut result = Vec::with_capacity(args.len());
    let mut skip_next = false;

    for arg in args {
        if skip_next {
            result.push("***".to_string());
            skip_next = false;
            continue;
        }

        if arg == "--token" || arg == "--api-key" || arg == "-t" || arg == "-k" {
            result.push(arg.clone());
            skip_next = true;
        } else if arg.starts_with("--token=") || arg.starts_with("--api-key=") {
            let eq_pos = arg.find('=').unwrap();
            result.push(format!("{}***", &arg[..=eq_pos]));
        } else {
            result.push(redact_token(arg));
        }
    }

    result
}

/// Redact environment variables for logging.
pub fn redact_env_vars(vars: &[(String, String)]) -> Vec<(String, String)> {
    vars.iter()
        .map(|(k, v)| {
            if looks_like_credential(k) {
                (k.clone(), "***".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_header_value_bearer() {
        assert_eq!(
            redact_header_value("Bearer sk-1234567890abcdef"),
            "Bearer ***"
        );
        assert_eq!(
            redact_header_value("bearer sk-1234567890abcdef"),
            "bearer ***"
        );
        assert_eq!(redact_header_value("Bearer  abc"), "Bearer ***");
    }

    #[test]
    fn redact_header_value_non_bearer() {
        assert_eq!(
            redact_header_value("Basic dXNlcjpwYXNz"),
            "Basic dXNlcjpwYXNz"
        );
    }

    #[test]
    fn test_redact_token_value() {
        assert_eq!(redact_token_value("sk-1234567890abcdef"), "sk-1***");
        assert_eq!(redact_token_value("short"), "***");
        assert_eq!(redact_token_value(""), "***");
    }

    #[test]
    fn test_redact_auth_header() {
        assert_eq!(redact_auth_header("Bearer token123"), "Bearer ***");
        assert_eq!(redact_auth_header("Basic dXNlcjpwYXNz"), "Basic ***");
        assert_eq!(redact_auth_header("Token abc"), "Token ***");
        assert_eq!(redact_auth_header("NoSpace"), "***");
    }

    #[test]
    fn redact_command_line_token_flag() {
        let args = vec![
            "memnoth".to_string(),
            "--token".to_string(),
            "sk-secret123".to_string(),
            "chat".to_string(),
        ];
        let redacted = redact_command_line(&args);
        assert_eq!(redacted[0], "memnoth");
        assert_eq!(redacted[1], "--token");
        assert_eq!(redacted[2], "***");
        assert_eq!(redacted[3], "chat");
    }

    #[test]
    fn redact_command_line_token_equals() {
        let args = vec![
            "memnoth".to_string(),
            "--token=sk-secret123".to_string(),
            "chat".to_string(),
        ];
        let redacted = redact_command_line(&args);
        assert_eq!(redacted[1], "--token=***");
    }

    #[test]
    fn test_redact_env_vars() {
        let vars = vec![
            ("MEMNOTH_API_KEY".to_string(), "sk-secret".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("OPENAI_API_KEY".to_string(), "sk-openai".to_string()),
        ];
        let redacted = redact_env_vars(&vars);
        assert_eq!(
            redacted[0],
            ("MEMNOTH_API_KEY".to_string(), "***".to_string())
        );
        assert_eq!(redacted[1], ("PATH".to_string(), "/usr/bin".to_string()));
        assert_eq!(
            redacted[2],
            ("OPENAI_API_KEY".to_string(), "***".to_string())
        );
    }

    #[test]
    fn test_looks_like_credential() {
        assert!(looks_like_credential("API_KEY"));
        assert!(looks_like_credential("SECRET"));
        assert!(looks_like_credential("TOKEN"));
        assert!(looks_like_credential("PASSWORD"));
        assert!(looks_like_credential("env.OPENAI_API_KEY"));
        assert!(!looks_like_credential("USERNAME"));
        assert!(!looks_like_credential("PATH"));
    }
}
