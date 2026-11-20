//! Gemini CLI quota through the CLI's own Google OAuth login (~/.gemini/oauth_creds.json).
//! Reference: CodexBar Providers/Gemini/GeminiStatusProbe.swift.

use super::Provider;
use crate::model::{Usage, Window};
use crate::util::{agent, home, http_error, jwt_claims, parse_time, read_json, write_json_private};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct Gemini;

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const LOAD_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const QUOTA_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";

fn gemini_home() -> PathBuf {
    match std::env::var("GEMINI_CLI_HOME") {
        Ok(path) if !path.trim().is_empty() => PathBuf::from(path.trim()),
        _ => home().join(".gemini"),
    }
}

fn creds_path() -> PathBuf {
    gemini_home().join("oauth_creds.json")
}

impl Provider for Gemini {
    fn id(&self) -> &'static str {
        "gemini"
    }
    fn name(&self) -> &'static str {
        "Gemini"
    }
    fn source(&self) -> &'static str {
        "oauth"
    }
    fn signed_in(&self) -> bool {
        creds_path().is_file()
    }

    fn fetch(&self) -> Result<Usage, String> {
        check_auth_type()?;
        let path = creds_path();
        let mut creds = read_json(&path)
            .map_err(|_| "Not logged in to Gemini. Run `gemini` to sign in.".to_string())?;
        let expired = creds["expiry_date"]
            .as_f64()
            .is_some_and(|ms| ms / 1000.0 < chrono::Utc::now().timestamp() as f64);
        let missing = creds["access_token"].as_str().is_none_or(str::is_empty);
        if expired || missing {
            refresh(&path, &mut creds)?;
        }
        let token = creds["access_token"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let claims = creds["id_token"]
            .as_str()
            .and_then(jwt_claims)
            .unwrap_or(Value::Null);
        let http = agent();

        // Plan and project are best effort; quota still works without them.
        let status: Value = http
            .post(LOAD_URL)
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(json!({"metadata": {"ideType": "GEMINI_CLI", "pluginType": "GEMINI"}}))
            .ok()
            .and_then(|r| r.into_json().ok())
            .unwrap_or(Value::Null);
        let project = project_id(&status["cloudaicompanionProject"]);
        let body = match &project {
            Some(id) => json!({"project": id}),
            None => json!({}),
        };
        let quota: Value = http
            .post(QUOTA_URL)
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(body)
            .map_err(|e| match crate::util::status_of(&e) {
                Some(401) => "Gemini login expired. Run `gemini` to sign in again.".to_string(),
                _ => format!("Gemini quota request failed: {}", http_error(e)),
            })?
            .into_json()
            .map_err(|e| format!("Gemini quota response unreadable: {e}"))?;

        let windows = map_buckets(&quota)?;
        Ok(Usage {
            provider: "gemini",
            name: "Gemini",
            source: "oauth",
            plan: plan(&status, claims["hd"].as_str().is_some()),
            account: claims["email"].as_str().map(String::from),
            windows,
            extra: Vec::new(),
            credits: None,
        })
    }
}

fn check_auth_type() -> Result<(), String> {
    let Ok(settings) = read_json(&gemini_home().join("settings.json")) else {
        return Ok(());
    };
    match settings["security"]["auth"]["selectedType"].as_str() {
        Some("api-key" | "gemini-api-key") => {
            Err("Gemini CLI uses an API key; quota needs a Google login.".into())
        }
        Some("vertex-ai") => Err("Gemini CLI uses Vertex AI; quota needs a Google login.".into()),
        _ => Ok(()),
    }
}

fn refresh(path: &Path, creds: &mut Value) -> Result<(), String> {
    let refresh_token = creds["refresh_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or("Not logged in to Gemini. Run `gemini` to sign in.")?
        .to_string();
    let (client_id, client_secret) = oauth_client().ok_or(
        "Gemini token expired and the Gemini CLI OAuth client was not found. Run `gemini` once, or set GEMINI_OAUTH_CLIENT_ID and GEMINI_OAUTH_CLIENT_SECRET.",
    )?;
    let response: Value = agent()
        .post(TOKEN_URL)
        .send_form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .map_err(|_| "Gemini login expired. Run `gemini` to sign in again.".to_string())?
        .into_json()
        .map_err(|e| format!("Gemini token response unreadable: {e}"))?;
    let access = response["access_token"]
        .as_str()
        .ok_or("Gemini token response has no access token")?;
    creds["access_token"] = json!(access);
    if let Some(seconds) = response["expires_in"].as_f64() {
        creds["expiry_date"] =
            json!(((chrono::Utc::now().timestamp() as f64 + seconds) * 1000.0) as i64);
    }
    if let Some(id_token) = response["id_token"].as_str() {
        creds["id_token"] = json!(id_token);
    }
    write_json_private(path, creds)
}

/// The Gemini CLI ships its OAuth client in oauth2.js; read it from the installed CLI.
fn oauth_client() -> Option<(String, String)> {
    let id = std::env::var("GEMINI_OAUTH_CLIENT_ID").unwrap_or_default();
    let secret = std::env::var("GEMINI_OAUTH_CLIENT_SECRET").unwrap_or_default();
    if !id.trim().is_empty() && !secret.trim().is_empty() {
        return Some((id.trim().into(), secret.trim().into()));
    }
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("GEMINI_OAUTH2_JS_PATH") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(root) = installed_package_root() {
        candidates.push(root.join("dist/src/code_assist/oauth2.js"));
        candidates
            .push(root.join("node_modules/@google/gemini-cli-core/dist/src/code_assist/oauth2.js"));
        if let Ok(entries) = std::fs::read_dir(root.join("bundle")) {
            candidates.extend(
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "js")),
            );
        }
    }
    candidates
        .iter()
        .find_map(|path| client_from_js(&std::fs::read_to_string(path).ok()?))
}

