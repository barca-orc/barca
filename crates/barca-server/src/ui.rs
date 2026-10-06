//! The web UI, embedded in the binary from `ui/dist` and served at `/ui/`.
//!
//! The UI is built (`pnpm --dir ui build`) before the wheel; its files are
//! compiled into the binary, so `barca serve` needs nothing else installed. A
//! binary built without them (e.g. `cargo test` without Node) still compiles
//! and serves a short explanation at `/ui/` instead.
//!
//! Everything is relative so the UI works behind a reverse proxy mounted at any
//! prefix: the build uses relative asset URLs, the UI routes with the URL hash
//! (so the page is always `<prefix>/ui/`), and the redirects below send a
//! relative `Location`.

use axum::body::Body;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use std::borrow::Cow;

#[derive(rust_embed::Embed)]
// Relative to this crate's Cargo.toml.
#[folder = "../../ui/dist"]
#[allow_missing = true]
struct UiAssets;

fn embedded(path: &str) -> Option<Cow<'static, [u8]>> {
    UiAssets::get(path).map(|f| f.data)
}

/// `GET /` and `GET /ui` → `ui/`. Relative, so behind nginx at `/barca/` the
/// browser lands on `/barca/ui/`.
pub async fn redirect_to_ui() -> Redirect {
    Redirect::to("ui/")
}

/// `GET /ui/` — the app.
pub async fn index() -> Response {
    ui_response("", embedded)
}

/// `GET /ui/{*path}` — a built asset.
pub async fn asset(Path(path): Path<String>) -> Response {
    ui_response(&path, embedded)
}

const NOT_BUILT: &str = "This barca binary was built without its web UI.\n\n\
Build it with `pnpm --dir ui install && pnpm --dir ui build` before `maturin build` \
(release wheels include it). The HTTP API is unaffected.\n";

/// Serve `path` (relative to the UI root; empty means the app) from `lookup`.
pub(crate) fn ui_response(
    path: &str,
    lookup: impl Fn(&str) -> Option<Cow<'static, [u8]>>,
) -> Response {
    let path = if path.is_empty() { "index.html" } else { path };
    let Some(bytes) = lookup(path) else {
        if path == "index.html" {
            return (StatusCode::NOT_FOUND, NOT_BUILT).into_response();
        }
        return StatusCode::NOT_FOUND.into_response();
    };
    // Vite fingerprints everything under assets/, so those never change under
    // the same name; anything else (index.html, favicon) must revalidate.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut resp = Response::new(Body::from(bytes.into_owned()));
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    resp
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json" | "map") => "application/json",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn files() -> impl Fn(&str) -> Option<Cow<'static, [u8]>> {
        let map: HashMap<&'static str, &'static [u8]> = HashMap::from([
            ("index.html", b"<!doctype html>app".as_slice()),
            ("assets/index-abc.js", b"console.log(1)".as_slice()),
            ("favicon.svg", b"<svg/>".as_slice()),
        ]);
        move |p| map.get(p).map(|b| Cow::Borrowed(*b))
    }

    fn header(r: &Response, h: header::HeaderName) -> &str {
        r.headers().get(h).unwrap().to_str().unwrap()
    }

    #[test]
    fn the_root_serves_the_app_uncached() {
        let r = ui_response("", files());
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(header(&r, header::CONTENT_TYPE), "text/html; charset=utf-8");
        assert_eq!(header(&r, header::CACHE_CONTROL), "no-cache");
    }

    #[test]
    fn fingerprinted_assets_are_cached_forever() {
        let r = ui_response("assets/index-abc.js", files());
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            header(&r, header::CONTENT_TYPE),
            "text/javascript; charset=utf-8"
        );
        assert!(header(&r, header::CACHE_CONTROL).contains("immutable"));
    }

    #[test]
    fn other_files_revalidate() {
        let r = ui_response("favicon.svg", files());
        assert_eq!(header(&r, header::CONTENT_TYPE), "image/svg+xml");
        assert_eq!(header(&r, header::CACHE_CONTROL), "no-cache");
    }

    #[test]
    fn a_missing_asset_is_404() {
        assert_eq!(
            ui_response("assets/nope.js", files()).status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_binary_without_the_ui_explains_itself() {
        let r = ui_response("", |_| None);
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("built without its web UI"));
    }
}
