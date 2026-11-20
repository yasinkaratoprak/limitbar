//! Codex (ChatGPT) usage through the Codex CLI's OAuth login (~/.codex/auth.json).
//! The token belongs to the Codex CLI: limitbar never refreshes it, because a refresh
//! rotates the refresh token and would sign the CLI out.
//! Reference: CodexBar Providers/Codex/CodexOAuth/*.swift.

use super::Provider;
use crate::model::{Usage, Window, window_label};
use crate::util::{agent, home, http_error, jwt_claims, parse_time, read_json, status_of};
use serde_json::Value;
use std::path::PathBuf;

pub struct Codex;

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

fn auth_path() -> PathBuf {
    match std::env::var("CODEX_HOME") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir.trim()).join("auth.json"),
        _ => home().join(".codex/auth.json"),
    }
}

impl Provider for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn name(&self) -> &'static str {
        "Codex"
    }
    fn source(&self) -> &'static str {
        "oauth"
    }
    fn signed_in(&self) -> bool {
        read_json(&auth_path())
            .is_ok_and(|auth| token(&auth["tokens"], "access_token", "accessToken").is_some())
    }

    fn fetch(&self) -> Result<Usage, String> {
        let auth = read_json(&auth_path())
            .map_err(|_| "Not logged in to Codex. Run `codex login`.".to_string())?;
        let tokens = &auth["tokens"];
        let access = token(tokens, "access_token", "accessToken")
            .ok_or("Not logged in to Codex. Run `codex login`.")?;
        let id_claims = token(tokens, "id_token", "idToken")
            .and_then(jwt_claims)
            .unwrap_or(Value::Null);
        let access_claims = jwt_claims(access).unwrap_or(Value::Null);
        if access_claims["exp"]
            .as_i64()
            .is_some_and(|exp| exp <= chrono::Utc::now().timestamp())
        {
            return Err(
                "Codex login has expired. Run `codex` once so it can refresh, then try again."
                    .into(),
            );
        }
        let account = token(tokens, "account_id", "accountId")
            .map(String::from)
            .or_else(|| account_id(&id_claims))
            .or_else(|| account_id(&access_claims));

        let mut request = agent()
            .get(USAGE_URL)
            .set("Authorization", &format!("Bearer {access}"))
            .set("Accept", "application/json");
        if let Some(account) = &account {
            request = request.set("ChatGPT-Account-Id", account);
        }
        let usage: Value = request
            .call()
            .map_err(|e| match status_of(&e) {
                Some(401) => {
                    "Codex login was rejected. Run `codex` once so it can refresh, then try again."
                        .to_string()
                }
                _ => format!("Codex usage request failed: {}", http_error(e)),
            })?
            .into_json()
            .map_err(|e| format!("Codex usage response unreadable: {e}"))?;

        let mut result = map_usage(&usage)?;
        result.account = id_claims["email"]
            .as_str()
            .or(id_claims["https://api.openai.com/profile"]["email"].as_str())
            .map(String::from);
        if result.plan.is_none() {
            result.plan = id_claims["https://api.openai.com/auth"]["chatgpt_plan_type"]
                .as_str()
                .or(id_claims["chatgpt_plan_type"].as_str())
                .map(plan_name);
        }
        Ok(result)
    }
}

fn token<'a>(tokens: &'a Value, snake: &str, camel: &str) -> Option<&'a str> {
    tokens[snake]
        .as_str()
        .filter(|t| !t.is_empty())
        .or_else(|| tokens[camel].as_str().filter(|t| !t.is_empty()))
}

fn account_id(claims: &Value) -> Option<String> {
    claims["chatgpt_account_id"]
        .as_str()
        .or(claims["https://api.openai.com/auth"]["chatgpt_account_id"].as_str())
        .or_else(|| {
            claims["organizations"]
                .as_array()?
                .iter()
                .find_map(|o| o["id"].as_str().filter(|s| !s.is_empty()))
        })
        .map(String::from)
}

fn plan_name(raw: &str) -> String {
    let mut chars = raw.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn window(value: &Value, fallback: &str) -> Option<Window> {
    let used = value["used_percent"].as_f64()?;
    let minutes = value["limit_window_seconds"]
        .as_f64()
        .map(|s| (s / 60.0).round() as u32);
    Some(Window {
        label: window_label(minutes, fallback),
        used_percent: used,
        window_minutes: minutes,
        resets_at: parse_time(&value["reset_at"]),
    })
}

fn map_usage(usage: &Value) -> Result<Usage, String> {
    let limits = &usage["rate_limit"];
    let mut windows: Vec<Window> = [
        window(&limits["primary_window"], "Session"),
        window(&limits["secondary_window"], "Weekly"),
    ]
    .into_iter()
    .flatten()
    .collect();
    // Classify by duration: the API can list the weekly window first.
    windows.sort_by_key(|w| w.window_minutes.unwrap_or(u32::MAX));

    let extra = usage["additional_rate_limits"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|item| {
            let name = item["limit_name"]
                .as_str()
                .or(item["metered_feature"].as_str())
                .unwrap_or("Additional");
            [
                &item["rate_limit"]["primary_window"],
                &item["rate_limit"]["secondary_window"],
            ]
            .into_iter()
            .filter_map(move |w| {
                let mut parsed = window(w, "")?;
                parsed.label = if parsed.label.is_empty() {
                    name.to_string()
                } else {
                    format!("{name} {}", parsed.label.to_lowercase())
                };
                Some(parsed)
            })
        })
        .collect();

    if windows.is_empty() {
        return Err("Codex usage response has no rate-limit windows".into());
    }
    let credits = &usage["credits"];
    let balance = credits["balance"]
        .as_f64()
        .or_else(|| credits["balance"].as_str()?.parse().ok());
    Ok(Usage {
        provider: "codex",
        name: "Codex",
        source: "oauth",
        plan: usage["plan_type"].as_str().map(plan_name),
        account: None,
        windows,
        extra,
        credits: if credits["unlimited"] == true {
            None
        } else {
            balance
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_windows_by_duration() {
        let usage = json!({
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {"used_percent": 19, "limit_window_seconds": 604800, "reset_at": 1795000000},
                "secondary_window": {"used_percent": 30, "limit_window_seconds": 18000, "reset_at": 1794000000}
            },
            "credits": {"has_credits": false, "unlimited": false, "balance": "0"}
        });
        let mapped = map_usage(&usage).unwrap();
        assert_eq!(mapped.plan.as_deref(), Some("Plus"));
        assert_eq!(mapped.windows[0].label, "Session");
        assert_eq!(mapped.windows[0].used_percent, 30.0);
        assert_eq!(mapped.windows[1].label, "Weekly");
        assert_eq!(mapped.credits, Some(0.0));
    }

    #[test]
    fn rejects_payload_without_windows() {
        assert!(map_usage(&json!({"rate_limit": {}})).is_err());
    }

    #[test]
    fn reads_account_from_claims() {
        let claims = json!({"https://api.openai.com/auth": {"chatgpt_account_id": "acct-1"}});
        assert_eq!(account_id(&claims).as_deref(), Some("acct-1"));
    }
}
