//! Platform layout for the two-webview shell.
//!
//! The chrome UI must keep a fixed pixel height while the page fills the rest.
//! A single webview cannot host a native browser chrome, so the app uses two
//! webviews:
//!
//! * Windows: two child WebView2 controls positioned with `set_bounds`.
//! * Linux: two GTK container rows inside tao's `default_vbox()`. wry packs
//!   webviews into a `GtkBox` with `expand = true`, so each webview gets its
//!   own wrapper box and the wrapper controls the row sizing. This avoids
//!   `GtkFixed` positioning quirks with WebKitGTK and works on Wayland as well
//!   as X11.
//!
//! This is also the reason the project does not use Tauri's built-in
//! multiwebview API on Linux (see docs/architecture.md).

use wry::{WebView, WebViewBuilder};

/// Height of the HTML chrome (tab strip + toolbar) in logical pixels.
/// Keep this in sync with `--chrome-height` in frontend/chrome.css.
pub const CHROME_HEIGHT: f64 = 88.0;

pub fn build_webviews(
    window: &tao::window::Window,
    chrome: WebViewBuilder<'_>,
    content: WebViewBuilder<'_>,
) -> wry::Result<(WebView, WebView)> {
    #[cfg(target_os = "linux")]
    {
        use gtk::prelude::*;
        use tao::platform::unix::WindowExtUnix;
        use wry::dpi::LogicalSize;
        use wry::WebViewBuilderExtUnix;

        let vbox = window
            .default_vbox()
            .expect("tao GTK window must expose a default vbox");

        let chrome_wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);
        chrome_wrap.set_size_request(-1, CHROME_HEIGHT as i32);
        let content_wrap = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Fixed-height chrome row, expanding page row.
        vbox.pack_start(&chrome_wrap, false, false, 0);
        vbox.pack_start(&content_wrap, true, true, 0);
        chrome_wrap.show_all();
        content_wrap.show_all();

        let size = window
            .inner_size()
            .to_logical::<u32>(window.scale_factor());
        let chrome_height = CHROME_HEIGHT.round() as u32;
        let chrome_position = CHROME_HEIGHT.round() as i32;
        let page_height = size.height.saturating_sub(chrome_height).max(1);
        let chrome = chrome
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0, 0).into(),
                size: LogicalSize::new(size.width, chrome_height).into(),
            })
            .build_gtk(&chrome_wrap)?;
        let content = content
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0, chrome_position).into(),
                size: LogicalSize::new(size.width, page_height).into(),
            })
            .build_gtk(&content_wrap)?;
        Ok((chrome, content))
    }

    #[cfg(not(target_os = "linux"))]
    {
        use wry::dpi::{LogicalPosition, LogicalSize};
        use wry::Rect;

        let size = window
            .inner_size()
            .to_logical::<u32>(window.scale_factor());
        let chrome_height = CHROME_HEIGHT.round() as u32;
        let chrome_position = CHROME_HEIGHT.round() as i32;
        let page_height = size.height.saturating_sub(chrome_height).max(1);
        let chrome = chrome
            .with_bounds(Rect {
                position: LogicalPosition::new(0, 0).into(),
                size: LogicalSize::new(size.width, chrome_height).into(),
            })
            .build_as_child(window)?;
        let content = content
            .with_bounds(Rect {
                position: LogicalPosition::new(0, chrome_position).into(),
                size: LogicalSize::new(size.width, page_height).into(),
            })
            .build_as_child(window)?;
        Ok((chrome, content))
    }
}

pub fn resize(window: &tao::window::Window, chrome: &WebView, content: &WebView) {
    #[cfg(not(target_os = "linux"))]
    {
        use wry::dpi::{LogicalPosition, LogicalSize};
        use wry::Rect;

        let size = window
            .inner_size()
            .to_logical::<u32>(window.scale_factor());
        let chrome_height = CHROME_HEIGHT.round() as u32;
        let chrome_position = CHROME_HEIGHT.round() as i32;
        let page_height = size.height.saturating_sub(chrome_height).max(1);
        let _ = chrome.set_bounds(Rect {
            position: LogicalPosition::new(0, 0).into(),
            size: LogicalSize::new(size.width, chrome_height).into(),
        });
        let _ = content.set_bounds(Rect {
            position: LogicalPosition::new(0, chrome_position).into(),
            size: LogicalSize::new(size.width, page_height).into(),
        });
    }

    #[cfg(target_os = "linux")]
    {
        // GTK containers handle the layout; a resize needs no manual bounds.
        let _ = (window, chrome, content);
    }
}
