use chrono::{DateTime, Utc};
use serde::Serialize;

/// One rate-limit window, such as a 5-hour session or a 7-day week.
#[derive(Debug, Clone, Serialize)]
pub struct Window {
    pub label: String,
    /// Share of the window already used, 0-100.
    pub used_percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_minutes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,
}

impl Window {
    pub fn remaining_percent(&self) -> f64 {
        (100.0 - self.used_percent).clamp(0.0, 100.0)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub provider: &'static str,
    pub name: &'static str,
    /// Where the numbers came from: "oauth" or "local".
    pub source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Main windows, shortest first (session, weekly).
    pub windows: Vec<Window>,
    /// Narrower windows, for example per model family.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<Window>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits: Option<f64>,
}

/// Result of asking one provider.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Report {
    Ok(Usage),
    Failed {
        provider: &'static str,
        name: &'static str,
        error: String,
    },
}

/// Label for a window length in minutes.
pub fn window_label(minutes: Option<u32>, fallback: &str) -> String {
    match minutes {
        Some(300) => "Session".into(),
        Some(10080) => "Weekly".into(),
        Some(m) if m >= 1440 && m % 1440 == 0 => format!("{} day", m / 1440),
        Some(m) if m >= 60 && m % 60 == 0 => format!("{} hour", m / 60),
        _ => fallback.into(),
    }
}
