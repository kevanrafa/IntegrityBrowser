//! Browser state: tabs, history, settings and the filtered proxy engine.
//!
//! Deliberately UI-independent: the event loop in `main.rs` owns the webviews
//! and only asks this module what the state *is*.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use integrity_core::filters::FilterEngine;
use integrity_core::proxy::{ProxyServer, ProxyStats};
use integrity_core::settings::{
    FilterListConfig, FilterListKind, Sensitivity, Settings,
};
use integrity_core::urlutil::{
    display_host, host_of, is_allowed_navigation, is_internal_url, normalize_user_input,
    same_document, NEWTAB_URL,
};
use serde_json::{json, Value};

pub const TEST_PAGE_URL: &str = "ib://localhost/tests/integrity-harness.html";
pub const SETTINGS_URL: &str = "ib://localhost/settings.html";
pub const MAX_REMOTE_LIST_BYTES: usize = 8 * 1024 * 1024;

pub struct Tab {
    pub id: u64,
    pub title: String,
    pub url: String,
    pub history: Vec<String>,
    pub index: usize,
    pub loading: bool,
}

impl Tab {
    fn new(id: u64, url: String) -> Self {
        Self {
            id,
            title: title_for_url(&url),
            history: vec![url.clone()],
            url,
            index: 0,
            loading: false,
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.history.len()
    }
}

pub fn title_for_url(url: &str) -> String {
    if is_newtab_url(url) {
        "New Tab".into()
    } else if is_settings_url(url) {
        "Integrity settings".into()
    } else if is_internal_url(url) {
        "Integrity self-test".into()
    } else {
        display_host(url)
    }
}

#[derive(Clone, Debug, Default)]
pub struct PageIntegrityStats {
    pub scanned: u64,
    pub suspicious: u64,
    pub neutralized: u64,
    pub click_throughs: u64,
    pub host: String,
    pub last_action: String,
}

pub struct Browser {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub next_tab_id: u64,
    pub settings: Settings,
    pub config_dir: PathBuf,
    pub engine: Arc<RwLock<Arc<FilterEngine>>>,
    pub stats: Arc<ProxyStats>,
    pub proxy: ProxyServer,
    pub pending_nav: Option<String>,
    pub page_stats: PageIntegrityStats,
    pub warnings: Vec<String>,
    pub last_remote_note: String,
    /// Last non-internal host, used so the settings page can pause protection
    /// for the site the user came from instead of for `localhost`.
    pub last_web_host: String,
}

impl Browser {
    pub fn new(
        settings: Settings,
        config_dir: PathBuf,
        engine: Arc<RwLock<Arc<FilterEngine>>>,
        stats: Arc<ProxyStats>,
        proxy: ProxyServer,
        warnings: Vec<String>,
    ) -> Self {
        let stats_enabled = settings.adblock_enabled || settings.tracker_blocking_enabled;
        stats.set_enabled(stats_enabled);
        let mut browser = Self {
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 1,
            settings,
            config_dir,
            engine,
            stats,
            proxy,
            pending_nav: None,
            page_stats: PageIntegrityStats::default(),
            warnings,
            last_remote_note: String::new(),
            last_web_host: String::new(),
        };
        let tab = Tab::new(browser.next_tab_id, NEWTAB_URL.to_string());
        browser.next_tab_id += 1;
        browser.tabs.push(tab);
        browser
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active.min(self.tabs.len().saturating_sub(1))]
    }

    fn active_tab_mut(&mut self) -> &mut Tab {
        let index = self.active.min(self.tabs.len().saturating_sub(1));
        &mut self.tabs[index]
    }

    pub fn active_url(&self) -> String {
        self.active_tab().url.clone()
    }

    pub fn active_host(&self) -> String {
        host_of(&self.active_tab().url).unwrap_or_default()
    }

    /// Host the site controls should apply to. While an internal page is open,
    /// this is the last real website so "pause for this site" keeps working
    /// from the settings page.
    pub fn allowlist_target_host(&self) -> String {
        if self.active_is_internal() {
            self.last_web_host.clone()
        } else {
            self.active_host()
        }
    }

