//! Antigravity quota from the local machine, no OAuth:
//! 1. a running Antigravity language server (desktop app / IDE) through its local API;
//! 2. otherwise the agy CLI's own report: `agy -p /usage --output-format json`.
//!
//! Reference: CodexBar `AntigravityStatusProbe*.swift`, `AntigravityQuotaSummaryParser.swift`
//! and `AntigravityProviderDescriptor.swift`.

use super::Provider;
use crate::model::{Usage, Window};
use crate::util::parse_time;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Antigravity;

const SERVICE: &str = "exa.language_server_pb.LanguageServerService";

impl Provider for Antigravity {
    fn id(&self) -> &'static str {
        "antigravity"
    }
    fn name(&self) -> &'static str {
        "Antigravity"
    }
    fn source(&self) -> &'static str {
        "local"
    }
    fn signed_in(&self) -> bool {
        agy_binary().is_some() || !language_servers().is_empty()
    }

    fn fetch(&self) -> Result<Usage, String> {
        let mut local_error = None;
        for server in language_servers() {
            match fetch_from_server(&server) {
                Ok(usage) => return Ok(usage),
                Err(error) => local_error = Some(error),
            }
        }
        match agy_binary() {
            Some(binary) => fetch_from_cli(&binary),
            None => Err(local_error
                .unwrap_or_else(|| "Antigravity is not running and `agy` was not found.".into())),
        }
    }
}

// ---------- local language server ----------

struct Server {
    pid: u32,
    csrf: String,
    extension_port: Option<u16>,
    extension_csrf: Option<String>,
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    args.iter().enumerate().find_map(|(i, arg)| {
        if arg == name {
            args.get(i + 1).cloned()
        } else {
            arg.strip_prefix(&prefix).map(String::from)
        }
    })
}

/// Desktop and IDE language servers are started with a CSRF token on the command line.
fn language_servers() -> Vec<Server> {
    let uid = fs::metadata("/proc/self")
        .map(|m| std::os::unix::fs::MetadataExt::uid(&m))
        .ok();
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let meta = entry.metadata().ok()?;
            if Some(std::os::unix::fs::MetadataExt::uid(&meta)) != uid {
                return None;
            }
            let raw = fs::read(entry.path().join("cmdline")).ok()?;
            let args: Vec<String> = raw
                .split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            let executable = Path::new(args.first()?)
                .file_name()?
                .to_str()?
                .to_lowercase();
            if !executable.starts_with("language_server")
                && !executable.starts_with("language-server")
            {
                return None;
            }
            let joined = args.join(" ").to_lowercase();
            if !joined.contains("antigravity") {
                return None;
            }
            let csrf = flag(&args, "--csrf_token").filter(|t| !t.is_empty())?;
            Some(Server {
                pid,
                csrf,
                extension_port: flag(&args, "--extension_server_port").and_then(|p| p.parse().ok()),
                extension_csrf: flag(&args, "--extension_server_csrf_token"),
            })
        })
        .collect()
}

