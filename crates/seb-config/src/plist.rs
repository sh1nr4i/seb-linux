use crate::{gzip_compress, rncryptor_encrypt, SebValue};
use base64::Engine;
use roxmltree::{Document, Node, ParsingOptions};

fn strip_utf8_bom(data: &[u8]) -> &[u8] {
    data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data)
}

pub fn is_xml_plist(data: &[u8]) -> bool {
    let trimmed = std::str::from_utf8(strip_utf8_bom(data))
        .unwrap_or("")
        .trim_start();
    trimmed.starts_with("<?xml")
        || trimmed.starts_with("<!DOCTYPE plist")
        || trimmed.starts_with("<plist")
}

pub fn parse_xml_plist(data: &[u8]) -> Option<SebValue> {
    let text = std::str::from_utf8(strip_utf8_bom(data)).ok()?;
    let opts = ParsingOptions {
        allow_dtd: true,
        ..ParsingOptions::default()
    };
    let doc = Document::parse_with_options(text, opts).ok()?;
    let plist_node = doc.root_element();
    if plist_node.tag_name().name() != "plist" {
        return None;
    }
    let mut elems = plist_node.children().filter(|n| n.is_element());
    let root_val = elems.next()?;
    // Strict: root inside <plist> must be <dict> and no trailing sibling elements
    if root_val.tag_name().name() != "dict" || elems.next().is_some() {
        return None;
    }
    parse_node(root_val)
}

fn parse_node(node: Node<'_, '_>) -> Option<SebValue> {
    match node.tag_name().name() {
        "dict" => {
            let mut entries = Vec::new();
            let mut children = node.children().filter(|n| n.is_element());
            while let Some(key_node) = children.next() {
                if key_node.tag_name().name() != "key" {
                    return None;
                }
                let key = key_node.text().unwrap_or("").to_string();
                let val_node = children.next()?;
                let val = parse_node(val_node)?;
                entries.push((key, val));
            }
            Some(SebValue::Map(entries))
        }
        "array" => {
            let mut items = Vec::new();
            for child in node.children().filter(|n| n.is_element()) {
                items.push(parse_node(child)?);
            }
            Some(SebValue::List(items))
        }
        "string" => {
            if let Some(txt) = node.text() {
                Some(SebValue::String(txt.to_string()))
            } else {
                Some(SebValue::String(String::new()))
            }
        }
        "integer" => {
            let raw = node.text()?.trim();
            if let Ok(i) = raw.parse::<i64>() {
                Some(SebValue::Int(i))
            } else if let Ok(u) = raw.parse::<u64>() {
                Some(SebValue::Uint(u))
            } else {
                None
            }
        }
        "real" => {
            let raw = node.text()?.trim();
            Some(SebValue::Float(raw.parse::<f64>().ok()?))
        }
        "true" => {
            if node.text().map(|t| !t.trim().is_empty()).unwrap_or(false) {
                return None;
            }
            Some(SebValue::Bool(true))
        }
        "false" => {
            if node.text().map(|t| !t.trim().is_empty()).unwrap_or(false) {
                return None;
            }
            Some(SebValue::Bool(false))
        }
        "data" => {
            let cleaned: String = node
                .text()
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if !cleaned.is_empty()
                && base64::engine::general_purpose::STANDARD
                    .decode(&cleaned)
                    .is_err()
            {
                return None;
            }
            Some(SebValue::Bytes(cleaned))
        }
        "date" => {
            let dt = node.text().unwrap_or("").trim().to_string();
            Some(SebValue::DateTime(dt))
        }
        _ => None,
    }
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn write_value(out: &mut String, val: &SebValue) {
    match val {
        SebValue::Map(entries) => {
            out.push_str("<dict>");
            for (k, v) in entries {
                if k.starts_with("__seb_internal_") {
                    continue;
                }
                out.push_str("<key>");
                out.push_str(&escape_xml(k));
                out.push_str("</key>");
                write_value(out, v);
            }
            out.push_str("</dict>");
        }
        SebValue::List(items) => {
            out.push_str("<array>");
            for item in items {
                write_value(out, item);
            }
            out.push_str("</array>");
        }
        SebValue::String(s) => {
            out.push_str("<string>");
            out.push_str(&escape_xml(s));
            out.push_str("</string>");
        }
        SebValue::Int(i) => {
            out.push_str("<integer>");
            out.push_str(&i.to_string());
            out.push_str("</integer>");
        }
        SebValue::Uint(u) => {
            out.push_str("<integer>");
            out.push_str(&u.to_string());
            out.push_str("</integer>");
        }
        SebValue::Float(f) => {
            out.push_str("<real>");
            out.push_str(&format!("{f:.17}")
                .trim_end_matches('0')
                .trim_end_matches('.'));
            out.push_str("</real>");
        }
        SebValue::Bool(true) => out.push_str("<true/>"),
        SebValue::Bool(false) => out.push_str("<false/>"),
        SebValue::Bytes(b64) => {
            out.push_str("<data>");
            out.push_str(b64);
            out.push_str("</data>");
        }
        SebValue::DateTime(dt) => {
            out.push_str("<date>");
            out.push_str(&escape_xml(dt));
            out.push_str("</date>");
        }
        SebValue::Empty => out.push_str("<string></string>"),
    }
}

pub fn serialize_xml_plist(root: &SebValue) -> Vec<u8> {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">",
    );
    write_value(&mut out, root);
    out.push_str("</plist>\n");
    out.into_bytes()
}

/// 100% parity with C++ `BinarySerializer::serializePlain`:
/// XML -> gzip -> prepend `plnd` -> outer gzip.
pub fn serialize_binary_plain(root: &SebValue) -> Option<Vec<u8>> {
    let xml = serialize_xml_plist(root);
    let inner_gz = gzip_compress(&xml)?;
    let mut buf = Vec::with_capacity(4 + inner_gz.len());
    buf.extend_from_slice(b"plnd");
    buf.extend_from_slice(&inner_gz);
    gzip_compress(&buf)
}

/// 100% parity with C++ `BinarySerializer::serializePassword`:
/// XML -> gzip -> RNCryptor v2 encrypt -> prepend `pwcc` (if is_hash) or `pswd` -> outer gzip.
pub fn serialize_binary_password(
    root: &SebValue,
    password: &str,
    is_hash: bool,
) -> Option<Vec<u8>> {
    let xml = serialize_xml_plist(root);
    let inner_gz = gzip_compress(&xml)?;
    let encrypted = rncryptor_encrypt(&inner_gz, password)?;
    let prefix = if is_hash { b"pwcc" } else { b"pswd" };
    let mut buf = Vec::with_capacity(4 + encrypted.len());
    buf.extend_from_slice(prefix);
    buf.extend_from_slice(&encrypted);
    gzip_compress(&buf)
}
