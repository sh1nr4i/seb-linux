pub mod crypto_pk;
pub mod loader;
pub mod plist;
pub mod settings;
pub mod validator;

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use base64::Engine;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use subtle::ConstantTimeEq;

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
type HmacSha256 = Hmac<Sha256>;

const RNCRYPTOR_ITERATIONS: u32 = 10_000;
const RNCRYPTOR_KEY_SIZE: usize = 32;
const RNCRYPTOR_SALT_SIZE: usize = 8;
const RNCRYPTOR_IV_SIZE: usize = 16;
const RNCRYPTOR_HMAC_SIZE: usize = 32;
const RNCRYPTOR_VERSION: u8 = 2;
const RNCRYPTOR_OPTIONS: u8 = 1;

// =========================================================================
// GZip Compression (100% parity with GZipCompressor)
// =========================================================================

pub fn gzip_is_compressed(data: &[u8]) -> bool {
    data.len() >= 3 && data[0] == 0x1f && data[1] == 0x8b && data[2] == 0x08
}

pub fn gzip_compress(data: &[u8]) -> Option<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).ok()?;
    encoder.finish().ok()
}

pub fn gzip_decompress(data: &[u8]) -> Option<Vec<u8>> {
    if !gzip_is_compressed(data) {
        return None;
    }
    let mut decoder = GzDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    Some(out)
}

pub fn gzip_peek(data: &[u8], count: usize) -> Option<Vec<u8>> {
    if count == 0 || !gzip_is_compressed(data) {
        return None;
    }
    let mut decoder = GzDecoder::new(data);
    let mut buf = vec![0u8; count];
    let mut total = 0usize;
    while total < count {
        match decoder.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => {
                if total == 0 {
                    return None;
                }
                break;
            }
        }
    }
    buf.truncate(total);
    Some(buf)
}

// =========================================================================
// Hashing & SEB Keys (100% parity with HashAlgorithm & KeyGenerator)
// =========================================================================

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn url_without_fragment(url: &str) -> &str {
    match url.find('#') {
        Some(idx) => &url[..idx],
        None => url,
    }
}