/// TCP ports this process listens on: its socket inodes matched against its own net/tcp tables.
fn listening_ports(pid: u32) -> Vec<u16> {
    let inodes: HashSet<String> = fs::read_dir(format!("/proc/{pid}/fd"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|fd| {
            let target = fs::read_link(fd.path()).ok()?;
            let text = target.to_str()?;
            Some(
                text.strip_prefix("socket:[")?
                    .strip_suffix(']')?
                    .to_string(),
            )
        })
        .collect();
    let mut ports: Vec<u16> = ["tcp", "tcp6"]
        .iter()
        .filter_map(|table| fs::read_to_string(format!("/proc/{pid}/net/{table}")).ok())
        .flat_map(|text| {
            text.lines()
                .skip(1)
                .filter_map(|line| {
                    let columns: Vec<&str> = line.split_whitespace().collect();
                    if columns.get(3) != Some(&"0A") || !inodes.contains(*columns.get(9)?) {
                        return None;
                    }
                    u16::from_str_radix(columns.get(1)?.rsplit(':').next()?, 16).ok()
                })
                .collect::<Vec<_>>()
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn fetch_from_server(server: &Server) -> Result<Usage, String> {
    // The language server's own CSRF token only travels over TLS. The extension server
    // speaks plain HTTP on loopback, so it only ever receives its own token.
    let mut endpoints: Vec<(&str, u16, String)> = listening_ports(server.pid)
        .into_iter()
        .filter(|port| Some(*port) != server.extension_port)
        .map(|port| ("https", port, server.csrf.clone()))
        .collect();
    if let (Some(port), Some(token)) = (
        server.extension_port,
        server.extension_csrf.as_ref().filter(|t| !t.is_empty()),
    ) {
        endpoints.push(("http", port, token.clone()));
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut last = "Antigravity language server has no reachable port".to_string();
    for (scheme, port, token) in endpoints {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let agent = crate::loopback_tls::agent(remaining.min(Duration::from_secs(3)));
        let call = |method: &str, body: Value| -> Result<Value, String> {
            let response = agent
                .post(&format!("{scheme}://127.0.0.1:{port}/{SERVICE}/{method}"))
                .set("Content-Type", "application/json")
                .set("Connect-Protocol-Version", "1")
                .set("X-Codeium-Csrf-Token", &token)
                .send_json(body)
                .map_err(crate::util::http_error)?;
            let value: Value = response.into_json().map_err(|e| e.to_string())?;
            match &value["code"] {
                Value::Null => Ok(value),
                Value::Number(n) if n.as_i64() == Some(0) => Ok(value),
                Value::String(s) if ["0", "ok", "success"].contains(&s.to_lowercase().as_str()) => {
                    Ok(value)
                }
                other => Err(format!("language server answered code {other}")),
            }
        };
        match call("RetrieveUserQuotaSummary", json!({"forceRefresh": true})) {
            Ok(summary) => {
                let payload = [&summary["response"], &summary["summary"]]
                    .into_iter()
                    .find(|v| !v.is_null())
                    .unwrap_or(&summary);
                match map_summary(payload) {
                    Ok(mut usage) => {
                        let status = call(
                            "GetUserStatus",
                            json!({"metadata": {"ideName": "antigravity", "extensionName": "antigravity", "ideVersion": "unknown", "locale": "en"}}),
                        )
                        .unwrap_or(Value::Null);
                        usage.account = status["userStatus"]["email"].as_str().map(String::from);
                        usage.plan = plan(&status["userStatus"]);
                        return Ok(usage);
                    }
                    Err(error) => last = error,
                }
            }
            Err(error) => last = format!("Antigravity local API: {error}"),
        }
    }
    Err(last)
}

fn plan(status: &Value) -> Option<String> {
    let tier = status["userTier"]["name"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    tier.or_else(|| {
        let info = &status["planStatus"]["planInfo"];
        [
            "planDisplayName",
            "displayName",
            "productName",
            "planName",
            "planShortName",
        ]
        .iter()
        .find_map(|k| info[k].as_str().map(str::trim).filter(|s| !s.is_empty()))
    })
    .map(String::from)
}

// ---------- agy CLI report ----------

fn agy_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ANTIGRAVITY_CLI_PATH") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    let from_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("agy"))
            .find(|p| p.is_file())
    });
    from_path.or_else(|| {
        [
            crate::util::home().join(".local/bin/agy"),
            PathBuf::from("/snap/bin/agy"),
            PathBuf::from("/usr/local/bin/agy"),
        ]
        .into_iter()
        .find(|p| p.is_file())
    })
}

fn fetch_from_cli(binary: &Path) -> Result<Usage, String> {
    // A private working directory, and failing browser openers so a logged-out agy cannot pop a window.
    // The directory goes on PATH, so it must be ours alone: fresh name, mode 0700, never reused.
    let work = private_dir()?;
    let stubs = work.join("bin");
    let prepared = fs::DirBuilder::new()
        .mode(0o700)
        .create(&stubs)
        .and_then(|()| {
            for name in ["xdg-open", "open", "sensible-browser"] {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o700)
                    .open(stubs.join(name))?;
                file.write_all(b"#!/bin/sh\nexit 1\n")?;
            }
            Ok(())
        });
    if let Err(error) = prepared {
        let _ = fs::remove_dir_all(&work);
        return Err(format!("cannot prepare {}: {error}", work.display()));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut joined = std::ffi::OsString::from(stubs.as_os_str());
    joined.push(":");
    joined.push(path);

    let result = (|| {
        let mut child = Command::new(binary)
            .args([
                "-p",
                "/usage",
                "--output-format",
                "json",
                "--print-timeout",
                "90s",
            ])
            .current_dir(&work)
            .env("PATH", joined)
            .env("BROWSER", stubs.join("xdg-open"))
            .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("cannot run agy: {e}"))?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let reader = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = stdout.by_ref().take(1 << 20).read_to_end(&mut buffer);
            buffer
        });
        let deadline = Instant::now() + Duration::from_secs(95);
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break Some(status);
            }
            if Instant::now() > deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        // Clean up anything agy left behind in its process group.
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", child.id())])
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
        let _ = child.wait();
        let output = reader.join().unwrap_or_default();
        if status.is_none() {
            return Err("agy /usage timed out".to_string());
        }
        parse_cli_report(&output)
    })();
    let _ = fs::remove_dir_all(&work);
    result
}

