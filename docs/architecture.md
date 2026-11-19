# Architecture

This document records the technical decisions behind Integrity Browser. The
top-level [README](../README.md) covers build and usage.

## Goals and constraints

- Ubuntu Linux desktop browser, functional MVP (not a mockup).
- HTML/CSS/JS frontend, Rust backend.
- Two independent surfaces: a trusted browser chrome and untrusted web content.
- Host-level ad/tracker blocking without breaking HTTPS.
- Overlay detection with click-through that preserves legitimate transparent UI.
- No Tauri multiwebview on Linux.

## Why wry + tao directly

Tauri 2 was evaluated first. `tauri-runtime-wry` 2.12.3 implements multiwebview by
packing child webviews into the window's `default_vbox()`. `GtkBox` children get
equal expansion, so a fixed-height browser chrome and an expanding page area
cannot be expressed through that API. The upstream issue is
[tauri-apps/tauri#10420](https://github.com/tauri-apps/tauri/issues/10420).

The project therefore depends on:

- `wry 0.55.1` — webview construction, custom protocol, IPC, proxy configuration.
- `tao 0.35.2` — window and event loop.
- `gtk 0.18` on Linux — wrapper rows for layout.

The Tauri features that would have helped (bundle, updater, IPC macros) are not
required for an MVP and are replaced by a small amount of explicit code.

## Process layout

A single tao event loop owns:

- `Window`
- `chrome: WebView` — `index.html`
- `content: WebView` — new tab, settings, self-test, web pages
- `Browser` — tabs, history, settings, snapshots
- `ProxyServer` — local HTTP/CONNECT filter proxy

All UI is HTML. Rust never renders chrome widgets.

## Native layout

`app/src/layout.rs` exposes `build_webviews()` and `resize()`.

### Linux

1. Get tao's `default_vbox()` via `WindowExtUnix`.
2. Create two `gtk::Box` wrapper rows packed vertically:
   - chrome: fixed `size_request(-1, 88)`, `expand = false`;
   - content: `expand = true`.
3. Build each wry webview with `build_gtk(&wrapper)`.

GTK owns sizing; `resize()` is intentionally a no-op. The 88 px value is kept in
sync with `--chrome-height` in `chrome.css`.

### Windows

Use two `build_as_child(window)` webviews and position them with logical
`Rect` values:

- chrome at `(0, 0)`, height 88;
- content at `(0, 88)`, remaining height.

`resize()` recomputes both rectangles on window resize.

### WebView2 note

Two WebView2 controls that share a user-data folder must receive identical
`AdditionalBrowserArguments`. Applying the proxy configuration to only one
webview produces `0x8007139F` (`Class not registered` / invalid state). Both
builders therefore get the same `ProxyConfig`.

## Trust boundary and IPC

Chrome (trusted) and content (untrusted) have separate `with_ipc_handler`
channels.

### Chrome IPC

Accepts full commands: navigate, tab operations, history, settings, filter lists,
stats reset, devtools. Input is JSON, limited to 16 KiB, rate-limited to 200
messages/second.

### Content IPC

Accepts:

- Integrity engine `config_request`, `stats`, `neutralized`, `click_through`.
- Shortcut requests when marked trusted.
- Navigation/settings commands **only while the active tab is an internal page**
  (`Browser::active_is_internal()`).

The last condition is the important one. A remote page can call
`window.ipc.postMessage(...)`, so every settings command is re-checked against the
active URL before dispatch. Integrity stats are additionally rejected when the
reported host does not match the active tab.

Internal pages are recognized on both `ib://localhost` and `http://ib.localhost`.
On Windows, programmatic navigation must use the HTTP form; wry rewrites the
initial `with_url()` load but not later `load_url()` calls. `platform_url()`
centralizes that translation.

## Tab model

`Browser` stores a vector of `Tab` values:

```rust
struct Tab {
    id: u64,
    url: String,
    title: String,
    history: Vec<String>,
    index: usize,
    loading: bool,
}
```

There is one content webview. The active tab's URL is loaded into it; switching
tabs reloads the stored URL. This keeps the native layout simple and avoids
platform-specific multi-webview bookkeeping. The cost is loss of per-tab page
state, documented in the README.

Navigation updates the active tab on `PageLoadEvent::Started/Finished`, and
same-document navigations update the current history entry.

## Proxy and filters

`integrity-core::proxy` starts a Tokio current-thread runtime on a dedicated
thread and binds `TcpListener` to `127.0.0.1:0`. wry is configured with that
endpoint.

- `CONNECT host:port`: parse authority, consult the filter engine, then either
  return `403` or open an upstream TCP connection and relay bytes.
- Absolute-form HTTP requests: parse `http://host/path`, consult the engine,
  return `204` or forward with `Connection: close`.
- WebSocket upgrades keep the upgrade headers and use bidirectional relay.

Stats are held in `ProxyStats` (atomics + mutex) and snapshotted into the UI
once per second.

The engine (`filters.rs`) merges enabled lists into hostname sets:

- built-in ads, built-in trackers, remote lists, custom hosts;
- subdomain matching (`www.doubleclick.net` matches `doubleclick.net`);
- localhost is never blocked.

Remote lists are HTTPS-only, size-limited, cached as files, and parsed for
`||host^` rules. A remote list with no cache is disabled until fetched.

## Integrity engine

`engine.js` is injected at document start into the content webview. It runs in
the page world and is deliberately best-effort:

1. skip internal pages except the self-test harness;
2. scan DOM elements on idle/`requestAnimationFrame`, and rescan on mutations;
3. classify suspicious overlays from size, opacity, background, stacking,
   pointer-events and absence of visible content;
4. on trusted `pointerdown`, temporarily disable `pointer-events` on suspicious
   ancestors, resolve the element below, restore styles, and allow the click to
   land there;
5. report stats and actions over IPC.

### Hidden new-tab blocking

The `integrity_block_hidden_tabs` setting is an opt-in strict mode. It is
independent of the sensitivity preset because it changes browser navigation
behaviour, not just overlay classification.

The engine wraps `window.open` at document start. The wrapper forwards every call
to the native function unless a hidden-link click was just blocked. In that case
it returns a small stub so a page handler does not throw while trying to use the
returned window.

A capture-phase `click` listener finds the nearest `a[href]` with a `_blank`
target and checks whether the link or an ancestor is effectively invisible:
low accumulated opacity, hidden/display:none, a suspicious overlay record, or a
large transparent textless box. A match calls `preventDefault()` but deliberately
does not call `stopPropagation()`. That distinction is the whole feature: the
browser cancels the navigation while the page's click handlers still observe the
gesture. Modified clicks bypass the check so the user keeps explicit control.

The block is counted in page stats and reported to Rust as
`hidden_tab_blocked`. The setting is persisted and pushed into the content
webview through `content_config()` under `blockHiddenTabs`.

It never touches `document.cookie`, storage, or network APIs, and it never
receives settings authority. A hostile page can tamper with it, which is why the
README describes it as mitigation rather than a security boundary.

## Internal pages and assets

`assets.rs` embeds all HTML/CSS/JS with `include_str!` and serves it through the
custom protocol. It attaches:

- `Content-Security-Policy` restricting scripts/styles/images to the internal
  origin (no remote script execution);
- `X-Content-Type-Options: nosniff`;
- `Referrer-Policy: no-referrer`;
- `Cache-Control: no-store`.

Settings is a full-height internal page in the content webview, not a popover or
a third webview. This keeps layout, focus and routing simple.

## Persistence

`integrity-core::settings` stores JSON in the platform config directory or
`INTEGRITY_BROWSER_CONFIG_DIR`. Load errors fall back to defaults with a warning
instead of panicking. Normalization guarantees the two built-in lists exist once,
deduplicates hosts, and disables remote lists without cached data.

## Known structural limits

- Single content webview means reload-based tab switching.
- Desktop shortcuts require chrome/internal focus.
- No TLS interception means no path-level filtering.
- No browser-level download manager.
- Linux GTK path needs runtime verification on Ubuntu.
