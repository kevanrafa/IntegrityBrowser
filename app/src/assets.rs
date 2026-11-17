//! Embedded frontend assets served through the `ib://` custom protocol.
//!
//! Everything is compiled into the binary so the MVP runs from a single
//! executable without a separate resource directory.

use std::borrow::Cow;

use wry::http::header::{self, HeaderValue};
use wry::http::{Request, Response, StatusCode};

struct Asset {
    mime: &'static str,
    body: &'static [u8],
}

const HTML_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn asset_for(path: &str) -> Option<Asset> {
    let asset = match path {
        "/" | "/index.html" => Asset {
            mime: "text/html; charset=utf-8",
            body: include_bytes!("../frontend/index.html"),
        },
        "/chrome.css" => Asset {
            mime: "text/css; charset=utf-8",
            body: include_bytes!("../frontend/chrome.css"),
        },
        "/chrome.js" => Asset {
            mime: "application/javascript; charset=utf-8",
            body: include_bytes!("../frontend/chrome.js"),
        },
        "/newtab.html" => Asset {
            mime: "text/html; charset=utf-8",
            body: include_bytes!("../frontend/newtab.html"),
        },
        "/newtab.css" => Asset {
            mime: "text/css; charset=utf-8",
            body: include_bytes!("../frontend/newtab.css"),
        },
        "/newtab.js" => Asset {
            mime: "application/javascript; charset=utf-8",
            body: include_bytes!("../frontend/newtab.js"),
        },
        "/settings.html" => Asset {
            mime: "text/html; charset=utf-8",
            body: include_bytes!("../frontend/settings.html"),
        },
        "/settings.css" => Asset {
            mime: "text/css; charset=utf-8",
            body: include_bytes!("../frontend/settings.css"),
        },
        "/settings.js" => Asset {
            mime: "application/javascript; charset=utf-8",
            body: include_bytes!("../frontend/settings.js"),
        },
        "/internal.css" => Asset {
            mime: "text/css; charset=utf-8",
            body: include_bytes!("../frontend/internal.css"),
        },
        "/internal.js" => Asset {
            mime: "application/javascript; charset=utf-8",
            body: include_bytes!("../frontend/internal.js"),
        },
        "/engine.js" => Asset {
            mime: "application/javascript; charset=utf-8",
            body: include_bytes!("../frontend/engine.js"),
        },
        "/assets/logo.svg" => Asset {
            mime: "image/svg+xml",
            body: include_bytes!("../frontend/assets/logo.svg"),
        },
        "/tests/integrity-harness.html" => Asset {
            mime: "text/html; charset=utf-8",
            body: include_bytes!("../frontend/tests/integrity-harness.html"),
        },
        "/tests/integrity-harness.css" => Asset {
            mime: "text/css; charset=utf-8",
            body: include_bytes!("../frontend/tests/integrity-harness.css"),
        },
        "/tests/integrity-harness.js" => Asset {
            mime: "application/javascript; charset=utf-8",
            body: include_bytes!("../frontend/tests/integrity-harness.js"),
        },
        _ => return None,
    };
    Some(asset)
}

pub fn respond(request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let path = request.uri().path();
    let Some(asset) = asset_for(path) else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Cow::Borrowed(&b"Not found"[..]))
            .expect("static 404 response is valid");
    };

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, asset.mime)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        // The custom scheme has a different origin on WebKitGTK and WebView2.
        // None of the pages need cross-origin access; keep it locked down.
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "null");
    if asset.mime.starts_with("text/html") {
        builder = builder.header(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(HTML_CSP),
        );
    }

    builder
        .body(Cow::Borrowed(asset.body))
        .expect("embedded asset response is valid")
}