/// Create a new directory that only this user can enter, under $XDG_RUNTIME_DIR when available.
fn private_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let mut last = None;
    for attempt in 0..16u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = base.join(format!(
            "limitbar-agy-{}-{:08x}{:02x}",
            std::process::id(),
            nanos,
            attempt
        ));
        // `create` fails if the path already exists, including as a symlink planted by someone else.
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => return Err(format!("cannot create {}: {error}", path.display())),
        }
    }
    Err(format!(
        "cannot create a private directory: {}",
        last.map(|e| e.to_string()).unwrap_or_default()
    ))
}

fn parse_cli_report(output: &[u8]) -> Result<Usage, String> {
    let text = String::from_utf8_lossy(output);
    let lowered = text.to_lowercase();
    if lowered.contains("select login method") || lowered.contains("not logged into antigravity") {
        return Err("Not logged in to Antigravity. Run `agy` and sign in.".into());
    }
    let start = text.find('{').ok_or("agy /usage returned no JSON")?;
    let report: Value = serde_json::from_str(text[start..].trim())
        .map_err(|_| "agy /usage returned unreadable JSON".to_string())?;
    if report["status"] != "SUCCESS" || report["command"]["name"] != "usage" {
        return Err(format!(
            "agy /usage failed (status {})",
            report["status"].as_str().unwrap_or("unknown")
        ));
    }
    map_summary(&report["command"]["data"])
}

// ---------- shared quota-summary mapping ----------

fn field<'a>(value: &'a Value, camel: &str, snake: &str) -> &'a Value {
    if value[camel].is_null() {
        &value[snake]
    } else {
        &value[camel]
    }
}

fn fraction(bucket: &Value) -> Option<f64> {
    let direct = field(bucket, "remainingFraction", "remaining_fraction");
    let nested = &bucket["remaining"];
    direct
        .as_f64()
        .or_else(|| field(nested, "remainingFraction", "remaining_fraction").as_f64())
        .or_else(|| {
            (nested["case"] == "remainingFraction")
                .then(|| nested["value"].as_f64())
                .flatten()
        })
}

/// Window length from the explicit `window`, else from the id or name.
fn cadence(bucket: &Value) -> Option<u32> {
    let candidates = [
        bucket["window"].as_str(),
        bucket["bucketId"].as_str().or(bucket["id"].as_str()),
        bucket["name"].as_str(),
    ];
    candidates.into_iter().flatten().find_map(|raw| {
        let text = raw.trim().to_lowercase().replace('_', "-");
        let text = text.strip_suffix(" limit").unwrap_or(&text);
        let is = |alias: &str| text == alias || text.ends_with(&format!("-{alias}"));
        if ["session", "5h", "5-hour", "five hour", "five-hour"]
            .iter()
            .any(|a| is(a))
            || text.contains("five hour")
        {
            Some(300)
        } else if is("weekly") || text.contains("weekly") {
            Some(10080)
        } else {
            None
        }
    })
}

fn family_label(group: &str, bucket_id: &str) -> String {
    let id = bucket_id.to_lowercase();
    if id.starts_with("gemini-")
        && (id == "gemini-5h" || id == "gemini-weekly" || id.starts_with("gemini-session"))
    {
        "Gemini".into()
    } else if id.starts_with("3p-") || id.contains("third-party") {
        "Claude/GPT".into()
    } else {
        group.trim().into()
    }
}

