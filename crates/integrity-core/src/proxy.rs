//! Local HTTP filtering proxy.
//!
//! The webview is configured to use this proxy for all requests. HTTPS is
//! tunnelled (no MITM); only the destination hostname is visible, which is
//! exactly what host-list ad/tracker blocking needs.
//!
//! Security notes:
//! * binds to 127.0.0.1 on an ephemeral port only;
//! * never forwards proxy credentials;
//! * localhost destinations are never blocked;
//! * upstream traffic is plain TCP relay after the host decision.

use crate::filters::{Category, FilterEngine};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

const MAX_HEADER_BYTES: usize = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const HEADER_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RECENT: usize = 40;

#[derive(Clone, Debug, Serialize)]
pub struct BlockedRequest {
    pub host: String,
    pub category: String,
    pub at_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct BlockedHost {
    pub host: String,
    pub count: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProxyStatsSnapshot {
    pub blocked: u64,
    pub inspected: u64,
    pub ads: u64,
    pub trackers: u64,
    pub custom: u64,
    pub recent: Vec<BlockedRequest>,
    pub top_hosts: Vec<BlockedHost>,
    pub enabled: bool,
    pub started_at_ms: u64,
}

#[derive(Default)]
struct StatsInner {
    blocked: u64,
    ads: u64,
    trackers: u64,
    custom: u64,
    recent: VecDeque<BlockedRequest>,
    hosts: HashMap<String, u64>,
    started_at_ms: u64,
}

pub struct ProxyStats {
    inspected: AtomicU64,
    enabled: AtomicBool,
    inner: Mutex<StatsInner>,
}

impl Default for ProxyStats {
    fn default() -> Self {
        Self::new()
    }
}

impl ProxyStats {
    pub fn new() -> Self {
        Self {
            inspected: AtomicU64::new(0),
            enabled: AtomicBool::new(true),
            inner: Mutex::new(StatsInner {
                started_at_ms: now_ms(),
                ..Default::default()
            }),
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn record_inspected(&self) {
        self.inspected.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_block(&self, host: &str, category: Category) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.blocked += 1;
        match category {
            Category::Ads => inner.ads += 1,
            Category::Trackers => inner.trackers += 1,
            Category::Custom => inner.custom += 1,
        }
        let host = host.to_ascii_lowercase();
        *inner.hosts.entry(host.clone()).or_insert(0) += 1;
        inner.recent.push_front(BlockedRequest {
            host,
            category: category.as_str().to_string(),
            at_ms: now_ms(),
        });
        while inner.recent.len() > MAX_RECENT {
            inner.recent.pop_back();
        }
    }

    pub fn reset(&self) {
        self.inspected.store(0, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.blocked = 0;
        inner.ads = 0;
        inner.trackers = 0;
        inner.custom = 0;
        inner.recent.clear();
        inner.hosts.clear();
        inner.started_at_ms = now_ms();
    }

    pub fn snapshot(&self) -> ProxyStatsSnapshot {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut top: Vec<BlockedHost> = inner
            .hosts
            .iter()
            .map(|(host, count)| BlockedHost {
                host: host.clone(),
                count: *count,
            })
            .collect();
        top.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.host.cmp(&b.host)));
        top.truncate(8);
        ProxyStatsSnapshot {
            blocked: inner.blocked,
            inspected: self.inspected.load(Ordering::Relaxed),
            ads: inner.ads,
            trackers: inner.trackers,
            custom: inner.custom,
            recent: inner.recent.iter().cloned().collect(),
            top_hosts: top,
            enabled: self.enabled(),
            started_at_ms: inner.started_at_ms,
        }
    }
}

type SharedEngine = Arc<RwLock<Arc<FilterEngine>>>;

pub struct ProxyServer {
    port: u16,
    shutdown: Arc<AtomicBool>,
    stats: Arc<ProxyStats>,
}

impl ProxyServer {
    pub fn start(engine: SharedEngine, stats: Arc<ProxyStats>) -> io::Result<Self> {
        Self::start_with_stats(engine, stats)
    }

    /// Start with a caller-supplied statistics handle (used by the app).
    pub fn start_with_stats(
        engine: SharedEngine,
        stats: Arc<ProxyStats>,
    ) -> io::Result<Self> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_thread = shutdown.clone();
        let stats_thread = stats.clone();
        std::thread::Builder::new()
            .name("integrity-proxy".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(err) => {
                        eprintln!("[proxy] failed to start runtime: {err}");
                        return;
                    }
                };
                rt.block_on(run_listener(listener, engine, stats_thread, shutdown_thread));
            })?;
        Ok(Self {
            port,
            shutdown,
            stats,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn proxy_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn stats(&self) -> Arc<ProxyStats> {
        self.stats.clone()
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for ProxyServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn run_listener(
    listener: std::net::TcpListener,
    engine: SharedEngine,
    stats: Arc<ProxyStats>,
    shutdown: Arc<AtomicBool>,
) {
    let listener = match TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("[proxy] invalid listener: {err}");
            return;
        }
    };
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        match timeout(Duration::from_millis(200), listener.accept()).await {
            Ok(Ok((stream, _addr))) => {
                let engine = engine.clone();
                let stats = stats.clone();
                tokio::spawn(async move {
                    handle_connection(stream, engine, stats).await;
                });
            }
            Ok(Err(err)) => {
                if !shutdown.load(Ordering::Relaxed) {
                    eprintln!("[proxy] accept error: {err}");
                }
            }
            Err(_) => {}
        }
    }
}

async fn handle_connection(mut client: TcpStream, engine: SharedEngine, stats: Arc<ProxyStats>) {
    let _ = client.set_nodelay(true);
    let Some((head, leftover)) = read_head(&mut client).await else {
        return;
    };
    let head = String::from_utf8_lossy(&head);
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    if method.is_empty() || target.is_empty() {
        let _ = respond(&mut client, "400 Bad Request", b"").await;
        return;
    }

    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_string(), value.trim().to_string()))
        })
        .collect();

    if method.eq_ignore_ascii_case("CONNECT") {
        handle_connect(client, target, leftover, engine, stats).await;
    } else {
        handle_http(client, method, target, headers, leftover, engine, stats).await;
    }
}

