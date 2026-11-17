//! URL parsing, search normalisation and navigation policy.

use url::Url;

pub const NEWTAB_URL: &str = "ib://localhost/newtab.html";
pub const DUCKDUCKGO_HOME: &str = "https://duckduckgo.com/";

/// Maximum length we will accept from the address bar.
const MAX_INPUT_LEN: usize = 8192;

/// Turn whatever the user typed into a navigable URL.
///
/// Returns `None` for an empty input. Anything that does not look like a URL
/// becomes a DuckDuckGo search, which is intentional: never pass unknown
/// schemes (`javascript:`, `file:`, `data:`) through to the webview.
pub fn normalize_user_input(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let trimmed = if trimmed.len() > MAX_INPUT_LEN {
        &trimmed[..MAX_INPUT_LEN]
    } else {
        trimmed
    };

    if let Ok(url) = Url::parse(trimmed) {
        match url.scheme() {
            "http" | "https" => return Some(url.to_string()),
            "ib" if is_internal_url(url.as_str()) => return Some(url.to_string()),
            _ => {}
        }
    }

    if trimmed.contains(char::is_whitespace) {
        return Some(search_url(trimmed));
    }

    // `Url::parse` reads `localhost:3000` as the scheme `localhost`. Treat a
    // `scheme:` prefix as host:port only when the prefix itself looks like a
    // host and the remainder looks like a numeric port. Everything else with
    // an explicit scheme is a search query, never a navigable URL.
    if let Some((prefix, rest)) = trimmed.split_once(':') {
        let host_prefix = prefix == "localhost"
            || prefix.contains('.')
            || prefix.starts_with('[')
            || prefix.starts_with("0x");
        let token = rest.split(['/', '?', '#']).next().unwrap_or("");
        let port_like =
            !token.is_empty() && token.chars().all(|c| c.is_ascii_digit());
        if !(host_prefix && port_like) {
            return Some(search_url(trimmed));
        }
    }

    let authority = trimmed.split(['/', '?', '#']).next().unwrap_or(trimmed);
    let host_like = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);

    let (host_part, port) = match host_like.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() => (host, Some(port)),
        _ => (host_like, None),
    };
    let has_port = port
        .map(|p| p.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false);

    let looks_like_host = if host_like.starts_with('[') {
        true
    } else if has_port {
        host_part == "localhost" || host_part.contains('.')
    } else {
        host_like == "localhost" || host_like.contains('.')
    };

    if looks_like_host {
        let candidate = format!("https://{trimmed}");
        if let Ok(url) = Url::parse(&candidate) {
            if url.host_str().is_some() {
                return Some(url.to_string());
            }
        }
    }

    Some(search_url(trimmed))
}

/// Origin the platform webview uses for the embedded assets.
///
/// wry registers custom protocols as `ib://localhost` on Linux/macOS, but
/// exposes them as `http://ib.localhost` on Windows. Initial `with_url` loads
/// are rewritten for us; later programmatic navigations must use the platform
/// origin directly or WebView2 leaves the previous document in place.
pub fn platform_url(url: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        if let Some(rest) = url.strip_prefix("ib://localhost") {
            return format!("http://ib.localhost{rest}");
        }
    }
    url.to_string()
}

pub fn search_url(query: &str) -> String {
    // `byte_serialize` follows form encoding (spaces become `+`). Browsers and
    // DuckDuckGo accept both, but `%20` is the canonical query encoding and is
    // easier to assert in tests. Literal `+` is already encoded as `%2B`.
    let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    format!("https://duckduckgo.com/?q={}", encoded.replace('+', "%20"))
}

/// Internal pages are served by the embedded asset handler. On Linux the
/// custom scheme is `ib://localhost`; WebKitGTK/WebView2 may expose it as
/// `http://ib.localhost` (notably on Windows), so accept both.
pub fn is_internal_url(raw: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    let internal_origin = match url.scheme() {
        "ib" => true,
        "http" | "https" => matches!(url.host_str(), Some("ib.localhost")),
        _ => false,
    };
    internal_origin
        && matches!(
            url.path(),
            "/" | "/newtab.html" | "/assets/logo.svg" | "/favicon.ico"
                | "/tests/integrity-harness.html" | "/settings.html"
        )
}

pub fn is_allowed_navigation(raw: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    match url.scheme() {
        "http" | "https" => true,
        "ib" => is_internal_url(raw),
        "about" => url.as_str() == "about:blank",
        _ => false,
    }
}

