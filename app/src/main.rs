//! Integrity Browser desktop shell.
//!
//! Two webviews share one native window:
//! * `chrome` renders the trusted browser UI from `ib://localhost/index.html`;
//! * `content` renders tabs, with the Integrity Protection engine injected as
//!   a document-start script and all traffic routed through the local filter
//!   proxy.
//!
//! Trust boundary: the chrome webview may change settings; the content
//! webview may only report statistics and request keyboard shortcuts. Settings
//! mutations are additionally gated to internal pages even though they are
//! harmless from the new-tab page today.

mod assets;
mod browser;
mod layout;

use std::io::Read;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use integrity_core::proxy::{ProxyServer, ProxyStats};
use integrity_core::settings::{config_dir, Sensitivity, Settings};
use integrity_core::urlutil::{is_allowed_navigation, platform_url, NEWTAB_URL};
use serde_json::Value;
use tao::{
    dpi::LogicalSize,
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Theme, Window, WindowBuilder},
};
use wry::{
    NewWindowResponse, PageLoadEvent, ProxyConfig, ProxyEndpoint, WebView, WebViewBuilder,
};

use browser::{is_settings_url, Browser, MAX_REMOTE_LIST_BYTES, SETTINGS_URL, TEST_PAGE_URL};

#[derive(Debug)]
enum UserEvent {
    ChromeIpc(String),
    ContentIpc(String),
    ContentTitle(String),
    LoadStarted(String),
    LoadFinished(String),
    SameDocumentNavigation(String),
    NewWindow(String),
    RemoteListFetched { url: String, result: Result<String, String> },
    Tick,
}

struct RateLimit {
    window_started: Instant,
    count: u32,
}

impl RateLimit {
    fn new() -> Self {
        Self {
            window_started: Instant::now(),
            count: 0,
        }
    }

    fn allow(&mut self, per_second: u32) -> bool {
        if self.window_started.elapsed() >= Duration::from_secs(1) {
            self.window_started = Instant::now();
            self.count = 0;
        }
        self.count = self.count.saturating_add(1);
        self.count <= per_second
    }
}

struct App {
    window: Window,
    browser: Browser,
    chrome: WebView,
    content: WebView,
    event_proxy: EventLoopProxy<UserEvent>,
    chrome_rate: RateLimit,
    content_rate: RateLimit,
    last_snapshot: String,
}

impl App {
    fn emit_state(&mut self) {
        let snapshot = self.browser.snapshot();
        let Ok(serialized) = serde_json::to_string(&snapshot) else {
            return;
        };
        if serialized == self.last_snapshot {
            return;
        }
        self.last_snapshot = serialized.clone();
        let script = format!(
            "window.__ibUpdate && window.__ibUpdate({serialized});"
        );
        let _ = self.chrome.evaluate_script(&script);

        if self.browser.active_is_internal() {
            let script = format!(
                "window.__ibPageUpdate && window.__ibPageUpdate({serialized});"
            );
            let _ = self.content.evaluate_script(&script);
        }

        if self.browser.active_is_newtab() {
            let state = serde_json::to_string(&self.browser.newtab_snapshot())
                .unwrap_or_else(|_| "{}".into());
            let script = format!("window.__newtabUpdate && window.__newtabUpdate({state});");
            let _ = self.content.evaluate_script(&script);
        }

        self.window.set_title(&self.browser.window_title());
    }

    fn push_content_config(&self) {
        let config = serde_json::to_string(&self.browser.content_config())
            .unwrap_or_else(|_| "{}".into());
        let script = format!(
            "window.__IntegrityBrowser && window.__IntegrityBrowser.configure({config});"
        );
        let _ = self.content.evaluate_script(&script);
    }

    fn toast(&self, message: &str, kind: &str) {
        let message = serde_json::to_string(message).unwrap_or_else(|_| "\"\"".into());
        let kind = serde_json::to_string(kind).unwrap_or_else(|_| "\"info\"".into());
        let script = format!(
            "if (window.__ibPageToast) {{ window.__ibPageToast({message}, {kind}); }} \
             else if (window.__IntegrityBrowser && window.__IntegrityBrowser.toast) {{ \
               window.__IntegrityBrowser.toast({message}, {kind}); \
             }}"
        );
        let _ = self.content.evaluate_script(&script);
    }