async fn handle_connect(
    mut client: TcpStream,
    authority: String,
    leftover: Vec<u8>,
    engine: SharedEngine,
    stats: Arc<ProxyStats>,
) {
    let Some((host, port)) = split_authority(&authority, 443) else {
        let _ = respond(&mut client, "400 Bad Request", b"").await;
        return;
    };
    stats.record_inspected();

    if stats.enabled() {
        if let Some(category) = decide(&engine, &host) {
            stats.record_block(&host, category);
            let _ = respond(&mut client, "403 Forbidden", b"Blocked by Integrity Browser").await;
            return;
        }
    }

    let upstream = timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port))).await;
    let mut upstream = match upstream {
        Ok(Ok(stream)) => stream,
        _ => {
            let _ = respond(&mut client, "502 Bad Gateway", b"Upstream connection failed").await;
            return;
        }
    };
    let _ = upstream.set_nodelay(true);

    let ok = b"HTTP/1.1 200 Connection Established\r\nProxy-Agent: IntegrityBrowser\r\n\r\n";
    if client.write_all(ok).await.is_err() {
        return;
    }
    if !leftover.is_empty() && upstream.write_all(&leftover).await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
}

async fn handle_http(
    mut client: TcpStream,
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    leftover: Vec<u8>,
    engine: SharedEngine,
    stats: Arc<ProxyStats>,
) {
    let Some(upstream_target) = parse_http_target(&target, &headers) else {
        let _ = respond(&mut client, "400 Bad Request", b"Unsupported request target").await;
        return;
    };
    stats.record_inspected();

    if stats.enabled() {
        if let Some(category) = decide(&engine, &upstream_target.host) {
            stats.record_block(&upstream_target.host, category);
            let _ = respond(&mut client, "204 No Content", b"").await;
            return;
        }
    }

    let connect = timeout(
        CONNECT_TIMEOUT,
        TcpStream::connect((upstream_target.host.as_str(), upstream_target.port)),
    )
    .await;
    let mut upstream = match connect {
        Ok(Ok(stream)) => stream,
        _ => {
            let _ = respond(&mut client, "502 Bad Gateway", b"Upstream connection failed").await;
            return;
        }
    };
    let _ = upstream.set_nodelay(true);

    let has_upgrade = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("upgrade"));
    let mut outgoing = format!("{} {} HTTP/1.1\r\n", method, upstream_target.path);
    for (name, value) in &headers {
        if name.eq_ignore_ascii_case("proxy-connection") {
            continue;
        }
        if name.eq_ignore_ascii_case("connection") {
            continue;
        }
        outgoing.push_str(name);
        outgoing.push_str(": ");
        outgoing.push_str(value);
        outgoing.push_str("\r\n");
    }
    // Forcing connection close avoids an unfiltered HTTP keep-alive pipeline
    // between the webview and an upstream server. WebSocket upgrades need the
    // normal bidirectional relay instead.
    if has_upgrade {
        outgoing.push_str("Connection: Upgrade\r\n");
    } else {
        outgoing.push_str("Connection: close\r\n");
    }
    outgoing.push_str("\r\n");

    if upstream.write_all(outgoing.as_bytes()).await.is_err() {
        let _ = respond(&mut client, "502 Bad Gateway", b"Upstream write failed").await;
        return;
    }
    if !leftover.is_empty() && upstream.write_all(&leftover).await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
}

