//! Host-based filter engine.
//!
//! Integrity Browser blocks at the connection level (inside its local proxy).
//! It therefore only needs host-level rules: `||ads.example.com^`, hosts-file
//! entries and plain host names. Path- and option-specific rules are counted
//! and skipped rather than silently over-blocking.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::settings::{FilterListKind, Settings};

pub const BUILTIN_ADS: &str = include_str!("../../../filters/ads.txt");
pub const BUILTIN_TRACKERS: &str = include_str!("../../../filters/trackers.txt");

/// Category used for statistics and UI grouping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Ads,
    Trackers,
    Custom,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Ads => "ads",
            Category::Trackers => "trackers",
            Category::Custom => "custom",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FilterParseStats {
    pub parsed_rules: usize,
    pub skipped_rules: usize,
    pub exception_rules: usize,
}

impl FilterParseStats {
    fn merge(&mut self, other: &FilterParseStats) {
        self.parsed_rules += other.parsed_rules;
        self.skipped_rules += other.skipped_rules;
        self.exception_rules += other.exception_rules;
    }
}

/// Immutable filter snapshot. Replaced atomically when settings change.
#[derive(Clone, Debug, Default)]
pub struct FilterEngine {
    ads: HashSet<String>,
    trackers: HashSet<String>,
    custom: HashSet<String>,
    exceptions: HashSet<String>,
    /// Rules containing `*` in the host portion.
    glob_rules: Vec<(String, Category)>,
    glob_exceptions: Vec<String>,
    pub stats: FilterParseStats,
}

impl FilterEngine {
    pub fn from_lists(lists: &[(String, Category, String)]) -> Self {
        let mut engine = Self::default();
        for (text, category, name) in lists {
            let mut stats = FilterParseStats::default();
            engine.parse(text, *category, &mut stats);
            if stats.skipped_rules > 0 && *category == Category::Custom {
                eprintln!(
                    "[filters] {name}: {} host rule(s) loaded, {} non-host rule(s) skipped",
                    stats.parsed_rules, stats.skipped_rules
                );
            }
            engine.stats.merge(&stats);
        }
        engine
    }

    /// Build the engine from persisted settings and cached custom lists.
    /// Returns human-readable warnings for unreadable list files.
    pub fn from_settings(settings: &Settings, config_dir: &Path) -> (Self, Vec<String>) {
        let mut lists: Vec<(String, Category, String)> = Vec::new();
        let mut warnings = Vec::new();

        for list in &settings.filter_lists {
            if !list.enabled {
                continue;
            }
            match list.kind {
                FilterListKind::BuiltinAds => {
                    lists.push((BUILTIN_ADS.to_string(), Category::Ads, list.name.clone()));
                }
                FilterListKind::BuiltinTrackers => {
                    lists.push((
                        BUILTIN_TRACKERS.to_string(),
                        Category::Trackers,
                        list.name.clone(),
                    ));
                }
                FilterListKind::Remote => {
                    let Some(rel) = list.cached_file.as_ref() else {
                        warnings.push(format!("List '{}' has no cached file", list.name));
                        continue;
                    };
                    let path = config_dir.join(rel);
                    match std::fs::read_to_string(&path) {
                        Ok(text) => lists.push((text, Category::Custom, list.name.clone())),
                        Err(err) => warnings.push(format!(
                            "Could not read filter list '{}': {err}",
                            list.name
                        )),
                    }
                }
            }
        }

        if !settings.custom_blocked_hosts.is_empty() {
            let mut text = String::from("! User custom blocked hosts\n");
            for host in &settings.custom_blocked_hosts {
                text.push_str(host);
                text.push('\n');
            }
            lists.push((text, Category::Custom, "Custom blocked hosts".into()));
        }

        let engine = Self::from_lists(&lists);
        (engine, warnings)
    }

    fn parse(&mut self, text: &str, category: Category, stats: &mut FilterParseStats) {
        for raw in text.lines() {
            let line = raw.trim().trim_start_matches('\u{feff}');
            if line.is_empty() || line.starts_with('!') || line.starts_with('[') {
                continue;
            }
            // Hosts-file comments and cosmetic/scriptlet filter syntax.
            if line.starts_with("##") || line.contains("##") || line.contains("#@#") {
                stats.skipped_rules += 1;
                continue;
            }
            if line.starts_with('#') {
                continue;
            }

            let (is_exception, body) = match line.strip_prefix("@@") {
                Some(rest) => (true, rest.trim()),
                None => (false, line),
            };

            // Hosts-file syntax: "0.0.0.0 example.com" / "127.0.0.1 example.com".
            let body = if body.starts_with("0.0.0.0")
                || body.starts_with("127.0.0.1")
                || body.starts_with("::1")
            {
                body.split_whitespace().nth(1).unwrap_or("")
            } else {
                body
            };

            let Some(host_rule) = extract_host_rule(body) else {
                if is_exception {
                    stats.skipped_rules += 1;
                } else {
                    stats.skipped_rules += 1;
                }
                continue;
            };

            if is_exception {
                stats.exception_rules += 1;
                self.add_exception(host_rule);
            } else {
                stats.parsed_rules += 1;
                self.add_rule(host_rule, category);
            }
        }
    }

