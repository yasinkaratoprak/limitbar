use crate::model::{Report, Usage, Window};
use chrono::{DateTime, Utc};

pub struct Style {
    pub color: bool,
    pub show_account: bool,
}

impl Style {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
}

pub fn text(reports: &[Report], style: &Style, now: DateTime<Utc>) -> String {
    let mut out = String::new();
    for (index, report) in reports.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        match report {
            Report::Ok(usage) => usage_block(&mut out, usage, style, now),
            Report::Failed { name, error, .. } => {
                out.push_str(&style.paint("1", &format!("== {name} ==")));
                out.push('\n');
                out.push_str(&style.paint("31", &format!("Error: {error}")));
                out.push('\n');
            }
        }
    }
    if reports.is_empty() {
        out.push_str("No signed-in providers found. Sign in with a provider CLI (codex, claude, gemini, agy) and try again.\n");
    }
    out
}

fn usage_block(out: &mut String, usage: &Usage, style: &Style, now: DateTime<Utc>) {
    out.push_str(&style.paint("1", &format!("== {} ({}) ==", usage.name, usage.source)));
    out.push('\n');
    for window in &usage.windows {
        window_lines(out, window, style, now);
    }
    if !usage.extra.is_empty() {
        for window in &usage.extra {
            let left = window.remaining_percent();
            let reset = window
                .resets_at
                .map(|at| format!(" · {}", reset_text(at, now)))
                .unwrap_or_default();
            out.push_str(&format!(
                "  {}: {}{}\n",
                window.label,
                style.paint(level_color(left), &format!("{left:.0}% left")),
                reset
            ));
        }
    }
    if let Some(credits) = usage.credits {
        out.push_str(&format!("Credits: {}\n", trim_number(credits)));
    }
    if style.show_account
        && let Some(account) = &usage.account
    {
        out.push_str(&format!("Account: {account}\n"));
    }
    if let Some(plan) = &usage.plan {
        out.push_str(&format!("Plan: {plan}\n"));
    }
}

fn window_lines(out: &mut String, window: &Window, style: &Style, now: DateTime<Utc>) {
    let left = window.remaining_percent();
    out.push_str(&format!(
        "{}: {} {}\n",
        window.label,
        style.paint(level_color(left), &format!("{left:.0}% left")),
        bar(left)
    ));
    if let Some(at) = window.resets_at {
        out.push_str(&format!("{}\n", reset_text(at, now)));
    }
}

fn level_color(left: f64) -> &'static str {
    if left <= 10.0 {
        "31"
    } else if left <= 30.0 {
        "33"
    } else {
        "32"
    }
}

pub fn bar(left: f64) -> String {
    let filled = ((left / 100.0) * 12.0).round().clamp(0.0, 12.0) as usize;
    format!("[{}{}]", "=".repeat(filled), "-".repeat(12 - filled))
}

pub fn reset_text(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = (at - now).num_minutes();
    if minutes <= 0 {
        return "Resets now".into();
    }
    if minutes < 60 {
        format!("Resets in {minutes}m")
    } else if minutes < 1440 {
        format!("Resets in {}h {}m", minutes / 60, minutes % 60)
    } else {
        format!("Resets in {}d {}h", minutes / 1440, minutes % 1440 / 60)
    }
}

fn trim_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn reset_text_formats() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(
            reset_text(now + chrono::Duration::minutes(45), now),
            "Resets in 45m"
        );
        assert_eq!(
            reset_text(now + chrono::Duration::minutes(90), now),
            "Resets in 1h 30m"
        );
        assert_eq!(
            reset_text(now + chrono::Duration::minutes(3000), now),
            "Resets in 2d 2h"
        );
        assert_eq!(
            reset_text(now - chrono::Duration::minutes(1), now),
            "Resets now"
        );
    }

    #[test]
    fn bar_fills_by_remaining() {
        assert_eq!(bar(100.0), "[============]");
        assert_eq!(bar(0.0), "[------------]");
        assert_eq!(bar(70.0), "[========----]");
    }
}