fn decide(engine: &SharedEngine, host: &str) -> Option<Category> {
    let guard = engine.read().unwrap_or_else(|e| e.into_inner());
    guard.decision(host)
}

struct HttpTarget {
    host: String,
    port: u16,
    path: String,
}

fn parse_http_target(target: &str, headers: &[(String, String)]) -> Option<HttpTarget> {
    if target.starts_with("http://") {
        let url = url::Url::parse(target).ok()?;
        let host = url.host_str()?.to_ascii_lowercase();
        let port = url.port_or_known_default().unwrap_or(80);
        let mut path = url.path().to_string();
        if path.is_empty() {
            path.push('/');
        }
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        return Some(HttpTarget { host, port, path });
    }
    if target.starts_with("https://") {
        // No TLS interception: WebKit should use CONNECT for HTTPS.
        return None;
    }
    if target.starts_with('/') || target == "*" {
        let host_header = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("host"))
            .map(|(_, value)| value.as_str())?;
        let (host, port) = split_authority(host_header, 80)?;
        return Some(HttpTarget {
            host,
            port,
            path: target.to_string(),
        });
    }
    None
}

fn split_authority(raw: &str, default_port: u16) -> Option<(String, u16)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(rest) = raw.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = rest[..end].to_string();
        let after = &rest[end + 1..];
        let port = match after.strip_prefix(':') {
            Some(port) if !port.is_empty() => port.parse().ok()?,
            Some(_) => default_port,
            None => default_port,
        };
        return Some((host, port));
    }
    if raw.matches(':').count() > 1 {
        // Unbracketed IPv6 literal; no port can be inferred safely.
        return Some((raw.to_string(), default_port));
    }
    if let Some((host, port)) = raw.rsplit_once(':') {
        if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) {
            return Some((host.to_ascii_lowercase(), port.parse().ok()?));
        }
    }
    Some((raw.to_ascii_lowercase(), default_port))
}

