use crate::config::schema::Secret;
use crate::error::ConfigIssue;

/// Substitute `${NAME}` references. `NAME` matches `[A-Za-z_][A-Za-z0-9_]*`.
/// A `$` that is not followed by `{` is left unchanged.
pub fn substitute(
    input: &str,
    field_path: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, ConfigIssue> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if !after.starts_with('{') {
            out.push('$');
            rest = after;
            continue;
        }
        let body = &after[1..];
        let Some(end) = body.find('}') else {
            return Err(issue(
                field_path,
                format!("unclosed environment variable reference ${{{body}"),
            ));
        };
        let name = &body[..end];
        if !valid_env_name(name) {
            return Err(issue(
                field_path,
                format!("invalid environment variable name '{name}'"),
            ));
        }
        match lookup(name) {
            Some(value) => out.push_str(&value),
            None => {
                return Err(issue(
                    field_path,
                    format!("environment variable {name} is not set"),
                ));
            }
        }
        rest = &body[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `password` must be exactly one `${NAME}`. The input value is never copied into the error.
pub fn substitute_secret(
    input: &str,
    field_path: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Secret, ConfigIssue> {
    let Some(name) = exact_env_name(input) else {
        return Err(issue(
            field_path,
            "must be an environment variable reference like ${DIPPER_PASSWORD}",
        ));
    };
    match lookup(name) {
        Some(value) => Ok(Secret::new(value)),
        None => Err(issue(
            field_path,
            format!("environment variable {name} is not set"),
        )),
    }
}

fn exact_env_name(input: &str) -> Option<&str> {
    let name = input.strip_prefix("${")?.strip_suffix('}')?;
    valid_env_name(name).then_some(name)
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn issue(path: &str, message: impl Into<String>) -> ConfigIssue {
    ConfigIssue {
        path: path.to_string(),
        line_col: None,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(key: &str) -> Option<String> {
        match key {
            "A" => Some("alpha".to_string()),
            "B" => Some("beta".to_string()),
            "PASSWORD" => Some("s3cret".to_string()),
            _ => None,
        }
    }

    #[test]
    fn substitutes_one_variable() {
        let got = substitute("hello ${A}", "p", &lookup).unwrap();
        assert_eq!(got, "hello alpha");
    }

    #[test]
    fn substitutes_several_variables() {
        let got = substitute("${A}-${B}", "p", &lookup).unwrap();
        assert_eq!(got, "alpha-beta");
    }

    #[test]
    fn leaves_a_lone_dollar() {
        let got = substitute("cost is $5 and $$", "p", &lookup).unwrap();
        assert_eq!(got, "cost is $5 and $$");
    }

    #[test]
    fn unclosed_reference_names_the_variable() {
        let err = substitute("pre ${NAME", "field.path", &lookup).unwrap_err();
        assert_eq!(err.path, "field.path");
        assert!(err.message.contains("NAME"), "{}", err.message);
    }

    #[test]
    fn invalid_name_is_reported() {
        let err = substitute("${1ABC}", "p", &lookup).unwrap_err();
        assert!(err.message.contains("1ABC"), "{}", err.message);
    }

    #[test]
    fn unset_variable_names_the_variable() {
        let err = substitute("${MISSING}", "sources.x.path", &lookup).unwrap_err();
        assert_eq!(err.path, "sources.x.path");
        assert!(err.message.contains("MISSING"), "{}", err.message);
    }

    #[test]
    fn secret_rejects_literals_without_echoing_them() {
        let err = substitute_secret("PR2-canary-literal", "sources.company.password", &lookup)
            .unwrap_err();
        assert_eq!(err.path, "sources.company.password");
        assert!(err.message.contains("${"), "{}", err.message);
        assert!(!err.message.contains("PR2-canary-literal"));
    }

    #[test]
    fn secret_substitutes_exact_reference() {
        let secret = substitute_secret("${PASSWORD}", "p", &lookup).unwrap();
        assert_eq!(secret.expose(), "s3cret");
        assert_eq!(format!("{secret:?}"), "Secret(***)");
        assert!(!format!("{secret:?}").contains("s3cret"));
    }

    #[test]
    fn secret_unset_names_the_variable() {
        let err = substitute_secret("${MISSING}", "sources.company.password", &lookup).unwrap_err();
        assert!(err.message.contains("MISSING"), "{}", err.message);
        assert!(!err.message.contains("${MISSING}"));
    }
}
