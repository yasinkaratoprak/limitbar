//! Claude usage through Claude Code's OAuth login: `~/.claude/.credentials.json`, or the
//! login Keychain on macOS. The token belongs to Claude Code: limitbar never refreshes it.
//! Reference: CodexBar Providers/Claude/ClaudeOAuth/ClaudeOAuthUsageFetcher.swift.

use super::Provider;
use crate::model::{Usage, Window};
use crate::util::{agent, home, http_error, parse_time, read_json, status_of};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

pub struct Claude;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// Keychain item Claude Code uses on macOS when no custom config dir is set.
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

#[cfg(target_os = "macos")]
fn custom_config_dir() -> bool {
    ["CLAUDE_SECURESTORAGE_CONFIG_DIR", "CLAUDE_CONFIG_DIR"]
        .iter()
        .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
}

fn credentials_path() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_SECURESTORAGE_CONFIG_DIR") {
        return if dir.trim().is_empty() {
            home().join(".claude")
        } else {
            PathBuf::from(dir.trim())
        }
        .join(".credentials.json");
    }
    match std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir.trim()).join(".credentials.json"),
        _ => home().join(".claude/.credentials.json"),
    }
}

/// Keep only the `claudeAiOauth` object, and only when it carries an access token.
/// Anything else stored next to it (e.g. MCP server tokens) is dropped right here.
fn oauth_of(mut credentials: Value) -> Option<Value> {
    let oauth = credentials.get_mut("claudeAiOauth")?.take();
    oauth["accessToken"]
        .as_str()
        .is_some_and(|t| !t.trim().is_empty())
        .then_some(oauth)
}

/// Claude Code's sign-in: the credentials file first, then (macOS) the login Keychain.
fn load_oauth() -> Option<Value> {
    read_json(&credentials_path())
        .ok()
        .and_then(oauth_of)
        .or_else(|| keychain_credentials().and_then(oauth_of))
}

#[cfg(not(target_os = "macos"))]
fn keychain_credentials() -> Option<Value> {
    None
}

/// Read Claude Code's Keychain item. The secret goes straight from the child's stdout into
/// memory; it is never printed, logged or written to disk.
#[cfg(target_os = "macos")]
fn keychain_credentials() -> Option<Value> {
    // A custom config dir uses a differently named item; don't guess and pick the wrong account.
    if custom_config_dir() {
        return None;
    }
    let output = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    serde_json::from_slice(&output.stdout).ok()
}

impl Provider for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn name(&self) -> &'static str {
        "Claude"
    }
    fn source(&self) -> &'static str {
        "oauth"
    }
    fn signed_in(&self) -> bool {
        load_oauth().is_some()
    }

    fn fetch(&self) -> Result<Usage, String> {
        let oauth = load_oauth().ok_or("Not logged in to Claude. Run `claude` and sign in.")?;
        let token = oauth["accessToken"].as_str().unwrap_or_default();
        let expired = oauth["expiresAt"]
            .as_f64()
            .is_none_or(|ms| ms / 1000.0 <= chrono::Utc::now().timestamp() as f64);
        if expired {
            return Err(
                "Claude login has expired. Run `claude` once so it can refresh, then try again."
                    .into(),
            );
        }
        let scopes: Vec<&str> = oauth["scopes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if !scopes.is_empty() && !scopes.contains(&"user:profile") {
            return Err(
                "Claude login lacks the user:profile scope. Run `claude` and sign in again.".into(),
            );
        }

        let version = claude_version();
        let http = agent();
        #[allow(clippy::result_large_err)]
        let request = |url: &str, user_agent: String| {
            http.get(url)
                .set("Authorization", &format!("Bearer {token}"))
                .set("Accept", "application/json")
                .set("Content-Type", "application/json")
                .set("anthropic-beta", "oauth-2025-04-20")
                .set("User-Agent", &user_agent)
                .call()
        };
        let response = match request(
            &format!("{USAGE_URL}?cedar_ember=1"),
            format!("claude-cli/{version} (external, cli)"),
        ) {
            // Older API versions reject the query or the user agent; retry once the plain way.
            Err(ureq::Error::Status(400, _)) => {
                request(USAGE_URL, format!("claude-code/{version}"))
            }
            Err(ureq::Error::Status(403, body)) => {
                let text = body.into_string().unwrap_or_default();
                if text.contains("user:profile") {
                    return Err("Claude login lacks the user:profile scope. Run `claude` and sign in again.".into());
                }
                request(USAGE_URL, format!("claude-code/{version}"))
            }
            other => other,
        };
        let usage: Value = response
            .map_err(|e| match status_of(&e) {
                Some(401) => "Claude login was rejected. Run `claude` once so it can refresh, then try again.".to_string(),
                Some(429) => "Claude usage API is rate limited; try again in a few minutes.".to_string(),
                _ => format!("Claude usage request failed: {}", http_error(e)),
            })?
            .into_json()
            .map_err(|e| format!("Claude usage response unreadable: {e}"))?;

        let mut result = map_usage(&usage)?;
        result.plan = oauth["subscriptionType"]
            .as_str()
            .or(oauth["rateLimitTier"].as_str())
            .map(plan_name);
        // Email is best effort.
        result.account = http
            .get(PROFILE_URL)
            .timeout(Duration::from_secs(15))
            .set("Authorization", &format!("Bearer {token}"))
            .set("Accept", "application/json")
            .call()
            .ok()
            .and_then(|r| r.into_json::<Value>().ok())
            .and_then(|p| profile_email(&p));
        Ok(result)
    }
}