pub fn compute_browser_exam_key(
    code_signature_hash: &str,
    program_build_version: &str,
    configuration_key: &str,
    salt: &[u8],
) -> String {
    let input = format!("{code_signature_hash}{program_build_version}{configuration_key}");
    let mut mac = <HmacSha256 as Mac>::new_from_slice(salt)
        .expect("HMAC accepts any key length");
    mac.update(input.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

pub fn calculate_browser_exam_key_hash(
    browser_exam_key: &str,
    url: &str,
) -> String {
    let base = url_without_fragment(url);
    let mut hasher = Sha256::new();
    hasher.update(base.as_bytes());
    hasher.update(browser_exam_key.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn calculate_configuration_key_hash(configuration_key: &str, url: &str) -> String {
    let base = url_without_fragment(url);
    let mut hasher = Sha256::new();
    hasher.update(base.as_bytes());
    hasher.update(configuration_key.as_bytes());
    hex::encode(hasher.finalize())
}

// =========================================================================
// RNCryptor v2 Password Encryption (100% parity with PasswordEncryption)
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum DecryptStatus {
    Success = 0,
    PasswordNeeded = 1,
    InvalidData = 2,
    UnexpectedError = 3,
}

pub fn rncryptor_decrypt(data: &[u8], password: &str) -> Result<Vec<u8>, DecryptStatus> {
    let min_len = 2 + 2 * RNCRYPTOR_SALT_SIZE + RNCRYPTOR_IV_SIZE + RNCRYPTOR_HMAC_SIZE;
    if data.len() < min_len {
        return Err(DecryptStatus::InvalidData);
    }

    let enc_salt = &data[2..2 + RNCRYPTOR_SALT_SIZE];
    let auth_salt = &data[2 + RNCRYPTOR_SALT_SIZE..2 + 2 * RNCRYPTOR_SALT_SIZE];
    let iv_start = 2 + 2 * RNCRYPTOR_SALT_SIZE;
    let iv = &data[iv_start..iv_start + RNCRYPTOR_IV_SIZE];
    let payload_start = iv_start + RNCRYPTOR_IV_SIZE;
    let payload_end = data.len() - RNCRYPTOR_HMAC_SIZE;
    let payload = &data[payload_start..payload_end];
    let original_hmac = &data[payload_end..];

    let mut enc_key = [0u8; RNCRYPTOR_KEY_SIZE];
    let mut auth_key = [0u8; RNCRYPTOR_KEY_SIZE];
    pbkdf2_hmac::<Sha1>(password.as_bytes(), enc_salt, RNCRYPTOR_ITERATIONS, &mut enc_key);
    pbkdf2_hmac::<Sha1>(password.as_bytes(), auth_salt, RNCRYPTOR_ITERATIONS, &mut auth_key);

    let mut mac = <HmacSha256 as Mac>::new_from_slice(&auth_key)
        .map_err(|_| DecryptStatus::UnexpectedError)?;
    mac.update(&data[..payload_end]);
    let computed_hmac = mac.finalize().into_bytes();

    if computed_hmac.as_slice().ct_eq(original_hmac).unwrap_u8() != 1 {
        return Err(DecryptStatus::PasswordNeeded);
    }

    let mut buf = payload.to_vec();
    let decrypted = Aes256CbcDec::new_from_slices(&enc_key, iv)
        .map_err(|_| DecryptStatus::UnexpectedError)?
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| DecryptStatus::InvalidData)?;
    Ok(decrypted.to_vec())
}

pub fn rncryptor_encrypt(plain: &[u8], password: &str) -> Option<Vec<u8>> {
    let mut enc_salt = [0u8; RNCRYPTOR_SALT_SIZE];
    let mut auth_salt = [0u8; RNCRYPTOR_SALT_SIZE];
    let mut iv = [0u8; RNCRYPTOR_IV_SIZE];
    let mut rng = rand::thread_rng();
    rng.fill_bytes(&mut enc_salt);
    rng.fill_bytes(&mut auth_salt);
    rng.fill_bytes(&mut iv);

    let mut enc_key = [0u8; RNCRYPTOR_KEY_SIZE];
    let mut auth_key = [0u8; RNCRYPTOR_KEY_SIZE];
    pbkdf2_hmac::<Sha1>(password.as_bytes(), &enc_salt, RNCRYPTOR_ITERATIONS, &mut enc_key);
    pbkdf2_hmac::<Sha1>(password.as_bytes(), &auth_salt, RNCRYPTOR_ITERATIONS, &mut auth_key);

    let mut buf = vec![0u8; plain.len() + RNCRYPTOR_IV_SIZE];
    buf[..plain.len()].copy_from_slice(plain);
    let cipher_len = Aes256CbcEnc::new_from_slices(&enc_key, &iv)
        .ok()?
        .encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len())
        .ok()?
        .len();
    buf.truncate(cipher_len);

    let mut out = Vec::with_capacity(
        2 + 2 * RNCRYPTOR_SALT_SIZE + RNCRYPTOR_IV_SIZE + cipher_len + RNCRYPTOR_HMAC_SIZE,
    );
    out.push(RNCRYPTOR_VERSION);
    out.push(RNCRYPTOR_OPTIONS);
    out.extend_from_slice(&enc_salt);
    out.extend_from_slice(&auth_salt);
    out.extend_from_slice(&iv);
    out.extend_from_slice(&buf);

    let mut mac = <HmacSha256 as Mac>::new_from_slice(&auth_key).ok()?;
    mac.update(&out);
    out.extend_from_slice(&mac.finalize().into_bytes());
    Some(out)
}

// =========================================================================
// SEB Canonical JSON Serializer (100% parity with configuration_data/json.cpp)
// =========================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", content = "v")]
pub enum SebValue {
    Map(Vec<(String, SebValue)>),
    List(Vec<SebValue>),
    Bytes(String), // already base64
    DateTime(String),
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    String(String),
    Empty,
}

fn is_originator_version(key: &str) -> bool {
    key.eq_ignore_ascii_case("originatorVersion")
}

fn seb_key_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let lf = left.to_lowercase();
    let rf = right.to_lowercase();
    match lf.cmp(&rf) {
        std::cmp::Ordering::Equal => left.cmp(right),
        ord => ord,
    }
}

fn has_serializable_entries(entries: &[(String, SebValue)]) -> bool {
    for (k, v) in entries {
        if is_originator_version(k) {
            continue;
        }
        if let SebValue::Map(child) = v {
            if !has_serializable_entries(child) {
                continue;
            }
        }
        return true;
    }
    false
}

fn append_string(out: &mut Vec<u8>, value: &str) {
    out.push(b'"');
    out.extend_from_slice(value.as_bytes());
    out.push(b'"');
}

fn append_map(out: &mut Vec<u8>, entries: &[(String, SebValue)]) {
    let mut filtered: Vec<&(String, SebValue)> = entries
        .iter()
        .filter(|(k, v)| {
            if is_originator_version(k) {
                return false;
            }
            if let SebValue::Map(child) = v {
                return has_serializable_entries(child);
            }
            true
        })
        .collect();
    filtered.sort_by(|a, b| seb_key_cmp(&a.0, &b.0));

    out.push(b'{');
    for (idx, (k, v)) in filtered.iter().enumerate() {
        if idx > 0 {
            out.push(b',');
        }
        append_string(out, k);
        out.push(b':');
        append_value(out, v);
    }
    out.push(b'}');
}

fn append_value(out: &mut Vec<u8>, value: &SebValue) {
    match value {
        SebValue::Map(entries) => append_map(out, entries),
        SebValue::List(items) => {
            out.push(b'[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                append_value(out, item);
            }
            out.push(b']');
        }
        SebValue::Bytes(b64) => append_string(out, b64),
        SebValue::DateTime(dt) => {
            if dt.is_empty() {
                append_string(out, "");
            } else {
                append_canonical_datetime(out, dt);
            }
        }
        SebValue::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        SebValue::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        SebValue::Uint(u) => out.extend_from_slice(u.to_string().as_bytes()),
        SebValue::Float(f) => {
            out.extend_from_slice(format_canonical_float(*f).as_bytes());
        }
        SebValue::String(s) => append_string(out, s),
        SebValue::Empty => append_string(out, ""),
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let m = month as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + (day as u64) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + (doe as i64) - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year, m as u32, d as u32)
}

fn normalize_iso_datetime_utc(dt: &str) -> Option<(i64, u32, u32, u32, u32, u32, u32)> {
    let s = dt.trim();
    if s.len() < 19 {
        return None;
    }
    let sep = s.as_bytes()[10];
    if sep != b'T' && sep != b't' && sep != b' ' {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    if s.as_bytes().get(4).copied()? != b'-' || s.as_bytes().get(7).copied()? != b'-' {
        return None;
    }
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if s.as_bytes().get(13).copied()? != b':' || s.as_bytes().get(16).copied()? != b':' {
        return None;
    }
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    let minute: u32 = s.get(14..16)?.parse().ok()?;
    let second: u32 = s.get(17..19)?.parse().ok()?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let mut rest = &s[19..];
    let mut millis: u32 = 0;
    if let Some(after_dot) = rest.strip_prefix('.') {
        let digit_len = after_dot
            .bytes()
            .take_while(|b| b.is_ascii_digit())
            .count();
        let frac = &after_dot[..digit_len];
        let mut ms_chars = frac.chars();
        let d1 = ms_chars.next().and_then(|c| c.to_digit(10)).unwrap_or(0);
        let d2 = ms_chars.next().and_then(|c| c.to_digit(10)).unwrap_or(0);
        let d3 = ms_chars.next().and_then(|c| c.to_digit(10)).unwrap_or(0);
        millis = d1 * 100 + d2 * 10 + d3;
        rest = &after_dot[digit_len..];
    }

    let offset_secs: i64 = if rest.is_empty() || rest.eq_ignore_ascii_case("Z") {
        0
    } else if let Some(sign_mul) = rest
        .strip_prefix('+')
        .map(|_| 1i64)
        .or_else(|| rest.strip_prefix('-').map(|_| -1i64))
    {
        let tz = &rest[1..];
        let (oh, om): (i64, i64) = if let Some((h, m)) = tz.split_once(':') {
            (h.parse().ok()?, m.parse().ok()?)
        } else if tz.len() == 4 {
            (tz[0..2].parse().ok()?, tz[2..4].parse().ok()?)
        } else if tz.len() == 2 {
            (tz.parse().ok()?, 0)
        } else {
            return None;
        };
        sign_mul * (oh * 3600 + om * 60)
    } else {
        return None;
    };

    let local_secs =
        days_from_civil(year, month, day) * 86_400 + (hour as i64) * 3600 + (minute as i64) * 60 + (second as i64);
    let utc_secs = local_secs - offset_secs;
    let utc_days = utc_secs.div_euclid(86_400);
    let sec_of_day = utc_secs.rem_euclid(86_400) as u32;
    let (uy, um, ud) = civil_from_days(utc_days);
    Some((
        uy,
        um,
        ud,
        sec_of_day / 3600,
        (sec_of_day % 3600) / 60,
        sec_of_day % 60,
        millis,
    ))
}

fn append_canonical_datetime(out: &mut Vec<u8>, dt: &str) {
    // Matches C++ xml_parser.cpp (toUTC) + json.cpp appendDateTime:
    // ISO-8601 UTC with 7 fractional digits (.0000000Z), emitted unquoted when valid.
    if let Some((y, m, d, hh, mm, ss, ms)) = normalize_iso_datetime_utc(dt) {
        let formatted = format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{ms:03}0000Z");
        out.extend_from_slice(formatted.as_bytes());
    } else {
        append_string(out, "");
    }
}

fn format_canonical_float(f: f64) -> String {
    if !f.is_finite() || f == 0.0 {
        return "0".to_string();
    }
    // Matches C++ QString::number(val, 'g', 15)
    let neg = f < 0.0;
    let abs = f.abs();
    let sci = format!("{abs:.14e}");
    let (mantissa, exp_str) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp_str.parse().unwrap_or(0);
    let mut digits = String::with_capacity(15);
    for b in mantissa.bytes() {
        if b.is_ascii_digit() {
            digits.push(b as char);
        }
    }
    let trimmed = digits.trim_end_matches('0');
    let trimmed = if trimmed.is_empty() { "0" } else { trimmed };

    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if (-4..15).contains(&exp) {
        if exp < 0 {
            out.push_str("0.");
            for _ in 0..((-exp - 1) as usize) {
                out.push('0');
            }
            out.push_str(trimmed);
        } else {
            let int_len = (exp as usize) + 1;
            if int_len >= trimmed.len() {
                out.push_str(trimmed);
                for _ in 0..(int_len - trimmed.len()) {
                    out.push('0');
                }
            } else {
                out.push_str(&trimmed[..int_len]);
                out.push('.');
                out.push_str(&trimmed[int_len..]);
            }
        }
    } else {
        let mut chars = trimmed.chars();
        if let Some(first) = chars.next() {
            out.push(first);
            let rest: String = chars.collect();
            if !rest.is_empty() {
                out.push('.');
                out.push_str(&rest);
            }
        }
        out.push('e');
        if exp < 0 {
            out.push('-');
            out.push_str(&format!("{:02}", exp.abs()));
        } else {
            out.push('+');
            out.push_str(&format!("{exp:02}"));
        }
    }
    out
}

pub fn serialize_canonical_json(root: &SebValue) -> Vec<u8> {
    let mut out = Vec::new();
    append_value(&mut out, root);
    out
}

pub fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_roundtrip_and_peek() {
        let src = b"plnd<?xml version=\"1.0\"?><plist></plist>";
        let gz = gzip_compress(src).unwrap();
        assert!(gzip_is_compressed(&gz));
        assert_eq!(gzip_peek(&gz, 4).unwrap(), b"plnd");
        assert_eq!(gzip_decompress(&gz).unwrap(), src);
    }

    #[test]
    fn rncryptor_v2_roundtrip_and_wrong_password() {
        let secret = b"SafeExamBrowser plist payload";
        let enc = rncryptor_encrypt(secret, "exam-pass-123").unwrap();
        assert_eq!(rncryptor_decrypt(&enc, "exam-pass-123").unwrap(), secret);
        assert_eq!(
            rncryptor_decrypt(&enc, "wrong-pass").unwrap_err(),
            DecryptStatus::PasswordNeeded
        );
    }

    #[test]
    fn key_hashes_match_seb_spec() {
        let ck = "abcdef0123456789";
        let url = "https://lms.example.edu/mod/quiz/view.php?id=1#section";
        let h1 = calculate_configuration_key_hash(ck, url);
        let h2 = sha256_hex(b"https://lms.example.edu/mod/quiz/view.php?id=1abcdef0123456789");
        assert_eq!(h1, h2);

        let bek = compute_browser_exam_key("sig", "0.6.1", ck, b"salt");
        let req_hash = calculate_browser_exam_key_hash(&bek, url);
        assert_eq!(req_hash.len(), 64);
    }

    #[test]
    fn canonical_json_omits_originator_and_sorts_case_folded() {
        let root = SebValue::Map(vec![
            ("originatorVersion".into(), SebValue::String("3.10".into())),
            ("startURL".into(), SebValue::String("https://example.org".into())),
            ("allowQuit".into(), SebValue::Bool(true)),
            ("emptyDict".into(), SebValue::Map(vec![])),
            ("zeroFloat".into(), SebValue::Float(0.0)),
            ("negZero".into(), SebValue::Float(-0.0)),
            ("wholeFloat".into(), SebValue::Float(1.0)),
            ("fracFloat".into(), SebValue::Float(-0.25)),
            ("sigFloat".into(), SebValue::Float(1234.567890123456)),
            ("smallFloat".into(), SebValue::Float(1e-5)),
            ("largeFloat".into(), SebValue::Float(1e15)),
            ("utcDate".into(), SebValue::DateTime("2025-05-01T12:34:56Z".into())),
            ("tzDate".into(), SebValue::DateTime("2025-05-01T19:34:56.78+07:00".into())),
        ]);
        let serialized = String::from_utf8(serialize_canonical_json(&root)).unwrap();
        assert_eq!(
            serialized,
            r#"{"allowQuit":true,"fracFloat":-0.25,"largeFloat":1e+15,"negZero":0,"sigFloat":1234.56789012346,"smallFloat":1e-05,"startURL":"https://example.org","tzDate":2025-05-01T12:34:56.7800000Z,"utcDate":2025-05-01T12:34:56.0000000Z,"wholeFloat":1,"zeroFloat":0}"#
        );
    }

    #[test]
    fn loads_encrypted_seb_plist_and_enables_moodle_toolbar() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
        <plist version="1.0"><dict>
            <key>startURL</key><string>https://lms.example.edu/mod/quiz/view.php?id=123</string>
            <key>showReloadButton</key><true/>
            <key>browserWindowAllowReload</key><true/>
            <key>enableBrowserWindowToolbar</key><false/>
            <key>allowQuit</key><true/>
            <key>enableScreenProctoring</key><false/>
        </dict></plist>"#;
        let inner_gz = gzip_compress(xml).unwrap();
        let enc = rncryptor_encrypt(&inner_gz, "secret").unwrap();
        let mut container = b"pswd".to_vec();
        container.extend_from_slice(&enc);
        let outer_gz = gzip_compress(&container).unwrap();

        let no_pw = loader::load_from_bytes(&outer_gz, None, false);
        assert_eq!(no_pw.status, loader::LoadStatus::PasswordNeeded);

        let ok = loader::load_from_bytes(&outer_gz, Some("secret"), false);
        assert_eq!(ok.status, loader::LoadStatus::Success);
        assert_eq!(
            ok.settings.browser.start_url,
            "https://lms.example.edu/mod/quiz/view.php?id=123"
        );
        // Moodle toolbar parity (AGENTS.md)
        assert!(ok.settings.browser.main_window.show_toolbar);
        assert!(!ok.settings.browser.main_window.allow_backward_navigation);
        assert!(!ok.settings.browser.main_window.allow_forward_navigation);
        assert!(ok.settings.browser.additional_window.allow_backward_navigation);
        assert!(ok.settings.browser.additional_window.allow_forward_navigation);
        assert!(!ok.settings.user_interface.taskbar.show_application_info);
        assert!(!ok.settings.user_interface.taskbar.show_audio);
        assert!(!ok.settings.user_interface.taskbar.show_network);
        assert!(ok.settings.mouse.allow_right_button);
        assert!(!ok.settings.proctoring.enabled);
        assert_eq!(ok.settings.browser.configuration_key.len(), 64);
    }

    #[test]
    fn browser_resource_profile_carries_start_url_and_double_query() {
        let mut cfg = settings::SessionConfiguration::default_session();
        let login_url = "https://lms.example.edu/mod/quiz/accessrule/seb/config.php?cmid=42??attempt=9";
        loader::apply_browser_resource_profile(&mut cfg, login_url);
        assert!(cfg.is_browser_resource);
        assert_eq!(cfg.settings.browser.start_url, login_url);
        assert_eq!(cfg.settings.browser.start_url_query, "?attempt=9");
        assert!(cfg.settings.security.allow_reconfiguration);
        assert!(cfg.settings.service.ignore_service);
    }
}