pub fn host_of(raw: &str) -> Option<String> {
    Url::parse(raw)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
}

pub fn serialized_host(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    match url.port() {
        Some(port) => Some(format!("{host}:{port}")),
        None => Some(host),
    }
}

/// True when two URLs differ only in fragment.
pub fn same_document(a: &str, b: &str) -> bool {
    let (Ok(mut ua), Ok(mut ub)) = (Url::parse(a), Url::parse(b)) else {
        return false;
    };
    ua.set_fragment(None);
    ub.set_fragment(None);
    ua == ub
}

pub fn is_https(raw: &str) -> bool {
    Url::parse(raw).map(|u| u.scheme() == "https").unwrap_or(false)
}

pub fn display_host(raw: &str) -> String {
    host_of(raw)
        .map(|h| h.strip_prefix("www.").unwrap_or(&h).to_string())
        .unwrap_or_else(|| raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_queries_into_duckduckgo() {
        assert_eq!(
            normalize_user_input("privacy browser").unwrap(),
            "https://duckduckgo.com/?q=privacy%20browser"
        );
        assert!(normalize_user_input("hello").unwrap().starts_with("https://duckduckgo.com/?q="));
    }

    #[test]
    fn keeps_http_and_https_urls() {
        assert_eq!(
            normalize_user_input("https://example.com/a?b=1").unwrap(),
            "https://example.com/a?b=1"
        );
        assert_eq!(
            normalize_user_input("http://example.com").unwrap(),
            "http://example.com/"
        );
    }

    #[test]
    fn adds_https_to_hosts() {
        assert_eq!(
            normalize_user_input("example.com").unwrap(),
            "https://example.com/"
        );
        assert_eq!(
            normalize_user_input("localhost:3000").unwrap(),
            "https://localhost:3000/"
        );
        assert_eq!(
            normalize_user_input("192.168.0.1:8080/x").unwrap(),
            "https://192.168.0.1:8080/x"
        );
    }

    #[test]
    fn rejects_dangerous_schemes_as_search() {
        let u = normalize_user_input("javascript:alert(1)").unwrap();
        assert!(u.starts_with("https://duckduckgo.com/?q="));
        let u = normalize_user_input("file:///etc/passwd").unwrap();
        assert!(u.starts_with("https://duckduckgo.com/?q="));
    }

    #[test]
    fn internal_urls_are_recognised_on_both_origins() {
        assert!(is_internal_url("ib://localhost/newtab.html"));
        assert!(is_internal_url("http://ib.localhost/newtab.html"));
        assert!(!is_internal_url("ib://localhost/index.html"));
        assert!(!is_internal_url("https://example.com/newtab.html"));
    }

    #[test]
    fn platform_url_keeps_external_urls_unchanged() {
        assert_eq!(platform_url("https://example.com/"), "https://example.com/");
        assert_eq!(
            platform_url("http://127.0.0.1:8124/"),
            "http://127.0.0.1:8124/"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn platform_url_maps_internal_origin_on_windows() {
        assert_eq!(
            platform_url("ib://localhost/newtab.html"),
            "http://ib.localhost/newtab.html"
        );
        assert_eq!(
            platform_url("ib://localhost/settings.html#privacy"),
            "http://ib.localhost/settings.html#privacy"
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn platform_url_keeps_internal_origin_off_windows() {
        assert_eq!(
            platform_url("ib://localhost/newtab.html"),
            "ib://localhost/newtab.html"
        );
    }

    #[test]
    fn navigation_policy_blocks_dangerous_schemes() {
        assert!(is_allowed_navigation("https://example.com"));
        assert!(is_allowed_navigation("http://example.com"));
        assert!(is_allowed_navigation("ib://localhost/newtab.html"));
        assert!(is_allowed_navigation("about:blank"));
        assert!(!is_allowed_navigation("file:///etc/passwd"));
        assert!(!is_allowed_navigation("javascript:alert(1)"));
        assert!(!is_allowed_navigation("data:text/html,hi"));
        assert!(!is_allowed_navigation("blob:https://evil.example/x"));
    }

    #[test]
    fn fragment_only_changes_are_same_document() {
        assert!(same_document("https://a.example/x", "https://a.example/x#top"));
        assert!(!same_document("https://a.example/x", "https://a.example/y"));
    }

    #[test]
    fn host_display_strips_www() {
        assert_eq!(display_host("https://www.example.com/a"), "example.com");
    }
}
