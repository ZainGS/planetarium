//! settings.rs — Planetarium's own settings (collision behavior), saved next to the repo list
//! as coordination.json in the app's data folder.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub const MODES: &[&str] = &["off", "warn", "ask", "talk"];

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct CoordSettings {
    /// Collision mode used unless a repo overrides it: "off" | "warn" | "ask" | "talk".
    pub default_mode: String,
    /// repo id → mode.
    pub repo_modes: HashMap<String, String>,
    /// How long an agent waits for your answer in Ask mode before it's just warned and continues.
    pub ask_timeout_secs: u64,
    /// Your own wording for "Wind down all agents" ({percent} and {resets_in} are filled in).
    /// None = Planetarium's default.
    pub wind_down_message: Option<String>,
}

impl Default for CoordSettings {
    fn default() -> Self {
        CoordSettings { default_mode: "warn".into(), repo_modes: HashMap::new(), ask_timeout_secs: 300, wind_down_message: None }
    }
}

impl CoordSettings {
    pub fn mode_for(&self, repo_id: Option<&str>) -> String {
        repo_id
            .and_then(|id| self.repo_modes.get(id))
            .filter(|m| MODES.contains(&m.as_str()))
            .cloned()
            .unwrap_or_else(|| self.default_mode.clone())
    }

    /// Keep values sane whatever the page sends.
    pub fn sanitized(mut self) -> Self {
        if !MODES.contains(&self.default_mode.as_str()) {
            self.default_mode = "warn".into();
        }
        self.repo_modes.retain(|_, m| MODES.contains(&m.as_str()));
        // Stay under the hook's own timeout (see claude_settings.rs), or Claude Code gives up first.
        self.ask_timeout_secs = self.ask_timeout_secs.clamp(30, 540);
        self.wind_down_message = self
            .wind_down_message
            .map(|m| m.trim().chars().take(2000).collect::<String>())
            .filter(|m| !m.is_empty());
        self
    }
}

pub struct SettingsStore {
    file: PathBuf,
    pub value: CoordSettings,
}

impl SettingsStore {
    pub fn open(file: PathBuf) -> Self {
        let value = fs::read_to_string(&file)
            .ok()
            .and_then(|s| serde_json::from_str::<CoordSettings>(&s).ok())
            .unwrap_or_default()
            .sanitized();
        SettingsStore { file, value }
    }

    pub fn set(&mut self, value: CoordSettings) -> Result<CoordSettings, String> {
        self.value = value.sanitized();
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let body = serde_json::to_string_pretty(&self.value).map_err(|e| e.to_string())?;
        fs::write(&self.file, body).map_err(|e| e.to_string())?;
        Ok(self.value.clone())
    }
}