    fn add_rule(&mut self, rule: String, category: Category) {
        if rule.contains('*') {
            self.glob_rules.push((rule, category));
            return;
        }
        match category {
            Category::Ads => {
                self.ads.insert(rule);
            }
            Category::Trackers => {
                self.trackers.insert(rule);
            }
            Category::Custom => {
                self.custom.insert(rule);
            }
        }
    }

    fn add_exception(&mut self, rule: String) {
        if rule.contains('*') {
            self.glob_exceptions.push(rule);
        } else {
            self.exceptions.insert(rule);
        }
    }

    /// Returns the matching category if this host should be blocked.
    pub fn decision(&self, host: &str) -> Option<Category> {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() || is_local_host(&host) {
            return None;
        }

        if any_suffix_matches(&self.exceptions, &host)
            || self
                .glob_exceptions
                .iter()
                .any(|g| glob_matches(g, &host))
        {
            return None;
        }

        if any_suffix_matches(&self.ads, &host) {
            return Some(Category::Ads);
        }
        if any_suffix_matches(&self.trackers, &host) {
            return Some(Category::Trackers);
        }
        if any_suffix_matches(&self.custom, &host) {
            return Some(Category::Custom);
        }
        for (glob, category) in &self.glob_rules {
            if glob_matches(glob, &host) {
                return Some(*category);
            }
        }
        None
    }

    pub fn is_blocked(&self, host: &str) -> bool {
        self.decision(host).is_some()
    }

    pub fn rule_count(&self) -> usize {
        self.ads.len()
            + self.trackers.len()
            + self.custom.len()
            + self.glob_rules.len()
            + self.exceptions.len()
            + self.glob_exceptions.len()
    }

    pub fn counts_by_category(&self) -> HashMap<&'static str, usize> {
        let mut map = HashMap::new();
        map.insert("ads", self.ads.len());
        map.insert("trackers", self.trackers.len());
        map.insert("custom", self.custom.len() + self.glob_rules.len());
        map
    }
}

fn is_local_host(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "127.0.0.1"
        || host == "::1"
        || host == "[::1]"
        || host == "0.0.0.0"
}

fn any_suffix_matches(set: &HashSet<String>, host: &str) -> bool {
    let mut current = host;
    loop {
        if set.contains(current) {
            return true;
        }
        match current.find('.') {
            Some(index) => current = &current[index + 1..],
            None => return false,
        }
    }
}

fn glob_matches(pattern: &str, host: &str) -> bool {
    // Tiny wildcard matcher (`*` only), sufficient for host rules.
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == host || host.ends_with(&format!(".{pattern}"));
    }
    let mut rest = host;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !rest.starts_with(part) {
                return false;
            }
            rest = &rest[part.len()..];
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else if let Some(idx) = rest.find(part) {
            rest = &rest[idx + part.len()..];
        } else {
            return false;
        }
    }
    true
}

/// Accept `||host^`, `||host^$options`, `host`, `*.host`. Reject anything with
/// a path, port, scheme or unsupported option that could over-block.
fn extract_host_rule(rule: &str) -> Option<String> {
    let mut body = rule.trim();
    if body.is_empty() {
        return None;
    }
    if let Some(rest) = body.strip_prefix("||") {
        body = rest;
    } else if body.contains("://") {
        return None;
    }

    if let Some(caret) = body.find('^') {
        let after = &body[caret + 1..];
        if !(after.is_empty() || after.starts_with('$')) {
            return None;
        }
        // Resource-type options (`$image`, `$script`, ...) cannot be honoured
        // by a connection-level blocker without over-blocking. Skip them;
        // generic options such as `$third-party` are still safe to treat as
        // whole-host rules.
        if let Some(options) = after.strip_prefix('$') {
            if options
                .split(',')
                .any(|option| is_resource_type_option(option))
            {
                return None;
            }
        }
        body = &body[..caret];
    }

    // Reject path/option rules unless the only thing after the host is a
    // caret (handled above). This intentionally favours under-blocking.
    if body.contains('/') || body.contains('$') || body.contains('?') || body.contains(':') {
        return None;
    }

    let mut host = body
        .trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .to_string();
    host = host
        .trim_start_matches("*.")
        .trim_start_matches('.')
        .to_string();
    host = host.trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.len() > 253 {
        return None;
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '*')
    {
        return None;
    }
    if !host.contains('.') && host != "localhost" {
        return None;
    }
    // Reject an all-numeric host that is not an IP-like token (guards against
    // accidental tiny filter lists blocking broad suffixes).
    if host.chars().all(|c| c.is_ascii_digit() || c == '.') && host.matches('.').count() != 3 {
        return None;
    }
    Some(host)
}

