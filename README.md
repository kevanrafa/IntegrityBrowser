# Integrity Browser

A privacy-first desktop web browser MVP with host-level ad/tracker blocking and
click-interception protection. Built with Rust, [wry](https://github.com/tauri-apps/wry)
and WebKitGTK (Linux) / WebView2 (Windows), with an HTML/CSS/JavaScript UI.

> **Status:** functional MVP. Navigation, tabs, blocking, settings persistence and
> the Integrity Protection engine work. The Linux/GTK layout is implemented but
> has not been runtime-tested on Ubuntu in the development environment used for
> this build; see [Known limitations](#known-limitations).

---

## Features

### Browsing
- Address bar with URL/search detection; **DuckDuckGo** is the built-in search engine.
- Tabs: open, switch, close, middle-click close, per-tab history.
- Back, forward, reload/stop, home and a dark-themed new-tab page.
- HTTPS/HTTP security indicator and a text-based favicon fallback.
- Dark UI for the browser chrome and all internal pages.

### Privacy
- **Ad blocking** and **tracker blocking** through a local HTTP/CONNECT filtering
  proxy. HTTPS stays end-to-end encrypted: only the destination hostname is
  inspected, never the payload.
- Two built-in curated host lists (`filters/ads.txt`, `filters/trackers.txt`),
  compiled into the binary.
- Add remote filter lists by URL. Lists are cached locally and can be refreshed.
- Add your own blocked hostnames.
- Live counters (blocked, ads, trackers, inspected) plus recent blocked hosts.
- Settings are persisted to JSON and survive restarts.

### Integrity Protection
A hybrid overlay detector and click-through system for “you clicked the page but
a transparent layer swallowed the click” attacks:

1. **Static scan** at document start and on DOM mutations. Elements are flagged
   only when they are large, stacked above the page, have low opacity, accept
   pointer events, and contain no visible content.
2. **Click-time check** on trusted `pointerdown`. If a suspicious layer is under
   the pointer, it is disarmed, the click is allowed to reach the element below,
   and a synthetic click is used as a fallback if the browser already committed
   the original target.
3. **Per-site pause** and a global **ON/OFF** switch.
4. Legitimate transparent elements are preserved: small overlays, elements with
   visible content, dialogs/`popover` content, and explicit opt-out markers are
   not neutralized.
5. Detection sensitivity: **Relaxed**, **Balanced** (default), **Strict**.
6. An independent, very strict **Block hidden new-tab links** option. Invisible
   `target="_blank"` links are counted as clicks by the page, but the browser does
   not open a tab. Modified clicks (Ctrl/Shift/Alt/Meta) still work normally.

The engine is best-effort protection, not a security boundary. A hostile page can
tamper with page JavaScript. The real security boundary is the Rust IPC channel
described in [Security model](#security-model).

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│ tao native window                                           │
│                                                             │
│  ┌───────────────────────────────────────────────────────┐  │
│  │ chrome webview (trusted)                              │  │
│  │ index.html + chrome.js + chrome.css                   │  │
│  │ tabs · address bar · privacy/integrity indicators     │  │
│  └───────────────────────────────────────────────────────┘  │
│  ┌───────────────────────────────────────────────────────┐  │
│  │ content webview (untrusted web)                       │  │
│  │ new-tab · settings · web pages                        │  │
│  │ engine.js injected at document start                  │  │
│  └───────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
          │ chrome IPC                    │ content IPC
          ▼                               ▼
┌─────────────────────────────────────────────────────────────┐
│ Rust app (tao event loop)                                   │
│ Browser state · tabs · settings · internal routing           │
│ ┌──────────────────────┐  ┌───────────────────────────────┐ │
│ │ integrity-core        │  │ local filtering proxy         │ │
│ │ filters · settings    │  │ 127.0.0.1:<ephemeral>         │ │
│ │ URL policy · proxy    │  │ HTTP + CONNECT host decisions │ │
│ └──────────────────────┘  └───────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

### Why not Tauri?

Tauri 2’s multiwebview API is not usable for this layout on Linux. In
`tauri-runtime-wry` 2.12.3, child webviews are packed into the window’s
`default_vbox()`, so a fixed-height chrome webview cannot coexist with an
expanding page webview (upstream issue
[tauri-apps/tauri#10420](https://github.com/tauri-apps/tauri/issues/10420)).
This project therefore uses `wry` + `tao` directly and builds the platform
layout itself:

- **Linux:** two GTK wrapper rows inside tao’s `default_vbox()`; the chrome row
  has a fixed 88 px request and the content row expands.
- **Windows:** two child WebView2 controls positioned with `set_bounds()` and
  updated on resize.

The frontend is plain HTML/CSS/JS in both cases. There is no bundler, framework or
JavaScript runtime dependency.

### Internal pages

Internal pages are embedded in the binary and served through wry’s custom
protocol:

| Platform | Origin |
| --- | --- |
| Linux / macOS | `ib://localhost` |
| Windows (WebView2) | `http://ib.localhost` |

`integrity_core::urlutil::platform_url()` translates `ib://localhost` to the
platform origin for programmatic navigation. Both origins are treated as
internal by URL policy and the engine is skipped on internal pages, except the
self-test harness which explicitly forces it.

---

## Requirements

### Ubuntu Linux

Tested dependency set targets **Ubuntu 22.04+ / 24.04** with WebKitGTK 4.1:

```bash
sudo apt update
sudo apt install -y \
  build-essential pkg-config curl \
  libgtk-3-dev \
  libwebkit2gtk-4.1-dev \
  libjavascriptcoregtk-4.1-dev \
  libsoup-3.0-dev \
  libxdo-dev \
  libssl-dev \
  librsvg2-dev
```

Also required:

- Rust stable (edition 2021). The project was compiled with Rust 1.93.
- A desktop session with GTK 3 and WebKitGTK 4.1 (`libwebkit2gtk-4.1-0`).
- X11 or Wayland. Both are supported by the GTK layout.

If `pkg-config` cannot find WebKitGTK, confirm that the 4.1 development package
is installed (not the older 4.0 package) and that
`pkg-config --modversion webkit2gtk-4.1` succeeds.

### Windows (development / smoke testing)

- Windows 10/11 with the Microsoft Edge WebView2 Runtime.
- Visual Studio Build Tools with the MSVC C++ workload.
- Rust stable with the `x86_64-pc-windows-msvc` toolchain.

---

## Build and run

From the repository root:

```bash
# Check the Rust workspace
cargo check --workspace

# Run the browser (debug)
cargo run -p integrity-browser

# Optimized build
cargo build --release -p integrity-browser
./target/release/integrity-browser
```

The workspace has two crates:

- `integrity-browser` — the desktop app (tao/wry shell, IPC, settings UI).
- `integrity-core` — URL policy, filter engine, proxy, settings, tests.

### Configuration directory

Settings and cached remote filter lists are stored in the platform config
directory:

| Platform | Default path |
| --- | --- |
| Linux | `~/.config/integrity-browser/` |
| Windows | `%APPDATA%\integrity-browser\` |

Override it for testing or portable runs:

```bash
INTEGRITY_BROWSER_CONFIG_DIR=/tmp/integrity-browser ./target/debug/integrity-browser
```

Files:

- `settings.json` — user settings and filter-list metadata.
- `lists/` — cached copies of remote filter lists.

Deleting `settings.json` restores defaults on the next launch.

---

## Using the browser

- Type a URL or a search term into the address bar and press **Enter**.
  Bare hostnames are normalized to HTTPS; unknown schemes such as
  `javascript:` and `file:` are treated as search queries and never loaded.
- The **shield** button opens Privacy settings; the **Integrity** pill opens
  Integrity Protection settings. Both are full pages inside the content area,
  not popovers.
- **Done / Back to browsing** returns to the last real website.

### Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+L` | Focus address bar |
| `Ctrl+T` | New tab |
| `Ctrl+W` | Close active tab |
| `Ctrl+R` / `F5` | Reload |
| `Alt+Left` | Back |
| `Alt+Right` | Forward |
| `Ctrl+,` | Open settings |
| `Ctrl+Shift+I` / `F12` | Open WebView devtools |

Shortcuts are delivered by the chrome webview and by internal pages. Focus inside
a regular web page does not currently forward shortcuts to the browser shell; see
[Known limitations](#known-limitations).

---

## Privacy design

The filtering proxy is a small HTTP proxy bound to `127.0.0.1` on an ephemeral
port. Both webviews use it through wry’s `ProxyConfig`.

- **HTTP:** the proxy parses the absolute request URL, checks the destination
  host, then relays the request. Blocked requests receive `204 No Content`.
- **HTTPS:** the webview uses `CONNECT`. The proxy sees only `host:port`; blocked
  hosts receive `403 Forbidden`, allowed hosts are tunneled with
  `copy_bidirectional`. There is **no TLS interception**.
- Localhost destinations are never blocked.
- Proxy credentials from the destination URL are not forwarded.
- The proxy and its stats are owned by the app; the content page cannot reach
  the proxy control surface.

Blocking is hostname-based. URL-path rules, cosmetic filtering (element hiding)
and per-request rules are intentionally out of scope for this MVP.

### Filter lists

Built-in lists use Adblock-style host rules:

```
! comment
||ads.example^
||tracker.example^
```

Remote lists are fetched over HTTPS only, size-limited, parsed for `||host^`
rules, cached under the config directory, and can be enabled/disabled or removed.
A remote list with no cached file is disabled until its first successful fetch.

---

## Integrity Protection details

The engine lives in `app/frontend/engine.js` and is injected at document start
into the content webview.

Detection signals:

- element covers a large share of the viewport;
- computed opacity is low;
- background is effectively transparent;
- element accepts pointer input and is topmost at its center;
- no visible text/media/dialog content;
- no explicit opt-out markers (`data-integrity-allow`, `role="dialog"`,
  `popover`, etc.);
- large `iframe` overlays are treated more conservatively.

Sensitivity presets (coverage / opacity thresholds):

| Preset | Intended use |
| --- | --- |
| Relaxed | Fewer false positives; only very large, nearly invisible layers |
| Balanced | Default; large semi-transparent click-catchers |
| Strict | Aggressive; also catches smaller and more visible layers |

### Hidden new-tab links

The **Block hidden new-tab links** switch is separate from sensitivity. Enable it
for the strictest behaviour:

- At document start, `window.open` is wrapped. During a blocked hidden-link click
  it returns a harmless stub, so a page handler that calls `window.open` cannot
  create a window either.
- On a capture-phase `click`, the engine finds the nearest `a[href]` whose target
  is `_blank`. If that link or an ancestor is near-invisible (low opacity,
  transparent and textless, hidden, or already flagged as a suspicious overlay),
  the engine calls `preventDefault()` and leaves event propagation alone.
- The site's own click handlers still run, so analytics and handlers see the
  gesture. The tab stays put.
- Clicks with Ctrl, Meta, Shift or Alt are skipped, so a deliberate "open in new
  tab" still works.
- Each block increments the page statistic **Hidden tabs blocked** and reports an
  event to the Rust side.

At click time the engine temporarily disables `pointer-events` on the flagged
layer, resolves `document.elementFromPoint()`, restores styles, and lets the
underlying element receive the interaction. This is why the engine avoids
blanket `pointer-events: none`: legitimate transparent UI would break.

All detections and click-throughs are reported to Rust over the content IPC
channel and shown in the settings page.

---

## Security model

There are two IPC channels with different privileges:

| Channel | Who can use it | Allowed operations |
| --- | --- | --- |
| Chrome IPC | Trusted `index.html` only | Full browser commands and settings mutations |
| Content IPC | Any loaded page / internal page | Integrity stats, shortcut requests, and (only while an internal page is active) settings commands |

Additional guardrails:

- Message size capped at 16 KiB; per-channel rate limiting.
- Integrity stats are accepted only when the reported host matches the active tab.
- Settings mutations from the content webview are gated on
  `Browser::active_is_internal()`. A regular website cannot change settings.
- `close_settings` is accepted only while the settings page is the active URL.
- Remote filter-list URLs must be HTTPS and pass validation; remote list rules
  are parsed as host rules, not executed.
- Navigation policy allows only `http`, `https`, `ib` internal pages and
  `about:blank`. `file:`, `javascript:`, `data:` and `blob:` are refused.
- Popups are denied and optionally reopened as a tab only when the URL passes
  navigation policy.
- Internal pages are served with a restrictive CSP and security headers.

The page-side engine is not trusted. Its output is treated as hints for UI and
statistics; it cannot cross the Rust security boundary.

---

## Testing

### Rust

```bash
cargo test --workspace
```

Current result: **28 tests passed** (filters, proxy behaviour, URL policy,
settings normalization/persistence, platform URL mapping). `cargo check
--workspace` is clean.

### Integrity self-test harness

Open **Settings → About → Run the Integrity self-test** or **Integrity → Open
Integrity self-test** in the running browser. The harness lives at
`app/frontend/tests/integrity-harness.html` and exercises twelve scenarios:
static overlays, stacked layers, an invisible iframe, click-through, legitimate
transparent elements, opt-outs, dialogs, neutered overlays, invisible `_blank`
links, visible `_blank` links, modified clicks and `window.open` suppression.

The harness also runs standalone in any modern browser. Serve the frontend
directory rather than opening `file://`:

```bash
cd app/frontend
python3 -m http.server 8123
# then open http://127.0.0.1:8123/tests/integrity-harness.html
```

Expected result: all 12 scenarios pass, with no console errors.

### Manual smoke checklist

1. Launch with an isolated config directory.
2. Navigate to an HTTPS site; confirm the address bar, title and tab update.
3. Open Privacy settings; toggle ad blocking; reload; confirm persistence in
   `settings.json`.
4. Open Integrity settings; toggle the global switch, per-site pause and
   **Block hidden new-tab links**.
5. Open the self-test harness; confirm neutralization and click-through work.
6. Attempt a settings mutation from a regular web page console; confirm it is
   ignored.

---

## Project layout

```
IntegrityBrowser/
├── Cargo.toml                 # workspace
├── app/
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs            # event loop, IPC, proxy/service wiring
│   │   ├── browser.rs         # tabs, history, settings, snapshots
│   │   ├── assets.rs          # embedded frontend + CSP/security headers
│   │   └── layout.rs          # Linux GTK rows / Windows WebView2 bounds
│   └── frontend/
│       ├── index.html         # chrome UI
│       ├── chrome.js/.css
│       ├── newtab.html/js/css
│       ├── settings.html/js/css
│       ├── engine.js          # Integrity Protection
│       ├── internal.*         # shared internal-page helpers
│       ├── assets/logo.svg    # brand mark supplied by the project owner
│       └── tests/             # overlay and hidden-link self-test harness
├── crates/integrity-core/
│   └── src/
│       ├── filters.rs         # host-rule engine
│       ├── proxy.rs           # local filtering proxy + stats
│       ├── settings.rs        # persisted settings
│       └── urlutil.rs         # URL normalization/policy
└── filters/
    ├── ads.txt                # built-in ad host list
    └── trackers.txt           # built-in tracker host list
```

---

## Known limitations

- **Linux runtime verification is pending.** The GTK two-row layout compiles in
  the source tree but was not run on Ubuntu in the development environment. The
  Windows/WebView2 backend was smoke-tested. Treat the Linux build as the first
  thing to verify on target hardware.
- **One content webview for all tabs.** Switching tabs reloads the destination
  URL; page scroll position and in-page JavaScript state are not preserved.
  Back/forward uses per-tab URL history and also reloads.
- **No TLS interception.** Blocking sees HTTPS hostnames only; path-level and
  response-based rules are not possible by design.
- **Host rules only.** No cosmetic filtering, scriptlets, or per-element rules.
- **Remote filter lists need a network and a valid HTTPS URL.** There is no
  bundled large third-party list; the built-ins are deliberately conservative.
- **No download manager, bookmarks, history UI, password manager, or tab restore.**
- **Keyboard shortcuts do not work while focus is inside a regular web page.**
- **No automatic updates or code signing.**
- **The page-side Integrity engine can be tampered with by a hostile page.**
  It raises the cost of click-interception attacks; it is not a hardened
  security boundary.
- **Downloads and response handling are delegated to the platform webview.**
  The proxy relays bytes; it does not scan content.
- **No automated end-to-end browser test suite.** The Rust tests and the
  JavaScript harness cover the logic and overlay scenarios; desktop IPC flows
  are exercised manually.

### Next steps

1. Verify and fix the GTK layout on Ubuntu 22.04/24.04 (X11 and Wayland), then
   add a CI job with the WebKitGTK dependencies.
2. Add native application accelerators so browser shortcuts work regardless of
   webview focus.
3. Preserve per-tab webview state (scroll, forms, media) instead of reloading on
   tab switch.
4. Add optional EasyList/uBlock-compatible host-list import with validation and
   size limits.
5. Add history/bookmarks/downloads UI and session restore.
6. Add integration tests for IPC authorization, settings persistence, and
   navigation policy, plus a packaged `.deb`/AppImage build.

---

## License

MIT. See [LICENSE](LICENSE).