    fn focus_address(&self) {
        let _ = self.chrome.focus();
        let _ = self
            .chrome
            .evaluate_script("window.__ibFocusAddress && window.__ibFocusAddress();");
    }

    fn open_settings(&mut self, section: &str) {
        let section: String = section
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(32)
            .collect();
        let url = if section.is_empty() || section == "general" {
            SETTINGS_URL.to_string()
        } else {
            format!("{SETTINGS_URL}#{section}")
        };
        self.load_url(&url);
    }

    fn close_settings(&mut self) {
        if let Some(url) = self.browser.back_target() {
            let _ = self.content.load_url(&platform_url(&url));
            let _ = self.content.focus();
            self.emit_state();
        } else {
            self.load_url(NEWTAB_URL);
        }
    }

    fn load_active_url(&mut self) {
        let url = self.browser.active_url();
        self.browser.mark_loading();
        let _ = self.content.load_url(&platform_url(&url));
        let _ = self.content.focus();
        self.emit_state();
    }

    fn load_url(&mut self, url: &str) {
        if !is_allowed_navigation(url) {
            self.toast("That URL uses a blocked scheme or internal page.", "error");
            return;
        }
        self.browser.mark_loading();
        let _ = self.content.load_url(&platform_url(url));
        let _ = self.content.focus();
        self.emit_state();
    }

    fn navigate_input(&mut self, input: &str) {
        match self.browser.normalize(input) {
            Some(url) => self.load_url(&url),
            None => self.toast("Enter a URL or search term.", "error"),
        }
    }

    fn new_tab(&mut self) {
        self.browser.new_tab();
        self.load_url(NEWTAB_URL);
        self.focus_address();
    }

    fn close_active_tab(&mut self) {
        if self.browser.close_active() {
            self.load_active_url();
            self.push_content_config();
        }
    }

    fn switch_tab(&mut self, id: u64) {
        if self.browser.switch_tab(id).is_some() {
            self.load_active_url();
            self.push_content_config();
        }
    }

    fn back(&mut self) {
        if let Some(url) = self.browser.back_target() {
            let _ = self.content.load_url(&platform_url(&url));
            self.emit_state();
        }
    }

    fn forward(&mut self) {
        if let Some(url) = self.browser.forward_target() {
            let _ = self.content.load_url(&platform_url(&url));
            self.emit_state();
        }
    }

    fn reload(&mut self) {
        self.load_active_url();
    }

    fn stop(&mut self) {
        let _ = self.content.evaluate_script("window.stop();");
        self.browser.mark_loading_done();
        self.emit_state();
    }

    fn settings_changed(&mut self) {
        self.push_content_config();
        self.emit_state();
    }

    fn toggle_from_newtab(&mut self, field: &str, enabled: bool) {
        if !self.browser.active_is_internal() {
            return;
        }
        match field {
            "integrity" => self.browser.toggle_integrity(enabled),
            "adblock" => self.browser.toggle_adblock(enabled),
            "trackers" => self.browser.toggle_trackers(enabled),
            "click_through" => self.browser.toggle_click_through(enabled),
            "block_hidden_tabs" => self.browser.toggle_block_hidden_tabs(enabled),
            _ => return,
        }
        self.settings_changed();
    }

    fn handle_shortcut(&mut self, value: &Value) {
        if value.get("trusted").and_then(Value::as_bool) != Some(true) {
            return;
        }
        let key = value
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let ctrl = value.get("ctrl").and_then(Value::as_bool).unwrap_or(false);
        let alt = value.get("alt").and_then(Value::as_bool).unwrap_or(false);
        let shift = value.get("shift").and_then(Value::as_bool).unwrap_or(false);

        match key.as_str() {
            "l" if ctrl => self.focus_address(),
            "t" if ctrl => self.new_tab(),
            "w" if ctrl => self.close_active_tab(),
            "r" if ctrl => self.reload(),
            "f5" => self.reload(),
            "i" if ctrl && shift => self.content.open_devtools(),
            "f12" => self.content.open_devtools(),
            "arrowleft" if alt => self.back(),
            "arrowright" if alt => self.forward(),
            "," if ctrl => self.open_settings("general"),
            _ => {}
        }
    }