/// Resource-type options narrow a rule to one request type. The connection
/// proxy cannot see request types, so rules carrying them are skipped instead
/// of being widened into a whole-host block.
fn is_resource_type_option(option: &str) -> bool {
    let name = option
        .trim()
        .trim_start_matches('~')
        .split('=')
        .next()
        .unwrap_or("")
        .trim();
    matches!(
        name,
        "image"
            | "images"
            | "script"
            | "stylesheet"
            | "css"
            | "font"
            | "media"
            | "object"
            | "object-subrequest"
            | "subdocument"
            | "document"
            | "xmlhttprequest"
            | "xhr"
            | "websocket"
            | "ping"
            | "popup"
            | "other"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(text: &str, category: Category) -> FilterEngine {
        FilterEngine::from_lists(&[(text.to_string(), category, "test".into())])
    }

    #[test]
    fn blocks_exact_and_subdomains() {
        let e = engine("||doubleclick.net^\n", Category::Ads);
        assert!(e.is_blocked("doubleclick.net"));
        assert!(e.is_blocked("securepubads.g.doubleclick.net"));
        assert!(!e.is_blocked("notdoubleclick.net"));
        assert!(!e.is_blocked("doubleclick.net.example.com"));
    }

    #[test]
    fn exceptions_win() {
        let e = engine(
            "||ads.example.com^\n@@||safe.ads.example.com^\n",
            Category::Ads,
        );
        assert!(e.is_blocked("ads.example.com"));
        assert!(e.is_blocked("x.ads.example.com"));
        assert!(!e.is_blocked("safe.ads.example.com"));
    }

    #[test]
    fn supports_hosts_file_and_plain_hosts() {
        let e = engine(
            "0.0.0.0 tracker.example\n127.0.0.1 metrics.example.org\nplain.example.net\n",
            Category::Trackers,
        );
        assert!(e.is_blocked("tracker.example"));
        assert!(e.is_blocked("cdn.metrics.example.org"));
        assert!(e.is_blocked("plain.example.net"));
    }

    #[test]
    fn skips_path_and_cosmetic_rules() {
        let e = engine(
            "||example.com/ads/*\n||example.com^$image\n##.ad-banner\nexample##.banner\n/ads[0-9]+/\n",
            Category::Ads,
        );
        assert_eq!(e.stats.parsed_rules, 0);
        assert!(e.stats.skipped_rules >= 5);
        assert!(!e.is_blocked("example.com"));
    }

    #[test]
    fn options_without_path_are_accepted_as_host_rule() {
        let e = engine("||ads.example.com^$third-party\n", Category::Ads);
        assert!(e.is_blocked("ads.example.com"));
    }

    #[test]
    fn wildcard_rules() {
        let e = engine("||*.adnet.example^\n", Category::Ads);
        assert!(e.is_blocked("x.adnet.example"));
        assert!(e.is_blocked("a.b.adnet.example"));
    }

    #[test]
    fn local_hosts_never_blocked() {
        let e = engine("||localhost^\n||127.0.0.1^\n", Category::Ads);
        assert!(!e.is_blocked("localhost"));
        assert!(!e.is_blocked("127.0.0.1"));
    }

    #[test]
    fn builtin_lists_load_and_block_known_hosts() {
        let e = FilterEngine::from_lists(&[
            (BUILTIN_ADS.to_string(), Category::Ads, "ads".into()),
            (
                BUILTIN_TRACKERS.to_string(),
                Category::Trackers,
                "trackers".into(),
            ),
        ]);
        assert!(e.rule_count() > 30, "builtin lists look empty");
        assert_eq!(e.decision("doubleclick.net"), Some(Category::Ads));
        assert_eq!(
            e.decision("www.google-analytics.com"),
            Some(Category::Trackers)
        );
        assert_eq!(e.decision("example.com"), None);
        assert_eq!(e.decision("cdn.jsdelivr.net"), None);
    }
}
