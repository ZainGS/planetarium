//! usage.rs — how much of your Claude plan's usage limits is used, and when it resets.
//!
//! Claude Code hands this to its status line (the info line under the prompt) as `rate_limits`,
//! on Pro/Max plans, once a session has had its first reply. Connect installs a tiny status line
//! that posts that JSON to Planetarium (server.rs, /planetarium/statusline) and shows what
//! Planetarium sends back. Limits are per account, so whichever session reported last is current.

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// 0–100.
    pub used_percentage: f64,
    /// Unix time, seconds.
    pub resets_at: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// The rolling 5-hour "session" limit.
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    /// When Planetarium last heard (ms).
    pub updated_at: u64,
}

fn window(v: &Value, key: &str) -> Option<Window> {
    let w = v.get(key)?;
    let used_percentage = w.get("used_percentage")?.as_f64()?.clamp(0.0, 100.0);
    let resets_at = w.get("resets_at").and_then(|r| r.as_u64().or_else(|| r.as_f64().map(|f| f as u64))).unwrap_or(0);
    Some(Window { used_percentage, resets_at })
}

/// The usage in a status line payload, if it has any.
pub fn parse(body: &str, now_ms: u64) -> Option<Usage> {
    let v: Value = serde_json::from_str(body).ok()?;
    let limits = v.get("rate_limits")?;
    let usage = Usage { five_hour: window(limits, "five_hour"), seven_day: window(limits, "seven_day"), updated_at: now_ms };
    (usage.five_hour.is_some() || usage.seven_day.is_some()).then_some(usage)
}

/// "20 min", "1 h 05 min", "2 days".
pub fn until(resets_at: u64, now_ms: u64) -> String {
    let secs = resets_at.saturating_sub(now_ms / 1000);
    let mins = (secs + 59) / 60;
    if mins < 60 {
        format!("{} min", mins.max(1))
    } else if mins < 48 * 60 {
        format!("{} h {:02} min", mins / 60, mins % 60)
    } else {
        format!("{} days", mins / (24 * 60))
    }
}

/// The text Planetarium's status line shows in Claude Code.
pub fn status_text(usage: Option<&Usage>, now_ms: u64) -> String {
    let Some(u) = usage else { return "Planetarium".into() };
    let mut parts = vec!["Planetarium".to_string()];
    if let Some(w) = &u.five_hour {
        parts.push(format!("session {:.0}% (resets in {})", w.used_percentage, until(w.resets_at, now_ms)));
    }
    if let Some(w) = &u.seven_day {
        parts.push(format!("week {:.0}%", w.used_percentage));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rate_limits() {
        let now = 1_800_000_000_000u64;
        let body = format!(
            r#"{{"session_id":"s","model":{{"display_name":"Opus"}},"rate_limits":{{"five_hour":{{"used_percentage":87.4,"resets_at":{}}},"seven_day":{{"used_percentage":40,"resets_at":{}}}}}}}"#,
            now / 1000 + 20 * 60,
            now / 1000 + 3 * 86400
        );
        let u = parse(&body, now).unwrap();
        assert_eq!(u.five_hour.as_ref().unwrap().used_percentage, 87.4);
        assert_eq!(until(u.five_hour.as_ref().unwrap().resets_at, now), "20 min");
        assert_eq!(status_text(Some(&u), now), "Planetarium · session 87% (resets in 20 min) · week 40%");
        assert!(parse(r#"{"session_id":"s"}"#, now).is_none(), "API-key sessions have no rate_limits");
        assert_eq!(until(now / 1000 + 65 * 60, now), "1 h 05 min");
    }
}