    fn handle_chrome_command(&mut self, body: &str) {
        if body.len() > 16 * 1024 || !self.chrome_rate.allow(200) {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        let command = value.get("type").and_then(Value::as_str).unwrap_or_default();
        match command {
            "navigate" => {
                if let Some(input) = value.get("input").and_then(Value::as_str) {
                    self.navigate_input(input);
                }
            }
            "back" => self.back(),
            "forward" => self.forward(),
            "reload" => self.reload(),
            "stop" => self.stop(),
            "home" => self.load_url(NEWTAB_URL),
            "new_tab" => self.new_tab(),
            "close_tab" => {
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    if self.browser.close_tab(id) {
                        self.load_active_url();
                        self.push_content_config();
                    }
                }
            }
            "switch_tab" => {
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    self.switch_tab(id);
                }
            }
            "focus_address" => self.focus_address(),
            "toggle_integrity" => {
                if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
                    self.browser.toggle_integrity(enabled);
                    self.settings_changed();
                    self.toast(
                        if enabled {
                            "Integrity Protection is on."
                        } else {
                            "Integrity Protection is off."
                        },
                        "info",
                    );
                }
            }
            "toggle_click_through" => {
                if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
                    self.browser.toggle_click_through(enabled);
                    self.settings_changed();
                }
            }
            "toggle_block_hidden_tabs" => {
                if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
                    self.browser.toggle_block_hidden_tabs(enabled);
                    self.settings_changed();
                    self.toast(
                        if enabled {
                            "Hidden new-tab links are blocked."
                        } else {
                            "Hidden new-tab links are allowed."
                        },
                        "info",
                    );
                }
            }
            "set_sensitivity" => {
                let sensitivity = match value
                    .get("sensitivity")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "relaxed" => Some(Sensitivity::Relaxed),
                    "balanced" => Some(Sensitivity::Balanced),
                    "strict" => Some(Sensitivity::Strict),
                    _ => None,
                };
                if let Some(sensitivity) = sensitivity {
                    self.browser.set_sensitivity(sensitivity);
                    self.settings_changed();
                }
            }
            "toggle_site_allowlist" => {
                if let Some(allowed) = value.get("allowed").and_then(Value::as_bool) {
                    self.browser.set_site_allowed(allowed);
                    self.settings_changed();
                    self.toast(
                        if allowed {
                            "Integrity Protection paused for this site."
                        } else {
                            "Integrity Protection resumed for this site."
                        },
                        "info",
                    );
                }
            }
            "toggle_adblock" => {
                if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
                    self.browser.toggle_adblock(enabled);
                    self.settings_changed();
                    self.toast(
                        if enabled {
                            "Ad blocking enabled."
                        } else {
                            "Ad blocking disabled."
                        },
                        "info",
                    );
                }
            }
            "toggle_tracker_blocking" => {
                if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
                    self.browser.toggle_trackers(enabled);
                    self.settings_changed();
                    self.toast(
                        if enabled {
                            "Tracker blocking enabled."
                        } else {
                            "Tracker blocking disabled."
                        },
                        "info",
                    );
                }
            }
            "toggle_filter_list" => {
                let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
                if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
                    if !self.browser.toggle_filter_list(id, enabled) {
                        self.toast("That filter list cannot be toggled.", "error");
                    }
                    self.settings_changed();
                }
            }
            "remove_filter_list" => {
                let id = value.get("id").and_then(Value::as_str).unwrap_or_default();
                if self.browser.remove_filter_list(id) {
                    self.settings_changed();
                    self.toast("Filter list removed.", "info");
                }
            }
            "add_custom_host" => {
                if let Some(host) = value.get("host").and_then(Value::as_str) {
                    match self.browser.add_custom_host(host) {
                        Ok(host) => {
                            self.settings_changed();
                            self.toast(&format!("Blocked host added: {host}"), "success");
                        }
                        Err(err) => self.toast(&err, "error"),
                    }
                }
            }
            "remove_custom_host" => {
                if let Some(host) = value.get("host").and_then(Value::as_str) {
                    if self.browser.remove_custom_host(host) {
                        self.settings_changed();
                    }
                }
            }
            "add_remote_list" => {
                if let Some(url) = value.get("url").and_then(Value::as_str) {
                    match browser::validate_list_url(url) {
                        Ok(url) => self.fetch_remote_list(url),
                        Err(err) => self.toast(&err, "error"),
                    }
                }
            }
            "refresh_filter_lists" => {
                let urls = self.browser.remote_list_urls();
                if urls.is_empty() {
                    self.toast("No remote filter lists to refresh.", "info");
                } else {
                    for url in urls {
                        self.fetch_remote_list(url);
                    }
                }
            }
            "reset_stats" => {
                self.browser.stats.reset();
                self.browser.page_stats = browser::PageIntegrityStats::default();
                self.emit_state();
                self.toast("Statistics reset.", "info");
            }
            "open_settings" => {
                let section = value
                    .get("section")
                    .and_then(Value::as_str)
                    .unwrap_or("general");
                self.open_settings(section);
            }
            "close_settings" => self.close_settings(),
            "devtools" => self.content.open_devtools(),
            "open_test_page" => self.load_url(TEST_PAGE_URL),
            "request_state" | "ready" => self.emit_state(),
            _ => {}
        }
    }

    fn fetch_remote_list(&self, url: String) {
        let proxy = self.event_proxy.clone();
        self.toast("Downloading filter list...", "info");
        std::thread::spawn(move || {
            let result = fetch_filter_list(&url);
            let _ = proxy.send_event(UserEvent::RemoteListFetched { url, result });
        });
    }

    fn handle_content_ipc(&mut self, body: &str) {
        if body.len() > 16 * 1024 || !self.content_rate.allow(120) {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return;
        };
        let kind = value.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "integrity" => {
                let event = value.get("event").and_then(Value::as_str).unwrap_or_default();
                match event {
                    "config_request" => self.push_content_config(),
                    "stats" => {
                        let host = value
                            .get("host")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        let active_host = self.browser.active_host().to_ascii_lowercase();
                        if !active_host.is_empty() && host != active_host {
                            return;
                        }
                        self.browser.page_stats.scanned = clamped(&value, "scanned");
                        self.browser.page_stats.suspicious = clamped(&value, "suspicious");
                        self.browser.page_stats.neutralized = clamped(&value, "neutralized");
                        self.browser.page_stats.click_throughs =
                            clamped(&value, "clickThroughs");
                        self.browser.page_stats.hidden_tabs_blocked =
                            clamped(&value, "hiddenTabsBlocked");
                        self.browser.page_stats.host = host;
                        self.emit_state();
                    }
                    "click_through" => {
                        self.browser.page_stats.last_action =
                            "passed a click through an overlay".into();
                        self.emit_state();
                    }
                    "neutralized" => {
                        self.browser.page_stats.last_action =
                            "neutralized a suspicious overlay".into();
                        self.emit_state();
                    }
                    "hidden_tab_blocked" => {
                        self.browser.page_stats.last_action =
                            "blocked a hidden link that opens a new tab".into();
                        self.emit_state();
                    }
                    _ => {}
                }
            }
            "shortcut" => self.handle_shortcut(&value),
            "navigate" if self.browser.active_is_internal() => {
                if let Some(input) = value.get("input").and_then(Value::as_str) {
                    self.navigate_input(input);
                }
            }
            "toggle" if self.browser.active_is_internal() => {
                let field = value.get("field").and_then(Value::as_str).unwrap_or_default();
                if let Some(enabled) = value.get("enabled").and_then(Value::as_bool) {
                    self.toggle_from_newtab(field, enabled);
                }
            }
            "newtab_state_request" | "refresh_state" if self.browser.active_is_internal() => {
                self.emit_state()
            }
            "open_settings" if self.browser.active_is_internal() => {
                let section = value
                    .get("section")
                    .and_then(Value::as_str)
                    .unwrap_or("general");
                self.open_settings(section);
            }
            "close_settings" if is_settings_url(&self.browser.active_url()) => {
                self.close_settings()
            }
            "open_test_page" if self.browser.active_is_internal() => {
                self.load_url(TEST_PAGE_URL)
            }
            // Trusted settings page: forward the full command set it needs.
            // Ordinary web pages never reach this arm.
            "toggle_integrity" | "toggle_click_through" | "toggle_block_hidden_tabs"
            | "set_sensitivity" | "toggle_site_allowlist" | "toggle_adblock"
            | "toggle_tracker_blocking"
            | "toggle_filter_list" | "remove_filter_list" | "add_custom_host"
            | "remove_custom_host" | "add_remote_list" | "refresh_filter_lists"
            | "reset_stats"
                if self.browser.active_is_internal() =>
            {
                self.handle_chrome_command(body);
            }
            _ => {}
        }
    }
}

