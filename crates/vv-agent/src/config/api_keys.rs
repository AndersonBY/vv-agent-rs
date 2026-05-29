use base64::{engine::general_purpose, Engine as _};

pub fn decode_api_key(raw_value: &str) -> String {
    let raw = raw_value.trim();
    if raw.is_empty() {
        return raw.to_string();
    }

    if let Some(direct) = extract_suffix_key(raw) {
        return direct;
    }

    if std::env::var("V_AGENT_ENABLE_BASE64_KEY_DECODE").as_deref() == Ok("1") {
        if let Some(decoded) = maybe_base64_decode(raw) {
            if let Some(from_decoded) = extract_suffix_key(&decoded) {
                return from_decoded;
            }
            if looks_like_api_key(&decoded) {
                return decoded;
            }
        }
    }

    raw.to_string()
}

fn extract_suffix_key(value: &str) -> Option<String> {
    let (_, suffix) = value.split_once(':')?;
    let suffix = suffix.trim();
    looks_like_api_key(suffix).then(|| suffix.to_string())
}

fn maybe_base64_decode(value: &str) -> Option<String> {
    let mut padded = value.to_string();
    let remainder = padded.len() % 4;
    if remainder != 0 {
        padded.extend(std::iter::repeat_n('=', 4 - remainder));
    }
    let decoded = general_purpose::STANDARD.decode(padded).ok()?;
    String::from_utf8(decoded).ok()
}

fn looks_like_api_key(value: &str) -> bool {
    !value.is_empty() && value.len() >= 10 && !value.chars().any(char::is_whitespace)
}
