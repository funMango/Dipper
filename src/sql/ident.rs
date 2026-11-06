/// `^[A-Za-z_][A-Za-z0-9_$#.]*$`, and no empty segment between dots.
pub fn validate_ident(s: &str) -> Result<(), String> {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return Err("invalid identifier".to_string()),
    }
    if chars.any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '#' | '.'))) {
        return Err("invalid identifier".to_string());
    }
    if s.split('.').any(str::is_empty) {
        return Err("invalid identifier".to_string());
    }
    Ok(())
}

/// `^[A-Za-z_][A-Za-z0-9_]*$`, and not a reserved `_dipper*` / `*__staging` name.
pub fn validate_dip_name(s: &str) -> Result<(), String> {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return Err("invalid dip name".to_string()),
    }
    if chars.any(|c| !(c.is_ascii_alphanumeric() || c == '_')) {
        return Err("invalid dip name".to_string());
    }
    if s.starts_with("_dipper") {
        return Err("invalid dip name: must not start with _dipper".to_string());
    }
    if s.ends_with("__staging") {
        return Err("invalid dip name: must not end with __staging".to_string());
    }
    Ok(())
}

/// One column, key, or filter name. A dot is only allowed in a table name.
pub fn validate_column_ident(s: &str) -> Result<(), String> {
    if s.contains('.') {
        return Err("invalid identifier".to_string());
    }
    validate_ident(s)
}

/// Quote each dot-separated segment of an already validated identifier.
pub fn quote_ident(s: &str) -> String {
    s.split('.')
        .map(|part| format!("\"{part}\""))
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_identifiers() {
        for ident in ["ORDERS", "SALES.ORDERS", "A$B#C", "_x1"] {
            assert!(validate_ident(ident).is_ok(), "{ident}");
        }
    }

    #[test]
    fn rejects_identifiers() {
        for ident in ["", "1A", "A B", "A;B", "A--", "\"A\"", "A.", ".A", "A..B"] {
            let err = validate_ident(ident).unwrap_err();
            assert!(err.contains("invalid identifier"), "{ident}: {err}");
        }
    }

    #[test]
    fn column_names_cannot_contain_a_dot() {
        assert!(validate_column_ident("ORDER_ID").is_ok());
        assert!(validate_column_ident("A$B#C").is_ok());
        let err = validate_column_ident("PKG.F").unwrap_err();
        assert!(err.contains("invalid identifier"), "{err}");
        assert!(validate_ident("SALES.ORDERS").is_ok());
    }

    #[test]
    fn quotes_qualified_ident() {
        assert_eq!(quote_ident("SALES.ORDERS"), "\"SALES\".\"ORDERS\"");
    }

    #[test]
    fn dip_names() {
        assert!(validate_dip_name("orders").is_ok());
        assert!(validate_dip_name("_x1").is_ok());
        assert!(validate_dip_name("_dipper").is_err());
        assert!(validate_dip_name("_dipper_runs").is_err());
        assert!(validate_dip_name("orders__staging").is_err());
        assert!(validate_dip_name("1A").is_err());
        assert!(validate_dip_name("A-B").is_err());
    }
}