fn map_summary(data: &Value) -> Result<Usage, String> {
    let mut windows = Vec::new();
    let mut extra = Vec::new();
    for group in data["groups"].as_array().into_iter().flatten() {
        let group_name = group["displayName"]
            .as_str()
            .or(group["name"].as_str())
            .unwrap_or("");
        for bucket in group["buckets"].as_array().into_iter().flatten() {
            let id = bucket["bucketId"]
                .as_str()
                .or(bucket["id"].as_str())
                .map(str::trim)
                .unwrap_or("");
            if id.is_empty() || bucket["disabled"] == true {
                continue;
            }
            let Some(fraction) = fraction(bucket) else {
                continue;
            };
            let minutes = cadence(bucket);
            let family = family_label(group_name, id);
            let label = match minutes {
                Some(300) => format!("{family} 5-hour"),
                Some(10080) => format!("{family} weekly"),
                _ => bucket["displayName"]
                    .as_str()
                    .or(bucket["name"].as_str())
                    .unwrap_or(id)
                    .to_string(),
            };
            let window = Window {
                label,
                used_percent: 100.0 - (fraction * 100.0).clamp(0.0, 100.0),
                window_minutes: minutes,
                resets_at: parse_time(field(bucket, "resetTime", "reset_time")),
            };
            if family == "Gemini" || family == "Claude/GPT" {
                windows.push(window)
            } else {
                extra.push(window)
            }
        }
    }
    // Gemini first, then Claude/GPT; 5-hour before weekly inside each family.
    windows.sort_by_key(|w| {
        (
            !w.label.starts_with("Gemini"),
            w.window_minutes.unwrap_or(u32::MAX),
        )
    });
    if windows.is_empty() && extra.is_empty() {
        return Err("Antigravity reported no quota buckets".into());
    }
    if windows.is_empty() {
        std::mem::swap(&mut windows, &mut extra);
    }
    Ok(Usage {
        provider: "antigravity",
        name: "Antigravity",
        source: "local",
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

    fn cli_report() -> Vec<u8> {
        json!({
            "status": "SUCCESS",
            "command": {"name": "usage", "data": {"groups": [
                {"name": "Gemini Models", "buckets": [
                    {"id": "gemini-weekly", "name": "Weekly Limit Remaining", "window": "weekly", "remaining_fraction": 0.9, "reset_time": "2026-11-26T16:18:16Z"},
                    {"id": "gemini-5h", "name": "Five Hour Limit Remaining", "window": "5h", "remaining_fraction": 0.875, "reset_time": "2026-11-20T14:08:16Z"}
                ]},
                {"name": "Claude and GPT models", "buckets": [
                    {"id": "3p-weekly", "window": "weekly", "remaining_fraction": 0.96},
                    {"id": "3p-5h", "window": "5h", "remaining_fraction": 0.5}
                ]},
                {"name": "Gemini 4 Argon", "buckets": [
                    {"id": "gemini-argon-wtu-weekly", "name": "Gemini 4 Argon Weekly Limit", "window": "weekly", "remaining_fraction": 0.8}
                ]}
            ]}}
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn maps_cli_report_families_and_cadences() {
        let usage = parse_cli_report(&cli_report()).unwrap();
        let labels: Vec<_> = usage.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Gemini 5-hour",
                "Gemini weekly",
                "Claude/GPT 5-hour",
                "Claude/GPT weekly"
            ]
        );
        assert_eq!(usage.windows[0].used_percent, 12.5);
        assert_eq!(usage.extra[0].label, "Gemini 4 Argon weekly");
    }

    #[test]
    fn local_api_camel_case_and_nested_fraction() {
        let data = json!({"groups": [{"displayName": "Gemini Models", "buckets": [
            {"bucketId": "gemini-5h", "remaining": {"case": "remainingFraction", "value": 0.25}, "resetTime": "2026-11-20T14:08:16Z"},
            {"bucketId": "gemini-weekly", "remaining": {"remainingFraction": 1.0}},
            {"bucketId": "gemini-off", "disabled": true, "remainingFraction": 0.0}
        ]}]});
        let usage = map_summary(&data).unwrap();
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].used_percent, 75.0);
        assert!(usage.windows[0].resets_at.is_some());
    }

    #[test]
    fn logged_out_and_failed_reports() {
        assert!(
            parse_cli_report(b"Select login method:")
                .unwrap_err()
                .contains("Not logged in")
        );
        assert!(parse_cli_report(br#"{"status":"ERROR","command":{"name":"usage"}}"#).is_err());
    }

    #[test]
    fn private_dir_is_fresh_and_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let first = private_dir().unwrap();
        let second = private_dir().unwrap();
        assert_ne!(first, second);
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir(&first).unwrap();
        fs::remove_dir(&second).unwrap();
    }

    #[test]
    fn parses_flags() {
        let args: Vec<String> = ["ls", "--csrf_token", "abc", "--extension_server_port=4242"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag(&args, "--csrf_token").as_deref(), Some("abc"));
        assert_eq!(
            flag(&args, "--extension_server_port").as_deref(),
            Some("4242")
        );
    }
}
