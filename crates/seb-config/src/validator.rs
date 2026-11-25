use crate::SebValue;

/// 100% parity with C++ `DataValidator` (`configuration/configuration_data/data_validator.cpp`):
/// - Password hash fields (`hashedAdminPassword`, `hashedQuitPassword`, `sebServerFallbackPasswordHash`)
///   must be empty or exactly 64 hex chars.
/// - Every dictionary key and string value recursively must NOT match `"\s*,` (canonical JSON injection).
pub fn validate_plist_data(root: &SebValue) -> bool {
    let SebValue::Map(entries) = root else {
        return false;
    };
    validate_passwords(entries) && validate_string_values(root)
}

fn is_valid_sha256_or_empty(s: &str) -> bool {
    s.is_empty() || (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn validate_passwords(entries: &[(String, SebValue)]) -> bool {
    for key in [
        "hashedAdminPassword",
        "hashedQuitPassword",
        "sebServerFallbackPasswordHash",
    ] {
        if let Some((_, val)) = entries.iter().find(|(k, _)| k == key) {
            match val {
                SebValue::Empty => {}
                SebValue::String(s) => {
                    if !is_valid_sha256_or_empty(s) {
                        return false;
                    }
                }
                _ => return false,
            }
        }
    }
    true
}

/// Rejects `"\s*,` (double quote followed by optional ASCII whitespace then comma)
/// to prevent canonical JSON structure tampering.
fn contains_json_injection(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b',' {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn validate_string_values(val: &SebValue) -> bool {
    match val {
        SebValue::Map(entries) => {
            for (k, v) in entries {
                if contains_json_injection(k) || !validate_string_values(v) {
                    return false;
                }
            }
            true
        }
        SebValue::List(items) => items.iter().all(validate_string_values),
        SebValue::String(s) => !contains_json_injection(s),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_password_hash_and_json_injection() {
        let ok = SebValue::Map(vec![
            ("hashedQuitPassword".into(), SebValue::String("a".repeat(64))),
            ("startURL".into(), SebValue::String("https://example.org".into())),
        ]);
        assert!(validate_plist_data(&ok));

        let bad_pw = SebValue::Map(vec![(
            "hashedAdminPassword".into(),
            SebValue::String("not-a-sha256".into()),
        )]);
        assert!(!validate_plist_data(&bad_pw));

        let bad_inj = SebValue::Map(vec![(
            "startURL".into(),
            SebValue::String(r#"https://example.org","allowQuit":true"#.into()),
        )]);
        assert!(!validate_plist_data(&bad_inj));
    }
}
