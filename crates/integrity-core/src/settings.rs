//! Persisted user settings.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SETTINGS_FILE: &str = "settings.json";
pub const APP_DIR_NAME: &str = "integrity-browser";
pub const CONFIG_DIR_ENV: &str = "INTEGRITY_BROWSER_CONFIG_DIR";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    Relaxed,
    Balanced,
    Strict,
}

impl Default for Sensitivity {
    fn default() -> Self {
        Sensitivity::Balanced
    }
}

impl Sensitivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Sensitivity::Relaxed => "relaxed",
            Sensitivity::Balanced => "balanced",
            Sensitivity::Strict => "strict",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterListKind {
    BuiltinAds,
    BuiltinTrackers,
    Remote,
}

impl Default for FilterListKind {
    fn default() -> Self {
        FilterListKind::Remote
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilterListConfig {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    #[serde(default)]
    pub kind: FilterListKind,
    #[serde(default)]
    pub url: Option<String>,
    /// Path relative to the config directory.
    #[serde(default)]
    pub cached_file: Option<String>,
    #[serde(default)]
    pub rule_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub integrity_enabled: bool,
    pub integrity_sensitivity: Sensitivity,
    pub integrity_click_through: bool,
    /// Suppress navigation from hidden links that open a new tab.
    pub integrity_block_hidden_tabs: bool,
    /// Hostnames where Integrity Protection is paused by the user.
    pub site_allowlist: Vec<String>,
    pub adblock_enabled: bool,
    pub tracker_blocking_enabled: bool,
    pub filter_lists: Vec<FilterListConfig>,
    /// Hosts typed in manually by the user (never downloaded).
    pub custom_blocked_hosts: Vec<String>,
    pub search_engine: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            integrity_enabled: true,
            integrity_sensitivity: Sensitivity::Balanced,
            integrity_click_through: true,
            integrity_block_hidden_tabs: false,
            site_allowlist: Vec::new(),
            adblock_enabled: true,
            tracker_blocking_enabled: true,
            filter_lists: default_filter_lists(),
            custom_blocked_hosts: Vec::new(),
            search_engine: "duckduckgo".into(),
        }
    }
}

pub fn default_filter_lists() -> Vec<FilterListConfig> {
    vec![
        FilterListConfig {
            id: "builtin-ads".into(),
            name: "Integrity Ads (built-in)".into(),
            enabled: true,
            kind: FilterListKind::BuiltinAds,
            url: None,
            cached_file: None,
            rule_count: 0,
        },
        FilterListConfig {
            id: "builtin-trackers".into(),
            name: "Integrity Trackers (built-in)".into(),
            enabled: true,
            kind: FilterListKind::BuiltinTrackers,
            url: None,
            cached_file: None,
            rule_count: 0,
        },
    ]
}

impl Settings {
    pub fn normalized(mut self) -> Self {
        self.version = 1;
        self.search_engine = "duckduckgo".into();
        self.site_allowlist = normalize_hosts(&self.site_allowlist);
        self.custom_blocked_hosts = normalize_hosts(&self.custom_blocked_hosts);

        // Guarantee that built-in lists always exist exactly once.
        let mut seen = std::collections::HashSet::new();
        self.filter_lists.retain(|list| seen.insert(list.id.clone()));
        for default in default_filter_lists() {
            if !self.filter_lists.iter().any(|l| l.id == default.id) {
                self.filter_lists.push(default);
            }
        }
        // Built-in list kinds are authoritative.
        for list in &mut self.filter_lists {
            if list.id == "builtin-ads" {
                list.kind = FilterListKind::BuiltinAds;
            } else if list.id == "builtin-trackers" {
                list.kind = FilterListKind::BuiltinTrackers;
            }
            if list.kind == FilterListKind::Remote && list.cached_file.is_none() {
                list.enabled = false;
            }
        }
        self
    }

