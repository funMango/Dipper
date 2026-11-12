//! Shared text helpers for command output.

use crate::config::{Config, SourceConfig};
use crate::error::DipperError;

/// Fixed text used when a message contains a known secret.
///
/// The whole message is replaced. Pieces of the secret are not left in place,
/// because a short value such as `1` or `DPI` would show where it had been.
pub const SECRET_WITHHELD: &str = "details withheld";

const ALREADY_HIDDEN_DRIVER: &str = "(driver message hidden because it contained the password)";

/// One configured value that must not appear in command output.
pub struct KnownSecret {
    pub value: String,
    /// User names match without regard to ASCII case. Passwords do not.
    pub ignore_ascii_case: bool,
}

/// Replace `text` when it contains any known secret.
///
/// Empty values are ignored. A message that already came from
/// [`crate::source`] driver redaction, or that is already this function's
/// withheld text, is returned unchanged. Otherwise the whole message becomes
/// [`SECRET_WITHHELD`], keeping Oracle `ORA-#####` and `DPI-####` codes that
/// do not themselves contain a secret.
pub fn redact_known_values(text: &str, secrets: &[KnownSecret]) -> String {
    if already_withheld(text) {
        return text.to_string();
    }
    let active: Vec<&KnownSecret> = secrets
        .iter()
        .filter(|secret| !secret.value.is_empty())
        .collect();
    if active.is_empty() || !active.iter().any(|secret| contains_secret(text, secret)) {
        return text.to_string();
    }
    let codes: Vec<String> = driver_codes(text)
        .into_iter()
        .filter(|code| !active.iter().any(|secret| contains_secret(code, secret)))
        .collect();
    if codes.is_empty() {
        SECRET_WITHHELD.to_string()
    } else {
        format!("{} {SECRET_WITHHELD}", codes.join(" "))
    }
}

/// Oracle connect, user, password, and the host, port, and service taken from
/// the connect string. Percent-encoded connect text is not decoded.
pub fn oracle_secret_values(config: &Config) -> Vec<KnownSecret> {
    let mut values = Vec::new();
    for source in config.sources.values() {
        if let SourceConfig::Oracle {
            connect,
            user,
            password,
            ..
        } = source
        {
            push_secret(&mut values, connect.clone(), false);
            for part in connect_parts(connect) {
                push_secret(&mut values, part, false);
            }
            push_secret(&mut values, user.clone(), true);
            push_secret(&mut values, password.expose().to_string(), false);
        }
    }
    values
}

/// Hide known Oracle secrets in `message`.
pub fn redact_with_config(config: &Config, message: &str) -> String {
    redact_known_values(message, &oracle_secret_values(config))
}

/// Redact every dip or ontology error before it is printed or stored.
///
/// The displayed text is what `main` writes after `dipper:`. When that text
/// contains a secret, the error is rewrapped so the exit code stays the same
/// and the secret is gone. Text that contains no secret is left as it was.
pub fn redact_error(error: DipperError, secrets: &[KnownSecret]) -> DipperError {
    if secrets.iter().all(|secret| secret.value.is_empty()) {
        return error;
    }
    let shown = error.to_string();
    let hidden = redact_known_values(&shown, secrets);
    if hidden == shown {
        return error;
    }
    if error.exit_code() == 2 {
        DipperError::UnknownDip(hidden)
    } else {
        DipperError::Source {
            source_name: "dip".to_string(),
            message: hidden,
        }
    }
}

fn push_secret(values: &mut Vec<KnownSecret>, value: String, ignore_ascii_case: bool) {
    if value.is_empty() || values.iter().any(|secret| secret.value == value) {
        return;
    }
    values.push(KnownSecret {
        value,
        ignore_ascii_case,
    });
}

/// `host[:port][/service]`, with an optional `//` prefix.
///
/// This is the easy-connect form stored in config. Percent-encoded octets are
/// kept literally and are not turned back into characters.
pub fn connect_parts(connect: &str) -> Vec<String> {
    let trimmed = connect.trim();
    let rest = trimmed.strip_prefix("//").unwrap_or(trimmed);
    let rest = rest.split('?').next().unwrap_or(rest);
    let (hostport, service) = match rest.split_once('/') {
        Some((hostport, service)) => (hostport, Some(service)),
        None => (rest, None),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((host, port))
            if !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            (host, Some(port))
        }
        _ => (hostport, None),
    };
    let mut parts = Vec::new();
    if !host.is_empty() && host != connect {
        parts.push(host.to_string());
    }
    if let Some(port) = port {
        if port != connect {
            parts.push(port.to_string());
        }
    }
    if let Some(service) = service.filter(|service| !service.is_empty() && *service != connect) {
        parts.push(service.to_string());
    }
    parts
}