fn clamped(value: &Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(1_000_000)
}

fn fetch_filter_list(url: &str) -> Result<String, String> {
    let response = ureq::get(url)
        .set("User-Agent", "IntegrityBrowser/0.1 filter-list")
        .timeout(Duration::from_secs(25))
        .call()
        .map_err(|err| format!("Download failed: {err}"))?;
    if response.status() != 200 {
        return Err(format!("Server returned HTTP {}", response.status()));
    }
    let mut text = String::new();
    response
        .into_reader()
        .take((MAX_REMOTE_LIST_BYTES + 1) as u64)
        .read_to_string(&mut text)
        .map_err(|err| format!("Could not read list: {err}"))?;
    if text.len() > MAX_REMOTE_LIST_BYTES {
        return Err("Filter list is larger than 8 MiB".into());
    }
    if text.trim().is_empty() {
        return Err("Filter list is empty".into());
    }
    Ok(text)
}

fn main() -> wry::Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let event_proxy = event_loop.create_proxy();

    let mut warnings = Vec::new();
    let (settings, mut load_warnings) = Settings::load();
    warnings.append(&mut load_warnings);
    let config_dir = config_dir();
    if let Err(err) = std::fs::create_dir_all(&config_dir) {
        warnings.push(format!(
            "Could not create config directory {}: {err}",
            config_dir.display()
        ));
    }
    let (engine, mut engine_warnings) = browser::build_engine(&settings, &config_dir);
    warnings.append(&mut engine_warnings);

    let engine = Arc::new(RwLock::new(Arc::new(engine)));
    let stats = Arc::new(ProxyStats::new());
    let proxy = ProxyServer::start_with_stats(engine.clone(), stats.clone())?;

    let window = WindowBuilder::new()
        .with_title("Integrity Browser")
        .with_inner_size(LogicalSize::new(1280.0, 820.0))
        .with_min_inner_size(LogicalSize::new(900.0, 600.0))
        .with_theme(Some(Theme::Dark))
        .build(&event_loop)
        .expect("failed to create the application window");

    let proxy_endpoint = ProxyEndpoint {
        host: "127.0.0.1".into(),
        port: proxy.port().to_string(),
    };

    let chrome_proxy = event_proxy.clone();
    let chrome_builder = WebViewBuilder::new()
        .with_url(platform_url("ib://localhost/index.html"))
        .with_custom_protocol("ib".into(), |_id, request| assets::respond(&request))
        .with_proxy_config(ProxyConfig::Http(proxy_endpoint.clone()))
        .with_background_color((20, 22, 27, 255))
        .with_ipc_handler(move |request| {
            let _ = chrome_proxy.send_event(UserEvent::ChromeIpc(request.body().clone()));
        });

    let content_proxy = event_proxy.clone();
    let title_proxy = event_proxy.clone();
    let nav_proxy = event_proxy.clone();
    let load_proxy = event_proxy.clone();
    let window_proxy = event_proxy.clone();
    let engine_script = format!(
        "/* Integrity Protection engine (document start) */\n{}",
        include_str!("../frontend/engine.js")
    );

    let content_builder = WebViewBuilder::new()
        .with_url(platform_url(NEWTAB_URL))
        .with_custom_protocol("ib".into(), |_id, request| assets::respond(&request))
        .with_initialization_script_for_main_only(engine_script, true)
        .with_proxy_config(ProxyConfig::Http(proxy_endpoint))
        .with_background_color((14, 15, 19, 255))
        .with_navigation_handler(move |url| {
            let allowed = is_allowed_navigation(&url);
            if allowed {
                let _ = nav_proxy.send_event(UserEvent::SameDocumentNavigation(url));
            }
            allowed
        })
        .with_new_window_req_handler(move |url, _features| {
            let _ = window_proxy.send_event(UserEvent::NewWindow(url));
            NewWindowResponse::Deny
        })
        .with_document_title_changed_handler(move |title| {
            let _ = title_proxy.send_event(UserEvent::ContentTitle(title));
        })
        .with_on_page_load_handler(move |event, url| match event {
            PageLoadEvent::Started => {
                let _ = load_proxy.send_event(UserEvent::LoadStarted(url));
            }
            PageLoadEvent::Finished => {
                let _ = load_proxy.send_event(UserEvent::LoadFinished(url));
            }
        })
        .with_ipc_handler(move |request| {
            let _ = content_proxy.send_event(UserEvent::ContentIpc(request.body().clone()));
        });

    let (chrome, content) = layout::build_webviews(&window, chrome_builder, content_builder)?;

    let browser = Browser::new(
        settings,
        config_dir,
        engine,
        stats,
        proxy,
        warnings,
    );
    let mut app = App {
        window,
        browser,
        chrome,
        content,
        event_proxy: event_proxy.clone(),
        chrome_rate: RateLimit::new(),
        content_rate: RateLimit::new(),
        last_snapshot: String::new(),
    };

    // Kick out the first state as soon as the chrome joins the event loop;
    // `ready` from chrome.js does the same thing after its DOM exists.
    let tick_proxy = event_proxy;
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        if tick_proxy.send_event(UserEvent::Tick).is_err() {
            break;
        }
    });

    let mut next_tick = Instant::now() + Duration::from_secs(1);
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(next_tick);
        match event {
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                app.emit_state();
                next_tick = Instant::now() + Duration::from_secs(1);
            }
            Event::UserEvent(UserEvent::ChromeIpc(body)) => {
                app.handle_chrome_command(&body);
            }
            Event::UserEvent(UserEvent::ContentIpc(body)) => {
                app.handle_content_ipc(&body);
            }
            Event::UserEvent(UserEvent::ContentTitle(title)) => {
                app.browser.set_title(title);
                app.emit_state();
            }
            Event::UserEvent(UserEvent::LoadStarted(url)) => {
                app.browser.on_load_started(&url);
                app.emit_state();
            }
            Event::UserEvent(UserEvent::LoadFinished(url)) => {
                app.browser.on_load_finished(&url);
                app.emit_state();
                app.push_content_config();
            }
            Event::UserEvent(UserEvent::SameDocumentNavigation(url)) => {
                if app.browser.on_same_document_navigation(&url) {
                    app.emit_state();
                }
            }
            Event::UserEvent(UserEvent::NewWindow(url)) => {
                if is_allowed_navigation(&url) {
                    app.browser.new_tab();
                    app.load_url(&url);
                } else {
                    app.toast("Blocked a popup with an unsafe URL.", "error");
                }
            }
            Event::UserEvent(UserEvent::RemoteListFetched { url, result }) => match result {
                Ok(text) => match app.browser.install_remote_list(&url, &text) {
                    Ok(count) => {
                        app.browser.last_remote_note =
                            format!("Filter list loaded: {count} rules from {url}");
                        app.settings_changed();
                        app.toast(&format!("Filter list loaded ({count} rules)."), "success");
                    }
                    Err(err) => {
                        app.browser.last_remote_note = format!("Filter list error: {err}");
                        app.emit_state();
                        app.toast(&err, "error");
                    }
                },
                Err(err) => {
                    app.browser.last_remote_note = format!("Filter list error: {err}");
                    app.emit_state();
                    app.toast(&err, "error");
                }
            },
            Event::UserEvent(UserEvent::Tick) => app.emit_state(),
            Event::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            } => layout::resize(&app.window, &app.chrome, &app.content),
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            Event::LoopDestroyed => {
                app.browser.proxy.shutdown();
            }
            _ => {}
        }
    });
}
