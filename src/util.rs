use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("limitbar/", env!("CARGO_PKG_VERSION")))
        .build()
}

pub fn read_json(path: &Path) -> Result<Value, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid JSON in {}: {e}", path.display()))
}

/// Replace a credential file atomically, keeping it private to the user.
pub fn write_json_private(path: &Path, value: &Value) -> Result<(), String> {
    let mode = fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o600)
        & 0o600;
    let temporary = path.with_extension(format!("limitbar-{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(if mode == 0 { 0o600 } else { mode })
            .open(&temporary)?;
        file.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|e| format!("cannot update {}: {e}", path.display()))
}

/// Turn an HTTP failure into a short message without echoing the response body.
pub fn http_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(transport) => format!("network error: {transport}"),
    }
}

pub fn status_of(error: &ureq::Error) -> Option<u16> {
    match error {
        ureq::Error::Status(code, _) => Some(*code),
        _ => None,
    }
}

/// Accept RFC 3339 strings, Unix seconds or Unix milliseconds.
pub fn parse_time(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(text) => DateTime::parse_from_rfc3339(text)
            .map(|t| t.with_timezone(&Utc))
            .ok()
            .or_else(|| text.parse::<f64>().ok().and_then(epoch)),
        Value::Number(number) => number.as_f64().and_then(epoch),
        _ => None,
    }
}

fn epoch(value: f64) -> Option<DateTime<Utc>> {
    let seconds = if value > 1e12 { value / 1000.0 } else { value };
    Utc.timestamp_opt(seconds as i64, 0).single()
}

/// Read the email claim from a JWT without verifying it (display only).
pub fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64_url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

fn base64_url_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_times() {
        let expected = Utc.with_ymd_and_hms(2026, 11, 20, 11, 39, 59).unwrap();
        assert_eq!(parse_time(&json!("2026-11-20T11:39:59Z")), Some(expected));
        assert_eq!(parse_time(&json!(expected.timestamp())), Some(expected));
        assert_eq!(
            parse_time(&json!(expected.timestamp() * 1000)),
            Some(expected)
        );
        assert_eq!(parse_time(&json!(null)), None);
    }

    #[test]
    fn decodes_jwt_claims() {
        // {"email":"a@b.c"}
        let token = "x.eyJlbWFpbCI6ImFAYi5jIn0.y";
        assert_eq!(jwt_claims(token).unwrap()["email"], "a@b.c");
    }
}