    pub fn active_is_newtab(&self) -> bool {
        is_newtab_url(&self.active_tab().url)
    }

    pub fn active_is_internal(&self) -> bool {
        is_internal_url(&self.active_tab().url)
    }

    pub fn integrity_effective(&self) -> bool {
        let host = self.active_host();
        self.settings.integrity_enabled
            && !host.is_empty()
            && !self.settings.is_site_allowed(&host)
    }

    pub fn site_allowed(&self) -> bool {
        let host = self.active_host();
        !host.is_empty() && self.settings.is_site_allowed(&host)
    }

    pub fn window_title(&self) -> String {
        let title = self.active_tab().title.trim();
        if title.is_empty() || title == "New Tab" {
            "Integrity Browser".into()
        } else {
            format!("{title} - Integrity Browser")
        }
    }

    // ----------------------------------------------------------------- tabs

    pub fn new_tab(&mut self) {
        let tab = Tab::new(self.next_tab_id, NEWTAB_URL.to_string());
        self.next_tab_id += 1;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.pending_nav = None;
        self.page_stats = PageIntegrityStats::default();
    }

    pub fn close_tab(&mut self, id: u64) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            let tab = Tab::new(self.next_tab_id, NEWTAB_URL.to_string());
            self.next_tab_id += 1;
            self.tabs.push(tab);
            self.active = 0;
        } else if self.active >= index && self.active > 0 {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.pending_nav = None;
        self.page_stats = PageIntegrityStats::default();
        true
    }

    pub fn close_active(&mut self) -> bool {
        let id = self.active_tab().id;
        self.close_tab(id)
    }

    pub fn switch_tab(&mut self, id: u64) -> Option<String> {
        let index = self.tabs.iter().position(|tab| tab.id == id)?;
        if index == self.active {
            return None;
        }
        self.active = index;
        self.pending_nav = None;
        self.page_stats = PageIntegrityStats::default();
        Some(self.active_url())
    }

    // ----------------------------------------------------------- navigation

    pub fn normalize(&self, input: &str) -> Option<String> {
        normalize_user_input(input).filter(|url| is_allowed_navigation(url))
    }

    pub fn back_target(&mut self) -> Option<String> {
        let tab = self.active_tab_mut();
        if !tab.can_go_back() {
            return None;
        }
        tab.index -= 1;
        tab.loading = true;
        let url = tab.history[tab.index].clone();
        self.pending_nav = Some(url.clone());
        Some(url)
    }

    pub fn forward_target(&mut self) -> Option<String> {
        let tab = self.active_tab_mut();
        if !tab.can_go_forward() {
            return None;
        }
        tab.index += 1;
        tab.loading = true;
        let url = tab.history[tab.index].clone();
        self.pending_nav = Some(url.clone());
        Some(url)
    }

    pub fn on_load_started(&mut self, url: &str) {
        self.record_web_host(url);
        let tab = self.active_tab_mut();
        tab.url = url.to_string();
        tab.loading = true;
        if is_internal_url(url) {
            tab.title = title_for_url(url);
        }
    }

    pub fn on_load_finished(&mut self, url: &str) {
        self.record_web_host(url);
        let pending = self.pending_nav.clone();
        {
            let tab = self.active_tab_mut();
            tab.url = url.to_string();
            tab.loading = false;
            if tab.title.trim().is_empty() || is_internal_url(url) {
                tab.title = title_for_url(url);
            }
        }

        if let Some(expected) = pending {
            if expected == url || same_document(&expected, url) {
                self.pending_nav = None;
                return;
            }
        }
        self.pending_nav = None;

        let tab = self.active_tab_mut();
        if tab.history.get(tab.index).map(|u| u == url).unwrap_or(false) {
            return;
        }
        if tab
            .history
            .get(tab.index)
            .map(|u| same_document(u, url))
            .unwrap_or(false)
        {
            tab.history[tab.index] = url.to_string();
            return;
        }
        tab.history.truncate(tab.index + 1);
        tab.history.push(url.to_string());
        tab.index = tab.history.len() - 1;
    }

    pub fn on_same_document_navigation(&mut self, url: &str) -> bool {
        self.record_web_host(url);
        let tab = self.active_tab_mut();
        if same_document(&tab.url, url) {
            tab.url = url.to_string();
            if let Some(entry) = tab.history.get_mut(tab.index) {
                *entry = url.to_string();
            }
            true
        } else {
            false
        }
    }

    pub fn set_title(&mut self, title: String) {
        let tab = self.active_tab_mut();
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        let title: String = title.chars().take(300).collect();
        tab.title = title;
    }

    fn record_web_host(&mut self, url: &str) {
        if is_internal_url(url) {
            return;
        }
        if let Some(host) = host_of(url) {
            if !host.is_empty() {
                self.last_web_host = host;
            }
        }
    }

    pub fn mark_loading(&mut self) {
        self.active_tab_mut().loading = true;
    }

    pub fn mark_loading_done(&mut self) {
        self.active_tab_mut().loading = false;
    }

    // ------------------------------------------------------------- settings

    pub fn toggle_integrity(&mut self, enabled: bool) {
        self.settings.integrity_enabled = enabled;
        self.persist();
    }

    pub fn toggle_click_through(&mut self, enabled: bool) {
        self.settings.integrity_click_through = enabled;
        self.persist();
    }

    pub fn set_sensitivity(&mut self, sensitivity: Sensitivity) {
        self.settings.integrity_sensitivity = sensitivity;
        self.persist();
    }

    pub fn set_site_allowed(&mut self, allowed: bool) {
        let host = self.allowlist_target_host();
        if host.is_empty() {
            return;
        }
        if allowed {
            self.settings.set_site_allowed(&host, true);
        } else {
            // Remove the exact host and any parent entry that still matches;
            // otherwise "resume protection" would silently do nothing.
            self.settings.site_allowlist.retain(|entry| {
                !(host == *entry || host.ends_with(&format!(".{entry}")))
            });
        }
        self.persist();
    }

    pub fn toggle_adblock(&mut self, enabled: bool) {
        self.settings.adblock_enabled = enabled;
        self.persist();
        self.rebuild_engine();
    }

    pub fn toggle_trackers(&mut self, enabled: bool) {
        self.settings.tracker_blocking_enabled = enabled;
        self.persist();
        self.rebuild_engine();
    }

    pub fn toggle_filter_list(&mut self, id: &str, enabled: bool) -> bool {
        let Some(list) = self.settings.filter_lists.iter_mut().find(|l| l.id == id) else {
            return false;
        };
        if list.kind == FilterListKind::Remote && list.cached_file.is_none() {
            return false;
        }
        list.enabled = enabled;
        self.persist();
        self.rebuild_engine();
        true
    }

    pub fn remove_filter_list(&mut self, id: &str) -> bool {
        let Some(position) = self
            .settings
            .filter_lists
            .iter()
            .position(|l| l.id == id && l.kind == FilterListKind::Remote)
        else {
            return false;
        };
        let removed = self.settings.filter_lists.remove(position);
        if let Some(rel) = removed.cached_file {
            let path = self.config_dir.join(rel);
            if path.starts_with(&self.config_dir) {
                let _ = std::fs::remove_file(path);
            }
        }
        self.persist();
        self.rebuild_engine();
        true
    }

    pub fn add_custom_host(&mut self, input: &str) -> Result<String, String> {
        let host = clean_host(input).ok_or_else(|| "That does not look like a hostname".to_string())?;
        if self.settings.custom_blocked_hosts.iter().any(|h| h == &host) {
            return Ok(host);
        }
        self.settings.custom_blocked_hosts.push(host.clone());
        self.persist();
        self.rebuild_engine();
        Ok(host)
    }

    pub fn remove_custom_host(&mut self, host: &str) -> bool {
        let before = self.settings.custom_blocked_hosts.len();
        self.settings
            .custom_blocked_hosts
            .retain(|h| !h.eq_ignore_ascii_case(host));
        if self.settings.custom_blocked_hosts.len() == before {
            return false;
        }
        self.persist();
        self.rebuild_engine();
        true
    }

    pub fn install_remote_list(&mut self, url: &str, text: &str) -> Result<String, String> {
        let url = validate_list_url(url)?;
        let rule_count = text
            .lines()
            .filter(|line| {
                let line = line.trim();
                !line.is_empty() && !line.starts_with('!') && !line.starts_with('#')
            })
            .count();

        let mut hasher = DefaultHasher::new();
        url.hash(&mut hasher);
        let id = format!("remote-{:016x}", hasher.finish());
        let file_name = format!("{id}.txt");
        let lists_dir = self.config_dir.join("lists");
        std::fs::create_dir_all(&lists_dir)
            .map_err(|err| format!("Could not create list cache directory: {err}"))?;
        std::fs::write(lists_dir.join(&file_name), text)
            .map_err(|err| format!("Could not cache filter list: {err}"))?;

        let name = url
            .split("://")
            .nth(1)
            .unwrap_or(&url)
            .split('/')
            .next()
            .unwrap_or("Remote list")
            .to_string();

        if let Some(list) = self.settings.filter_lists.iter_mut().find(|l| l.url.as_deref() == Some(url.as_str())) {
            list.enabled = true;
            list.cached_file = Some(format!("lists/{file_name}"));
            list.rule_count = rule_count;
        } else {
            self.settings.filter_lists.push(FilterListConfig {
                id,
                name,
                enabled: true,
                kind: FilterListKind::Remote,
                url: Some(url),
                cached_file: Some(format!("lists/{file_name}")),
                rule_count,
            });
        }
        self.persist();
        self.rebuild_engine();
        Ok(rule_count.to_string())
    }

    pub fn remote_list_urls(&self) -> Vec<String> {
        self.settings
            .filter_lists
            .iter()
            .filter(|l| l.kind == FilterListKind::Remote)
            .filter_map(|l| l.url.clone())
            .collect()
    }

    fn persist(&mut self) {
        if let Err(err) = self.settings.save() {
            self.warnings.push(format!("Could not save settings: {err}"));
        }
    }

    pub fn rebuild_engine(&mut self) {
        let (engine, warnings) = build_engine(&self.settings, &self.config_dir);
        {
            let mut guard = self.engine.write().unwrap_or_else(|e| e.into_inner());
            *guard = Arc::new(engine);
        }
        self.stats.set_enabled(
            self.settings.adblock_enabled || self.settings.tracker_blocking_enabled,
        );
        self.warnings = warnings;
    }

    // ------------------------------------------------------------- snapshots

    pub fn content_config(&self) -> Value {
        json!({
            "enabled": self.integrity_effective(),
            "masterEnabled": self.settings.integrity_enabled,
            "siteAllowed": self.site_allowed(),
            "sensitivity": self.settings.integrity_sensitivity.as_str(),
            "clickThrough": self.settings.integrity_click_through,
            "host": self.active_host(),
        })
    }

    pub fn snapshot(&self) -> Value {
        let tab = self.active_tab();
        let host = self.active_host();
        let stats = self.stats.snapshot();
        let recent: Vec<Value> = stats
            .recent
            .iter()
            .take(20)
            .map(|item| {
                json!({
                    "host": item.host,
                    "category": item.category,
                    "atMs": item.at_ms,
                })
            })
            .collect();
        let top_hosts: Vec<Value> = stats
            .top_hosts
            .iter()
            .map(|item| json!({ "host": item.host, "count": item.count }))
            .collect();
        let lists: Vec<Value> = self
            .settings
            .filter_lists
            .iter()
            .map(|list| {
                json!({
                    "id": list.id,
                    "name": list.name,
                    "enabled": list.enabled,
                    "kind": list.kind,
                    "url": list.url,
                    "ruleCount": list.rule_count,
                    "cached": list.cached_file.is_some(),
                })
            })
            .collect();
        let tabs: Vec<Value> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                json!({
                    "id": tab.id,
                    "title": if tab.title.trim().is_empty() { display_host(&tab.url) } else { tab.title.clone() },
                    "url": tab.url,
                    "loading": tab.loading,
                    "active": index == self.active,
                })
            })
            .collect();

        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "tabs": tabs,
            "activeIndex": self.active,
            "activeTabId": tab.id,
            "url": tab.url,
            "title": tab.title,
            "loading": tab.loading,
            "canGoBack": tab.can_go_back(),
            "canGoForward": tab.can_go_forward(),
            "isInternal": is_internal_url(&tab.url),
            "host": host,
            "integrity": {
                "masterEnabled": self.settings.integrity_enabled,
                "enabled": self.integrity_effective(),
                "siteAllowed": self.site_allowed(),
                "clickThrough": self.settings.integrity_click_through,
                "sensitivity": self.settings.integrity_sensitivity.as_str(),
                "targetHost": self.allowlist_target_host(),
                "targetSiteAllowed": self
                    .settings
                    .is_site_allowed(&self.allowlist_target_host()),
                "page": {
                    "scanned": self.page_stats.scanned,
                    "suspicious": self.page_stats.suspicious,
                    "neutralized": self.page_stats.neutralized,
                    "clickThroughs": self.page_stats.click_throughs,
                    "host": self.page_stats.host,
                    "lastAction": self.page_stats.last_action,
                }
            },
            "privacy": {
                "adblockEnabled": self.settings.adblock_enabled,
                "trackerBlockingEnabled": self.settings.tracker_blocking_enabled,
                "proxyUrl": self.proxy.proxy_url(),
                "stats": {
                    "blocked": stats.blocked,
                    "inspected": stats.inspected,
                    "ads": stats.ads,
                    "trackers": stats.trackers,
                    "custom": stats.custom,
                    "enabled": stats.enabled,
                    "recent": recent,
                    "topHosts": top_hosts,
                },
                "lists": lists,
                "customHosts": self.settings.custom_blocked_hosts,
            },
            "warnings": self.warnings,
            "remoteNote": self.last_remote_note,
        })
    }

    pub fn newtab_snapshot(&self) -> Value {
        let stats = self.stats.snapshot();
        json!({
            "blocked": stats.blocked,
            "ads": stats.ads,
            "trackers": stats.trackers,
            "inspected": stats.inspected,
            "integrityEnabled": self.settings.integrity_enabled,
            "siteAllowed": self.site_allowed(),
            "clickThrough": self.settings.integrity_click_through,
            "adblockEnabled": self.settings.adblock_enabled,
            "trackerBlockingEnabled": self.settings.tracker_blocking_enabled,
        })
    }
}