fn already_withheld(text: &str) -> bool {
    text == SECRET_WITHHELD
        || text.ends_with(&format!(" {SECRET_WITHHELD}"))
        || text.contains(ALREADY_HIDDEN_DRIVER)
}

fn contains_secret(text: &str, secret: &KnownSecret) -> bool {
    let needle = secret.value.as_str();
    if needle.is_empty() || needle.len() > text.len() {
        return false;
    }
    if !secret.ignore_ascii_case {
        return text.contains(needle);
    }
    let bytes = text.as_bytes();
    let needle_bytes = needle.as_bytes();
    let width = needle_bytes.len();
    let mut index = 0;
    while index + width <= bytes.len() {
        if text.is_char_boundary(index)
            && text.is_char_boundary(index + width)
            && bytes[index..index + width].eq_ignore_ascii_case(needle_bytes)
        {
            return true;
        }
        index += 1;
    }
    false
}

fn driver_codes(text: &str) -> Vec<String> {
    let mut codes = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(code) =
            code_at(text, index, "ORA-", 5).or_else(|| code_at(text, index, "DPI-", 4))
        {
            index += code.len();
            codes.push(code.to_string());
            continue;
        }
        index += 1;
    }
    codes
}

fn code_at<'a>(text: &'a str, index: usize, prefix: &str, digits: usize) -> Option<&'a str> {
    let rest = text.get(index..)?;
    if !rest.starts_with(prefix) {
        return None;
    }
    let end = index + prefix.len() + digits;
    let code = text.get(index..end)?;
    if !code.as_bytes()[prefix.len()..]
        .iter()
        .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let bounded = text
        .as_bytes()
        .get(end)
        .is_none_or(|byte| !byte.is_ascii_digit());
    if bounded {
        Some(code)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{connect_parts, redact_known_values, KnownSecret, SECRET_WITHHELD};

    fn secret(value: &str) -> KnownSecret {
        KnownSecret {
            value: value.to_string(),
            ignore_ascii_case: false,
        }
    }

    fn user(value: &str) -> KnownSecret {
        KnownSecret {
            value: value.to_string(),
            ignore_ascii_case: true,
        }
    }

    #[test]
    fn redact_known_values_replaces_longest_first_and_skips_empty() {
        let got = redact_known_values(
            "see password-and-more then password and tail",
            &[secret(""), secret("password"), secret("password-and-more")],
        );
        assert_eq!(got, SECRET_WITHHELD);
        assert!(!got.contains("password"));
        assert!(!got.contains("***"));
        assert_eq!(redact_known_values("abc", &[secret("")]), "abc");
        assert_eq!(redact_known_values("abc", &[]), "abc");
    }

    #[test]
    fn short_secrets_do_not_leave_a_guessable_remainder() {
        let original = "DPI-1047: Oracle client failed for user at step 1";
        for password in ["DPI", "Oracle", "1"] {
            let got = redact_known_values(original, &[secret(password)]);
            assert!(
                !got.contains(password),
                "password {password} can be read from {got}"
            );
            assert!(!got.contains("***"), "{got}");
            assert!(
                got == SECRET_WITHHELD || got.ends_with(&format!(" {SECRET_WITHHELD}")),
                "{password} -> {got}"
            );
        }
    }

    #[test]
    fn already_hidden_driver_text_is_not_rewritten() {
        let hidden = "ORA-01017 (driver message hidden because it contained the password)";
        let got = redact_known_values(hidden, &[secret("password"), secret("ORA")]);
        assert_eq!(got, hidden);
    }

    #[test]
    fn user_match_ignores_ascii_case_and_connect_parts_are_secrets() {
        let got = redact_known_values("login READER failed", &[user("reader")]);
        assert_eq!(got, SECRET_WITHHELD);
        assert!(!got.to_ascii_lowercase().contains("reader"));

        assert_eq!(
            connect_parts("db.example:1521/SVC"),
            vec![
                "db.example".to_string(),
                "1521".to_string(),
                "SVC".to_string()
            ]
        );
        let parts = connect_parts("//db.example:1521/SVC");
        let mut secrets: Vec<KnownSecret> = parts.into_iter().map(|part| secret(&part)).collect();
        secrets.push(secret("db.example:1521/SVC"));
        let got = redact_known_values("down at db.example tonight", &secrets);
        assert_eq!(got, SECRET_WITHHELD);
        assert!(!got.contains("db.example"));
    }

    #[test]
    fn driver_code_without_the_secret_is_kept() {
        let got = redact_known_values("ORA-01017: logon denied for READER", &[user("reader")]);
        assert_eq!(got, format!("ORA-01017 {SECRET_WITHHELD}"));
        assert!(!got.to_ascii_lowercase().contains("reader"));
    }
}
