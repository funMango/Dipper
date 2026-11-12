//! Shared text helpers for command output.

use crate::config::{Config, SourceConfig};

/// Replace known secret values with `***`.
///
/// Empty values are ignored. Longer values are replaced first so a short
/// secret that is a prefix of a longer one cannot split the longer value.
pub fn redact_known_values(text: &str, secrets: &[&str]) -> String {
    let mut ordered: Vec<&str> = secrets
        .iter()
        .copied()
        .filter(|secret| !secret.is_empty())
        .collect();
    ordered.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    ordered.dedup();
    let mut redacted = text.to_string();
    for secret in ordered {
        redacted = redacted.replace(secret, "***");
    }
    redacted
}

/// Oracle connect, user, and password strings resolved from the config.
pub fn oracle_secret_values(config: &Config) -> Vec<String> {
    let mut values = Vec::new();
    for source in config.sources.values() {
        if let SourceConfig::Oracle {
            connect,
            user,
            password,
            ..
        } = source
        {
            values.push(connect.clone());
            values.push(user.clone());
            values.push(password.expose().to_string());
        }
    }
    values
}

/// Hide known Oracle secrets in `message`.
pub fn redact_with_config(config: &Config, message: &str) -> String {
    let owned = oracle_secret_values(config);
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    redact_known_values(message, &refs)
}

#[cfg(test)]
mod tests {
    use super::redact_known_values;

    #[test]
    fn redact_known_values_replaces_longest_first_and_skips_empty() {
        let got = redact_known_values(
            "see password-and-more then password and tail",
            &["", "password", "password-and-more"],
        );
        assert_eq!(got, "see *** then *** and tail");
        assert!(!got.contains("password"));
        assert_eq!(redact_known_values("abc", &[""]), "abc");
        assert_eq!(redact_known_values("abc", &[]), "abc");
    }
}