pub fn is_newtab_url(url: &str) -> bool {
    is_internal_url(url) && url.split(['?', '#']).next().unwrap_or(url).ends_with("/newtab.html")
}

pub fn is_settings_url(url: &str) -> bool {
    is_internal_url(url) && url.split(['?', '#']).next().unwrap_or(url).ends_with("/settings.html")
}

pub fn clean_host(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.len() > 300 {
        return None;
    }
    let raw = if trimmed.contains("://") {
        url::Url::parse(trimmed).ok()?.host_str()?.to_string()
    } else {
        trimmed
            .split(['/', '?', '#'])
            .next()
            .unwrap_or(trimmed)
            .trim_start_matches("*.")
            .to_string()
    };
    let host = raw.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
        || (!host.contains('.') && host != "localhost")
    {
        return None;
    }
    Some(host)
}

pub fn validate_list_url(input: &str) -> Result<String, String> {
    let url = url::Url::parse(input.trim()).map_err(|_| "Invalid filter list URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Filter list URL must use http:// or https://".into());
    }
    if url.host_str().is_none() {
        return Err("Filter list URL has no host".into());
    }
    Ok(url.to_string())
}

pub fn build_engine(
    settings: &Settings,
    config_dir: &Path,
) -> (FilterEngine, Vec<String>) {
    let mut adjusted = settings.clone();
    for list in &mut adjusted.filter_lists {
        match list.kind {
            FilterListKind::BuiltinAds if !settings.adblock_enabled => list.enabled = false,
            FilterListKind::BuiltinTrackers if !settings.tracker_blocking_enabled => {
                list.enabled = false
            }
            FilterListKind::Remote if !settings.adblock_enabled => list.enabled = false,
            _ => {}
        }
    }
    if !settings.adblock_enabled {
        adjusted.custom_blocked_hosts.clear();
    }
    FilterEngine::from_settings(&adjusted, config_dir)
}