/// Claude's API wants a realistic CLI version in the user agent.
fn claude_version() -> String {
    Command::new("claude")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().next().map(String::from))
        .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or_else(|| "2.1.0".into())
}

fn plan_name(raw: &str) -> String {
    match raw {
        "pro" => "Claude Pro".into(),
        "max" => "Claude Max".into(),
        "team" => "Claude Team".into(),
        "enterprise" => "Claude Enterprise".into(),
        other => other.into(),
    }
}

fn profile_email(profile: &Value) -> Option<String> {
    let keys = ["emailAddress", "email_address", "email"];
    keys.iter()
        .find_map(|k| profile["account"][k].as_str())
        .or_else(|| keys.iter().find_map(|k| profile[k].as_str()))
        .map(String::from)
}

fn window(value: &Value, label: &str, minutes: u32) -> Option<Window> {
    Some(Window {
        label: label.into(),
        used_percent: value["utilization"].as_f64()?,
        window_minutes: Some(minutes),
        resets_at: parse_time(&value["resets_at"]),
    })
}

fn map_usage(usage: &Value) -> Result<Usage, String> {
    let weekly = if usage["seven_day"]["utilization"].is_number() {
        &usage["seven_day"]
    } else {
        &usage["seven_day_oauth_apps"]
    };
    let windows: Vec<Window> = [
        window(&usage["five_hour"], "Session", 300),
        window(weekly, "Weekly", 10080),
    ]
    .into_iter()
    .flatten()
    .collect();

    let mut extra: Vec<Window> = [
        ("seven_day_sonnet", "Sonnet weekly"),
        ("seven_day_opus", "Opus weekly"),
    ]
    .iter()
    .filter_map(|(key, label)| window(&usage[*key], label, 10080))
    .collect();
    for limit in usage["limits"].as_array().into_iter().flatten() {
        if limit["group"] != "weekly" || limit["kind"] != "weekly_scoped" {
            continue;
        }
        let (Some(percent), Some(model)) = (
            limit["percent"].as_f64(),
            limit["scope"]["model"]["display_name"].as_str(),
        ) else {
            continue;
        };
        let label = format!("{} weekly", model.trim());
        if model.trim().is_empty() || extra.iter().any(|w| w.label.eq_ignore_ascii_case(&label)) {
            continue;
        }
        extra.push(Window {
            label,
            used_percent: percent,
            window_minutes: Some(10080),
            resets_at: parse_time(&limit["resets_at"]),
        });
    }

    if windows.is_empty() && extra.is_empty() {
        return Err("Claude usage response has no usage windows".into());
    }
    Ok(Usage {
        provider: "claude",
        name: "Claude",
        source: "oauth",
        plan: None,
        account: None,
        windows,
        extra,
        credits: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_session_weekly_and_model_windows() {
        let usage = json!({
            "five_hour": {"utilization": 32, "resets_at": "2026-11-20T11:39:59.123+00:00"},
            "seven_day": {"utilization": 24, "resets_at": "2026-11-25T22:59:59Z"},
            "seven_day_opus": {"utilization": null},
            "seven_day_sonnet": {"utilization": 5},
            "limits": [
                {"group": "weekly", "kind": "weekly_scoped", "percent": 12, "scope": {"model": {"display_name": "Fable"}}},
                {"group": "weekly", "kind": "weekly_scoped", "percent": 5, "scope": {"model": {"display_name": "Sonnet"}}}
            ]
        });
        let mapped = map_usage(&usage).unwrap();
        assert_eq!(mapped.windows.len(), 2);
        assert_eq!(mapped.windows[0].used_percent, 32.0);
        assert!(mapped.windows[0].resets_at.is_some());
        let labels: Vec<_> = mapped.extra.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Sonnet weekly", "Fable weekly"]);
    }

    #[test]
    fn empty_payload_is_an_error_not_zero_usage() {
        assert!(map_usage(&json!({"five_hour": null})).is_err());
    }

    #[test]
    fn keeps_only_the_claude_oauth_object() {
        let stored = json!({
            "claudeAiOauth": {"accessToken": "t", "scopes": ["user:profile"]},
            "mcpOAuth": {"some-server": {"accessToken": "other"}}
        });
        assert_eq!(
            oauth_of(stored),
            Some(json!({"accessToken": "t", "scopes": ["user:profile"]}))
        );
        assert_eq!(
            oauth_of(json!({"claudeAiOauth": {"accessToken": " "}})),
            None
        );
        assert_eq!(oauth_of(json!({"mcpOAuth": {}})), None);
    }

    #[test]
    fn email_from_profile() {
        assert_eq!(
            profile_email(&json!({"account": {"email_address": "a@b.c"}})).as_deref(),
            Some("a@b.c")
        );
    }
}