    pub fn load() -> (Self, Vec<String>) {
        let path = settings_path();
        match Self::load_from(&path) {
            Ok(settings) => (settings, Vec::new()),
            Err(err) => (
                Settings::default(),
                vec![format!(
                    "Could not load settings from {}: {err}. Using defaults.",
                    path.display()
                )],
            ),
        }
    }

    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let settings: Settings = serde_json::from_str(&text)?;
        Ok(settings.normalized())
    }

    pub fn save(&self) -> anyhow::Result<()> {
        self.save_to(&settings_path())
    }

    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    pub fn is_site_allowed(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        self.site_allowlist.iter().any(|entry| {
            host == *entry || host.ends_with(&format!(".{entry}"))
        })
    }

    pub fn set_site_allowed(&mut self, host: &str, allowed: bool) {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return;
        }
        if allowed {
            if !self.site_allowlist.iter().any(|h| h == &host) {
                self.site_allowlist.push(host);
            }
        } else {
            self.site_allowlist.retain(|h| h != &host);
        }
    }

    pub fn filter_list(&self, id: &str) -> Option<&FilterListConfig> {
        self.filter_lists.iter().find(|l| l.id == id)
    }
}

fn normalize_hosts(entries: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for entry in entries {
        let mut host = entry.trim().to_ascii_lowercase();
        if host.is_empty() {
            continue;
        }
        if let Ok(url) = url::Url::parse(&host) {
            if let Some(h) = url.host_str() {
                host = h.to_string();
            }
        }
        host = host.trim_start_matches("*.").to_string();
        host = host.trim_end_matches('.').to_string();
        if host.is_empty() || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '*')
        {
            continue;
        }
        if !out.contains(&host) {
            out.push(host);
        }
    }
    out
}

pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(CONFIG_DIR_ENV) {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    dirs::config_dir()
        .map(|dir| dir.join(APP_DIR_NAME))
        .unwrap_or_else(|| PathBuf::from(".integrity-browser"))
}

pub fn settings_path() -> PathBuf {
    config_dir().join(SETTINGS_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_contain_builtin_lists() {
        let s = Settings::default();
        assert_eq!(s.filter_lists.len(), 2);
        assert!(s.filter_lists.iter().all(|l| l.enabled));
        assert!(s.integrity_enabled);
        assert!(!s.integrity_block_hidden_tabs);
        assert!(s.adblock_enabled && s.tracker_blocking_enabled);
    }

    #[test]
    fn normalization_repairs_missing_builtins_and_duplicates() {
        let mut s = Settings::default();
        s.filter_lists.pop();
        s.filter_lists.push(s.filter_lists[0].clone());
        s.site_allowlist = vec![
            "HTTPS://Example.COM/path".into(),
            "example.com".into(),
            "*.foo.example".into(),
        ];
        let s = s.normalized();
        assert_eq!(
            s.filter_lists
                .iter()
                .filter(|l| l.id == "builtin-ads")
                .count(),
            1
        );
        assert!(s.filter_lists.iter().any(|l| l.id == "builtin-trackers"));
        assert_eq!(
            s.site_allowlist,
            vec!["example.com".to_string(), "foo.example".to_string()]
        );
    }

    #[test]
    fn site_allowlist_matches_subdomains() {
        let mut s = Settings::default();
        s.set_site_allowed("example.com", true);
        assert!(s.is_site_allowed("example.com"));
        assert!(s.is_site_allowed("www.example.com"));
        assert!(!s.is_site_allowed("notexample.com"));
        s.set_site_allowed("example.com", false);
        assert!(!s.is_site_allowed("www.example.com"));
    }

    #[test]
    fn round_trip_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE);
        let mut s = Settings::default();
        s.integrity_sensitivity = Sensitivity::Strict;
        s.integrity_block_hidden_tabs = true;
        s.adblock_enabled = false;
        s.custom_blocked_hosts.push("tracker.example".into());
        s.save_to(&path).unwrap();
        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.integrity_sensitivity, Sensitivity::Strict);
        assert!(loaded.integrity_block_hidden_tabs);
        assert!(!loaded.adblock_enabled);
        assert_eq!(loaded.custom_blocked_hosts, vec!["tracker.example"]);
    }

    #[test]
    fn corrupt_settings_are_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE);
        std::fs::write(&path, "{not json").unwrap();
        assert!(Settings::load_from(&path).is_err());
    }
}