async fn read_head(stream: &mut TcpStream) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 2048];
    loop {
        if buffer.len() > MAX_HEADER_BYTES {
            return None;
        }
        let read = timeout(HEADER_TIMEOUT, stream.read(&mut chunk)).await;
        let count = match read {
            Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return None,
            Ok(Ok(count)) => count,
        };
        buffer.extend_from_slice(&chunk[..count]);
        if let Some(index) = find_bytes(&buffer, b"\r\n\r\n") {
            let head = buffer[..index].to_vec();
            let leftover = buffer[index + 4..].to_vec();
            return Some((head, leftover));
        }
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

async fn respond(stream: &mut TcpStream, status: &str, body: &[u8]) -> io::Result<()> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    stream.write_all(&response).await?;
    let _ = stream.shutdown().await;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::FilterEngine;

    fn test_engine() -> SharedEngine {
        let engine = FilterEngine::from_lists(&[(
            "||blocked.test^\n||tracker.test^\n".to_string(),
            Category::Ads,
            "test".into(),
        )]);
        Arc::new(RwLock::new(Arc::new(engine)))
    }

    async fn read_all(stream: &mut TcpStream) -> Vec<u8> {
        let mut out = Vec::new();
        let _ = timeout(Duration::from_secs(5), stream.read_to_end(&mut out)).await;
        out
    }

    async fn read_headers(stream: &mut TcpStream) -> Vec<u8> {
        let mut out = Vec::new();
        let mut byte = [0u8; 1];
        let _ = timeout(Duration::from_secs(5), async {
            loop {
                match stream.read(&mut byte).await {
                    Ok(0) => break,
                    Ok(_) => {
                        out.push(byte[0]);
                        if out.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .await;
        out
    }

    #[tokio::test]
    async fn blocks_http_and_connect_requests() {
        let stats = Arc::new(ProxyStats::new());
        let proxy = ProxyServer::start_with_stats(test_engine(), stats.clone()).unwrap();
        let port = proxy.port();

        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client
            .write_all(b"GET http://blocked.test/ads.js HTTP/1.1\r\nHost: blocked.test\r\n\r\n")
            .await
            .unwrap();
        let response = String::from_utf8_lossy(&read_all(&mut client).await).to_string();
        assert!(response.starts_with("HTTP/1.1 204"), "{response}");

        let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        client
            .write_all(b"CONNECT tracker.test:443 HTTP/1.1\r\nHost: tracker.test:443\r\n\r\n")
            .await
            .unwrap();
        let response = String::from_utf8_lossy(&read_all(&mut client).await).to_string();
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.blocked, 2);
        assert!(snapshot.recent.iter().any(|r| r.host == "blocked.test"));
        assert!(snapshot.recent.iter().any(|r| r.host == "tracker.test"));
    }

    #[tokio::test]
    async fn forwards_allowed_http_requests() {
        let upstream = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let _ = read_headers(&mut socket).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                )
                .await
                .unwrap();
            let _ = socket.shutdown().await;
        });

        let stats = Arc::new(ProxyStats::new());
        let proxy = ProxyServer::start_with_stats(test_engine(), stats.clone()).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .unwrap();
        let request = format!(
            "GET http://127.0.0.1:{upstream_port}/hello HTTP/1.1\r\nHost: 127.0.0.1:{upstream_port}\r\n\r\n"
        );
        client.write_all(request.as_bytes()).await.unwrap();
        let response = String::from_utf8_lossy(&read_all(&mut client).await).to_string();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("hello"), "{response}");
        server.await.unwrap();
        assert_eq!(stats.snapshot().blocked, 0);
    }

    #[tokio::test]
    async fn tunnels_allowed_connect_requests() {
        let echo = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = echo.accept().await.unwrap();
            let mut buf = [0u8; 4];
            socket.read_exact(&mut buf).await.unwrap();
            socket.write_all(&buf).await.unwrap();
            let _ = socket.shutdown().await;
        });

        let stats = Arc::new(ProxyStats::new());
        let proxy = ProxyServer::start_with_stats(test_engine(), stats.clone()).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .unwrap();
        let request = format!(
            "CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\nHost: 127.0.0.1:{echo_port}\r\n\r\n"
        );
        client.write_all(request.as_bytes()).await.unwrap();
        let headers = String::from_utf8_lossy(&read_headers(&mut client).await).to_string();
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        client.write_all(b"ping").await.unwrap();
        let mut echoed = [0u8; 4];
        timeout(Duration::from_secs(5), client.read_exact(&mut echoed))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&echoed, b"ping");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn disabling_blocking_stops_decisions() {
        let stats = Arc::new(ProxyStats::new());
        stats.set_enabled(false);
        let proxy = ProxyServer::start_with_stats(test_engine(), stats.clone()).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .unwrap();
        client
            .write_all(b"GET http://blocked.test/ads.js HTTP/1.1\r\nHost: blocked.test\r\n\r\n")
            .await
            .unwrap();
        let response = String::from_utf8_lossy(&read_all(&mut client).await).to_string();
        // Not blocked: the proxy attempts DNS/upstream and reports a gateway error.
        assert!(!response.starts_with("HTTP/1.1 204"), "{response}");
        assert_eq!(stats.snapshot().blocked, 0);
    }

    #[test]
    fn authority_parser_handles_ipv6_and_ports() {
        assert_eq!(
            split_authority("example.com:443", 80),
            Some(("example.com".into(), 443))
        );
        assert_eq!(
            split_authority("example.com", 80),
            Some(("example.com".into(), 80))
        );
        assert_eq!(
            split_authority("[::1]:8080", 80),
            Some(("::1".into(), 8080))
        );
        assert_eq!(
            split_authority("::1", 80),
            Some(("::1".into(), 80))
        );
    }
}