fn installed_package_root() -> Option<PathBuf> {
    let binary = std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|dir| Path::new(dir).join("gemini"))
        .find(|p| p.is_file())?;
    let resolved = std::fs::canonicalize(binary).ok()?;
    let mut dir = resolved.parent()?.to_path_buf();
    for _ in 0..8 {
        for candidate in [
            dir.clone(),
            dir.join("lib/node_modules/@google/gemini-cli"),
            dir.join("libexec/lib/node_modules/@google/gemini-cli"),
        ] {
            if read_json(&candidate.join("package.json"))
                .is_ok_and(|p| p["name"] == "@google/gemini-cli")
            {
                return Some(candidate);
            }
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

fn client_from_js(source: &str) -> Option<(String, String)> {
    let find = |name: &str| -> Option<String> {
        let start = source.find(&format!("{name} = "))? + name.len() + 3;
        let rest = &source[start..];
        let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
        let value: String = rest[1..].chars().take_while(|c| *c != quote).collect();
        (!value.is_empty()).then_some(value)
    };
    Some((find("OAUTH_CLIENT_ID")?, find("OAUTH_CLIENT_SECRET")?))
}

fn project_id(value: &Value) -> Option<String> {
    let raw = match value {
        Value::String(s) => Some(s.as_str()),
        Value::Object(_) => value["id"].as_str().or(value["projectId"].as_str()),
        _ => None,
    }?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn plan(status: &Value, workspace: bool) -> Option<String> {
    if let Some(name) = status["paidTier"]["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(name.into());
    }
    match status["currentTier"]["id"].as_str()? {
        "standard-tier" => Some("Paid".into()),
        "free-tier" if workspace => Some("Workspace".into()),
        "free-tier" => Some("Free".into()),
        "legacy-tier" => Some("Legacy".into()),
        _ => None,
    }
}

/// Daily per-model buckets, grouped into Pro / Flash / Flash Lite by the tightest model.
fn map_buckets(quota: &Value) -> Result<Vec<Window>, String> {
    let buckets = quota["buckets"]
        .as_array()
        .filter(|b| !b.is_empty())
        .ok_or("No Gemini quota buckets in response")?;
    let mut per_model: BTreeMap<String, (f64, Option<chrono::DateTime<chrono::Utc>>)> =
        BTreeMap::new();
    for bucket in buckets {
        let (Some(model), Some(fraction)) = (
            bucket["modelId"].as_str(),
            bucket["remainingFraction"].as_f64(),
        ) else {
            continue;
        };
        let reset = parse_time(&bucket["resetTime"]);
        let entry = per_model
            .entry(model.to_lowercase())
            .or_insert((fraction, reset));
        if fraction < entry.0 {
            *entry = (fraction, reset);
        }
    }
    type Tier = (&'static str, fn(&str) -> bool);
    let tiers: [Tier; 3] = [
        ("Pro", |id| id.contains("pro")),
        ("Flash", |id| {
            id.contains("flash") && !id.contains("flash-lite")
        }),
        ("Flash Lite", |id| id.contains("flash-lite")),
    ];
    let windows: Vec<Window> = tiers
        .iter()
        .filter_map(|(label, matches)| {
            let (fraction, reset) = per_model
                .iter()
                .filter(|(id, _)| matches(id))
                .map(|(_, v)| *v)
                .min_by(|a, b| a.0.total_cmp(&b.0))?;
            Some(Window {
                label: format!("{label} (daily)"),
                used_percent: 100.0 - fraction * 100.0,
                window_minutes: Some(1440),
                resets_at: reset,
            })
        })
        .collect();
    if windows.is_empty() {
        Err("No Gemini quota buckets in response".into())
    } else {
        Ok(windows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_buckets_by_tier_using_tightest_model() {
        let quota = json!({"buckets": [
            {"modelId": "gemini-2.5-pro", "remainingFraction": 0.8, "resetTime": "2030-01-01T00:00:00Z"},
            {"modelId": "gemini-3-pro", "remainingFraction": 0.25, "resetTime": "2030-01-02T00:00:00Z"},
            {"modelId": "gemini-2.5-flash", "remainingFraction": 0.5},
            {"modelId": "gemini-2.5-flash-lite", "remainingFraction": 1.0},
            {"remainingFraction": 0.0}
        ]});
        let windows = map_buckets(&quota).unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label, "Pro (daily)");
        assert_eq!(windows[0].used_percent, 75.0);
        assert_eq!(windows[1].used_percent, 50.0);
        assert_eq!(windows[2].used_percent, 0.0);
    }

    #[test]
    fn reads_oauth_client_from_bundle() {
        let js = "const OAUTH_CLIENT_ID = 'abc-123.apps.googleusercontent.com';\nconst OAUTH_CLIENT_SECRET = 'GOCSPX-xyz';";
        assert_eq!(
            client_from_js(js),
            Some((
                "abc-123.apps.googleusercontent.com".into(),
                "GOCSPX-xyz".into()
            ))
        );
    }

    #[test]
    fn plan_names() {
        assert_eq!(
            plan(&json!({"paidTier": {"name": "Gemini Pro"}}), false).as_deref(),
            Some("Gemini Pro")
        );
        assert_eq!(
            plan(&json!({"currentTier": {"id": "free-tier"}}), true).as_deref(),
            Some("Workspace")
        );
        assert_eq!(plan(&json!({}), false), None);
    }
}
